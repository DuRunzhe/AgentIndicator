use crate::model::{AgentState, ContextUsage};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Debug, Default)]
pub struct SessionFacts {
    pub state: Option<AgentState>,
    pub model: Option<String>,
    pub context: Option<ContextUsage>,
    pub cwd: Option<PathBuf>,
    pub requires_terminal_probe: bool,
    /// Rollout version carried into asynchronous terminal probing. A result for
    /// an older version is ignored after a resume or newly appended event.
    pub activity: Option<SystemTime>,
    pub automatic_confirmation_mode: bool,
    approval_policy: Option<String>,
    approvals_reviewer: Option<String>,
}

struct FileCursor {
    offset: u64,
    length: u64,
    facts: SessionFacts,
    tools: HashMap<String, PendingTool>,
    results: HashSet<String>,
    last_access: Instant,
}

impl Default for FileCursor {
    fn default() -> Self {
        Self {
            offset: 0,
            length: 0,
            facts: SessionFacts::default(),
            tools: HashMap::new(),
            results: HashSet::new(),
            last_access: Instant::now(),
        }
    }
}

#[derive(Clone)]
struct PendingTool {
    user_input: bool,
    approval: bool,
}

#[derive(Default)]
pub struct SessionAnalyzer {
    files: HashMap<PathBuf, FileCursor>,
    codex_rollouts: Vec<(PathBuf, PathBuf)>,
    codex_indexed_at: Option<Instant>,
    last_cache_prune: Option<Instant>,
}

impl SessionAnalyzer {
    #[cfg(not(target_os = "macos"))]
    pub fn analyze_codex(
        &mut self,
        cwd: &Path,
        resumed_session_id: Option<&str>,
    ) -> Option<SessionFacts> {
        let stale = self
            .codex_indexed_at
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(30));
        if stale {
            self.refresh_codex_index();
        }
        if let Some(session_id) = resumed_session_id {
            if let Some((path, rollout_cwd)) = self
                .codex_rollouts
                .iter()
                .find(|(path, _)| rollout_has_session_id(path, session_id))
                .map(|(path, cwd)| (path.clone(), cwd.clone()))
            {
                return self.analyze_jsonl(&path, "codex").map(|mut facts| {
                    facts.cwd = Some(rollout_cwd);
                    facts
                });
            }
            // A just-created session file can appear between the 30s index refreshes.
            self.refresh_codex_index();
            if let Some((path, rollout_cwd)) = self
                .codex_rollouts
                .iter()
                .find(|(path, _)| rollout_has_session_id(path, session_id))
                .map(|(path, cwd)| (path.clone(), cwd.clone()))
            {
                return self.analyze_jsonl(&path, "codex").map(|mut facts| {
                    facts.cwd = Some(rollout_cwd);
                    facts
                });
            }
        }
        self.analyze_codex_for_cwd(cwd)
    }

    pub fn analyze_codex_for_cwd(&mut self, cwd: &Path) -> Option<SessionFacts> {
        let stale = self
            .codex_indexed_at
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(30));
        if stale {
            self.refresh_codex_index();
        }
        let path = self
            .codex_rollouts
            .iter()
            .filter(|(_, rollout_cwd)| rollout_cwd == cwd)
            .filter_map(|(path, _)| Some((path.metadata().ok()?.modified().ok()?, path.clone())))
            .max_by_key(|(modified, _)| *modified)
            .map(|(_, path)| path)?;
        self.analyze_jsonl(&path, "codex")
    }

    pub fn analyze_codex_rollout(&mut self, path: &Path) -> Option<SessionFacts> {
        self.analyze_jsonl(path, "codex").map(|mut facts| {
            facts.cwd = primary_codex_rollout_cwd(path);
            facts
        })
    }

    fn refresh_codex_index(&mut self) {
        self.codex_rollouts.clear();
        if let Some(root) = dirs::home_dir().map(|p| p.join(".codex/sessions")) {
            collect_rollouts(&root, &mut self.codex_rollouts);
        }
        self.codex_indexed_at = Some(Instant::now());
    }

    pub fn analyze_jsonl(&mut self, path: &Path, agent: &str) -> Option<SessionFacts> {
        self.prune_files();
        let metadata = path.metadata().ok()?;
        let length = metadata.len();
        let activity = metadata.modified().ok();
        let cursor = self.files.entry(path.to_owned()).or_default();
        cursor.last_access = Instant::now();
        cursor.facts.activity = activity;
        if length < cursor.length {
            *cursor = FileCursor::default();
        }
        if length == cursor.length {
            return Some(cursor.facts.clone());
        }
        let mut file = File::open(path).ok()?;
        file.seek(SeekFrom::Start(cursor.offset)).ok()?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = reader.read_line(&mut line).ok()?;
            if bytes == 0 {
                break;
            }
            // Do not consume an incomplete final JSONL record.
            if !line.ends_with('\n') {
                break;
            }
            if let Ok(event) = serde_json::from_str::<Value>(&line) {
                apply_event(&event, agent, cursor);
                cursor.offset += bytes as u64;
            } else {
                cursor.offset += bytes as u64;
            }
        }
        cursor.length = cursor.offset;
        apply_pending_priority(cursor);
        Some(cursor.facts.clone())
    }

    fn prune_files(&mut self) {
        if self
            .last_cache_prune
            .is_some_and(|at| at.elapsed() < Duration::from_secs(60))
        {
            return;
        }
        self.last_cache_prune = Some(Instant::now());
        self.files.retain(|path, cursor| {
            path.is_file() && cursor.last_access.elapsed() < Duration::from_secs(86_400)
        });
        while self.files.len() > 200 {
            let oldest = self
                .files
                .iter()
                .min_by_key(|(_, cursor)| cursor.last_access)
                .map(|(path, _)| path.clone());
            if let Some(path) = oldest {
                self.files.remove(&path);
            } else {
                break;
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn rollout_has_session_id(path: &Path, session_id: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.starts_with("rollout-")
                && name.ends_with(".jsonl")
                && name
                    .strip_suffix(".jsonl")
                    .is_some_and(|base| base.ends_with(session_id))
        })
}

fn collect_rollouts(directory: &Path, output: &mut Vec<(PathBuf, PathBuf)>) {
    let Ok(entries) = directory.read_dir() else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rollouts(&path, output);
        } else if path
            .file_name()
            .and_then(|v| v.to_str())
            .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
        {
            if let Some(cwd) = primary_codex_rollout_cwd(&path) {
                output.push((path, cwd));
            }
        }
    }
}

/// The thread id of a rollout, taken from its file name
/// (`rollout-<timestamp>-<uuid>.jsonl`). Codex uses this id both for
/// `codex resume <id>` and for the app's `codex://threads/<id>` deep link, and
/// it matches `session_meta`'s `session_id`.
pub fn codex_rollout_thread_id(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?.strip_suffix(".jsonl")?;
    let rest = name.strip_prefix("rollout-")?;
    // The name is `rollout-<YYYY-MM-DDTHH-MM-SS>-<uuid>`: a fixed-width
    // timestamp, then `-`, then the id.
    let (timestamp, id) = rest.split_at_checked(19)?;
    if timestamp.as_bytes().get(10) != Some(&b'T') {
        return None;
    }
    let id = id.strip_prefix('-')?;
    (!id.is_empty()).then_some(id)
}

/// Thread sources Codex uses for work it runs on its own behalf. Subagents and
/// the "guardian" review agent share the session directory with real
/// conversations, and reporting one produces a row the app refuses to open
/// ("already open in another app").
const INTERNAL_THREAD_SOURCES: [&str; 2] = ["subagent", "guardian_review"];

/// The session header of a rollout: which project it belongs to and whether it
/// is a conversation at all.
pub struct CodexRolloutHeader {
    pub cwd: PathBuf,
    pub user_thread: bool,
}

pub fn codex_rollout_header(path: &Path) -> Option<CodexRolloutHeader> {
    let mut reader = BufReader::new(File::open(path).ok()?);
    let mut line = String::new();
    for _ in 0..8 {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            break;
        }
        let event: Value = serde_json::from_str(&line).ok()?;
        if event["type"] != "session_meta" {
            continue;
        }
        let payload = &event["payload"];
        let cwd = payload["cwd"].as_str().or_else(|| event["cwd"].as_str())?;
        let thread_source = payload["thread_source"]
            .as_str()
            .or_else(|| event["thread_source"].as_str());
        let source = &payload["source"];
        let internal = thread_source
            .is_some_and(|source| INTERNAL_THREAD_SOURCES.contains(&source))
            || source["subagent"].is_object();
        return Some(CodexRolloutHeader {
            cwd: PathBuf::from(cwd),
            user_thread: !internal,
        });
    }
    None
}

pub fn primary_codex_rollout_cwd(path: &Path) -> Option<PathBuf> {
    let header = codex_rollout_header(path)?;
    header.user_thread.then_some(header.cwd)
}

fn apply_event(event: &Value, agent: &str, cursor: &mut FileCursor) {
    if agent == "codex"
        && event["type"] == "event_msg"
        && matches!(
            event["payload"]["type"].as_str(),
            Some("task_started" | "task_complete")
        )
    {
        cursor.tools.clear();
        cursor.results.clear();
    }
    collect_tools(event, &mut cursor.tools, &mut cursor.results);
    if agent == "claude" {
        if event["type"] == "system" && event["subtype"] == "turn_duration" {
            // Claude Code closes every completed user turn with a
            // turn_duration marker. When the turn's last assistant message
            // ended in a question, that marker must not erase the
            // awaiting-reply signal; only the user's next message clears it.
            if cursor.facts.state != Some(AgentState::WaitingReply) {
                cursor.facts.state = Some(AgentState::Ready);
            }
        } else if event["type"] == "user" {
            cursor.facts.state = Some(AgentState::Working);
        } else if event["type"] == "assistant" {
            cursor.facts.state = Some(if event["message"]["stop_reason"] == "end_turn" {
                if assistant_ends_with_question(event) {
                    AgentState::WaitingReply
                } else {
                    AgentState::Ready
                }
            } else {
                AgentState::Working
            });
            set_model(&mut cursor.facts, event["message"]["model"].as_str());
        }
    } else if agent == "codex" {
        let event_type = event["type"].as_str();
        let payload_type = event["payload"]["type"].as_str();
        match (event_type, payload_type) {
            (Some("event_msg"), Some("task_complete")) => {
                cursor.facts.state = Some(AgentState::Ready)
            }
            (Some("event_msg"), Some("task_started")) => {
                cursor.facts.state = Some(AgentState::Working)
            }
            (
                Some("response_item"),
                Some(
                    "reasoning"
                    | "message"
                    | "function_call"
                    | "function_call_output"
                    | "custom_tool_call"
                    | "custom_tool_call_output",
                ),
            ) => cursor.facts.state = Some(AgentState::Working),
            _ => {}
        }
        if event_type == Some("turn_context") {
            set_model(&mut cursor.facts, event["payload"]["model"].as_str());
            if let Some(policy) = event["payload"]["approval_policy"].as_str() {
                cursor.facts.approval_policy = Some(policy.into());
            }
            if let Some(reviewer) = event["payload"]["approvals_reviewer"].as_str() {
                cursor.facts.approvals_reviewer = Some(reviewer.into());
            }
            cursor.facts.automatic_confirmation_mode = cursor.facts.approval_policy.as_deref()
                == Some("never")
                || cursor.facts.approvals_reviewer.as_deref() == Some("auto_review");
        }
        if payload_type == Some("thread_settings_applied") {
            set_model(
                &mut cursor.facts,
                event["payload"]["thread_settings"]["model"].as_str(),
            );
        }
        if payload_type == Some("token_count") {
            let info = &event["payload"]["info"];
            let used = info["last_token_usage"]["total_tokens"]
                .as_u64()
                .or_else(|| info["total_token_usage"]["total_tokens"].as_u64());
            let window = info["model_context_window"].as_u64();
            if let (Some(used_tokens), Some(window_tokens)) = (used, window) {
                cursor.facts.context = Some(ContextUsage {
                    used_tokens,
                    window_tokens,
                });
            }
        }
    }
}

fn set_model(facts: &mut SessionFacts, model: Option<&str>) {
    if let Some(value) = model.map(str::trim).filter(|v| !v.is_empty()) {
        facts.model = Some(value.chars().take(80).collect());
    }
}

fn assistant_ends_with_question(event: &Value) -> bool {
    event["message"]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
        .last()
        .is_some_and(|text| {
            matches!(
                text.trim_end_matches(|c: char| {
                    c.is_whitespace() || "\"'”’）)]】}。.!！*_`~～".contains(c)
                })
                .chars()
                .last(),
                Some('?' | '？')
            )
        })
}

fn collect_tools(
    value: &Value,
    tools: &mut HashMap<String, PendingTool>,
    results: &mut HashSet<String>,
) {
    let kind = value["type"].as_str().unwrap_or_default();
    if matches!(kind, "tool_use" | "function_call" | "custom_tool_call") {
        let id = if matches!(kind, "function_call" | "custom_tool_call") {
            value["call_id"].as_str().or_else(|| value["id"].as_str())
        } else {
            value["id"]
                .as_str()
                .or_else(|| value["tool_use_id"].as_str())
                .or_else(|| value["call_id"].as_str())
        };
        if let Some(id) = id {
            let name = value["name"]
                .as_str()
                .or_else(|| value["tool_name"].as_str())
                .unwrap_or_default()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            let input = value
                .get("input")
                .or_else(|| value.get("arguments"))
                .unwrap_or(&Value::Null);
            tools.insert(
                id.into(),
                PendingTool {
                    user_input: matches!(name.as_str(), "requestuserinput" | "askuserquestion"),
                    approval: contains_escalation(input) || is_permission_request(&name),
                },
            );
        }
        return;
    }
    if matches!(
        kind,
        "tool_result" | "function_call_output" | "custom_tool_call_output"
    ) {
        if let Some(id) = value["tool_use_id"]
            .as_str()
            .or_else(|| value["call_id"].as_str())
            .or_else(|| value["id"].as_str())
        {
            results.insert(id.into());
        }
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                collect_tools(item, tools, results);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                collect_tools(item, tools, results);
            }
        }
        _ => {}
    }
}

/// Codex can ask the user to widen its sandbox through a dedicated
/// `request_permissions` tool (`{"permissions": {"file_system": ...,
/// "network": ...}}`). That is how the ChatGPT desktop app drives its embedded
/// Codex; the CLI instead marks a call's own arguments with
/// `sandbox_permissions: "require_escalated"` (see [`contains_escalation`]).
/// Without this, a permission request looks like an ordinary pending tool and
/// the session reports Working while it is really waiting for confirmation.
fn is_permission_request(normalized_name: &str) -> bool {
    normalized_name == "requestpermissions"
}

fn contains_escalation(value: &Value) -> bool {
    match value {
        Value::String(text) => {
            text.contains("sandbox_permissions") && text.contains("require_escalated")
        }
        Value::Array(items) => items.iter().any(contains_escalation),
        Value::Object(map) => {
            map.get("sandbox_permissions").and_then(Value::as_str) == Some("require_escalated")
                || map.values().any(contains_escalation)
        }
        _ => false,
    }
}

fn apply_pending_priority(cursor: &mut FileCursor) {
    let pending: Vec<_> = cursor
        .tools
        .iter()
        .filter(|(id, _)| !cursor.results.contains(*id))
        .map(|(_, tool)| tool)
        .collect();
    cursor.facts.requires_terminal_probe = false;
    if pending.iter().any(|tool| tool.user_input) {
        cursor.facts.state = Some(AgentState::WaitingReply);
    } else if pending.iter().any(|tool| tool.approval) {
        cursor.facts.state = Some(AgentState::Waiting);
        cursor.facts.requires_terminal_probe = true;
    } else if !pending.is_empty() {
        cursor.facts.state = Some(AgentState::Working);
        cursor.facts.requires_terminal_probe = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn claude_turn_duration_keeps_waiting_reply_after_a_question() {
        // Mirrors the tail of a real interactive Claude session: after the
        // assistant ends its turn with a question, Claude Code appends a
        // turn_duration system event. That marker must not erase the
        // awaiting-reply signal left by the question.
        let mut cursor = FileCursor::default();
        for raw in [
            r#"{"type":"user","message":{"role":"user","content":"帮我做"}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"完成了。需要我帮你清理掉它吗？"}]}}"#,
            r#"{"type":"system","subtype":"turn_duration","durationMs":100}"#,
        ] {
            apply_event(&serde_json::from_str(raw).unwrap(), "claude", &mut cursor);
        }
        assert_eq!(cursor.facts.state, Some(AgentState::WaitingReply));
    }

    #[test]
    fn claude_turn_duration_stays_ready_without_a_question() {
        let mut cursor = FileCursor::default();
        for raw in [
            r#"{"type":"user","message":{"role":"user","content":"帮我做"}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"完成了。"}]}}"#,
            r#"{"type":"system","subtype":"turn_duration","durationMs":100}"#,
        ] {
            apply_event(&serde_json::from_str(raw).unwrap(), "claude", &mut cursor);
        }
        assert_eq!(cursor.facts.state, Some(AgentState::Ready));
    }

    #[test]
    fn pairs_tools_by_id_and_prioritizes_user_input() {
        let mut cursor = FileCursor::default();
        for raw in [
            r#"{"type":"function_call","call_id":"a","name":"exec","arguments":"{}"}"#,
            r#"{"type":"function_call_output","call_id":"a"}"#,
            r#"{"type":"tool_use","id":"b","name":"request_user_input"}"#,
        ] {
            apply_event(&serde_json::from_str(raw).unwrap(), "codex", &mut cursor);
        }
        apply_pending_priority(&mut cursor);
        assert_eq!(cursor.facts.state, Some(AgentState::WaitingReply));
    }
    #[test]
    fn recognizes_nested_escalation() {
        let input: Value = serde_json::json!({"args":{"sandbox_permissions":"require_escalated"}});
        assert!(contains_escalation(&input));
    }

    #[test]
    fn chatgpt_style_permission_request_waits_for_confirmation() {
        // The ChatGPT desktop app drives Codex through the app-server, which
        // asks to widen the sandbox with a dedicated `request_permissions`
        // tool instead of the CLI's `sandbox_permissions` argument. A pending
        // such call means the session waits for the user, not that it works.
        let mut cursor = FileCursor::default();
        apply_event(
            &serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "request_permissions",
                    "call_id": "call-permissions",
                    "arguments": "{\"permissions\":{\"file_system\":{\"write\":[\"/project/.git\"]},\"network\":{\"enabled\":true}},\"reason\":\"需要执行 git fetch\"}"
                }
            }),
            "codex",
            &mut cursor,
        );
        apply_pending_priority(&mut cursor);
        assert_eq!(cursor.facts.state, Some(AgentState::Waiting));
        assert!(cursor.facts.requires_terminal_probe);
    }

    #[test]
    fn answered_permission_request_returns_to_working() {
        let mut cursor = FileCursor::default();
        for event in [
            serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "request_permissions",
                    "call_id": "call-permissions",
                    "arguments": "{\"permissions\":{\"network\":{\"enabled\":true}}}"
                }
            }),
            serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": "call-permissions",
                    "output": "approved"
                }
            }),
            serde_json::json!({
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "exec_command",
                    "call_id": "call-exec",
                    "arguments": "{\"cmd\":\"git fetch\"}"
                }
            }),
        ] {
            apply_event(&event, "codex", &mut cursor);
        }
        apply_pending_priority(&mut cursor);
        assert_eq!(cursor.facts.state, Some(AgentState::Working));
    }

    #[test]
    fn custom_tool_calls_pair_by_call_id() {
        let mut cursor = FileCursor::default();
        for raw in [
            r#"{"type":"custom_tool_call","id":"item-a","call_id":"call-a","name":"exec","input":"{\"sandbox_permissions\":\"require_escalated\"}"}"#,
            r#"{"type":"custom_tool_call_output","id":"output-a","call_id":"call-a"}"#,
        ] {
            apply_event(&serde_json::from_str(raw).unwrap(), "codex", &mut cursor);
        }
        apply_pending_priority(&mut cursor);
        assert_eq!(cursor.facts.state, None);
        assert!(!cursor.facts.requires_terminal_probe);
    }

    #[test]
    fn task_complete_clears_stale_pending_tools() {
        let mut cursor = FileCursor::default();
        let call = serde_json::json!({
            "type":"custom_tool_call", "call_id":"old-call", "name":"exec",
            "input":"{\"sandbox_permissions\":\"require_escalated\"}"
        });
        apply_event(&call, "codex", &mut cursor);
        apply_event(
            &serde_json::json!({"type":"event_msg","payload":{"type":"task_complete"}}),
            "codex",
            &mut cursor,
        );
        apply_pending_priority(&mut cursor);
        assert_eq!(cursor.facts.state, Some(AgentState::Ready));
        assert!(!cursor.facts.requires_terminal_probe);
    }

    #[test]
    fn codex_context_uses_last_turn_instead_of_cumulative_usage() {
        let mut cursor = FileCursor::default();
        apply_event(
            &serde_json::json!({
                "type":"event_msg",
                "payload":{
                    "type":"token_count",
                    "info":{
                        "total_token_usage":{"total_tokens":19_414_314},
                        "last_token_usage":{"total_tokens":194_475},
                        "model_context_window":258_400
                    }
                }
            }),
            "codex",
            &mut cursor,
        );
        let context = cursor.facts.context.expect("context usage");
        assert_eq!(context.used_tokens, 194_475);
        assert_eq!(context.window_tokens, 258_400);
    }

    #[test]
    fn codex_auto_confirm_metadata_persists_until_explicitly_changed() {
        let mut cursor = FileCursor::default();
        apply_event(
            &serde_json::json!({"type":"turn_context","payload":{"approval_policy":"never"}}),
            "codex",
            &mut cursor,
        );
        apply_event(
            &serde_json::json!({"type":"turn_context","payload":{"model":"gpt-5"}}),
            "codex",
            &mut cursor,
        );
        assert!(cursor.facts.automatic_confirmation_mode);
        apply_event(
            &serde_json::json!({"type":"turn_context","payload":{"approval_policy":"on-request"}}),
            "codex",
            &mut cursor,
        );
        assert!(!cursor.facts.automatic_confirmation_mode);
    }

    #[test]
    fn auto_confirm_session_still_reports_a_pending_escalated_tool_as_waiting() {
        let mut cursor = FileCursor::default();
        for raw in [
            r#"{"type":"turn_context","payload":{"approval_policy":"never"}}"#,
            r#"{"type":"custom_tool_call","call_id":"call-a","name":"exec","input":"{\"sandbox_permissions\":\"require_escalated\"}"}"#,
        ] {
            apply_event(&serde_json::from_str(raw).unwrap(), "codex", &mut cursor);
        }
        apply_pending_priority(&mut cursor);
        assert!(cursor.facts.automatic_confirmation_mode);
        assert_eq!(cursor.facts.state, Some(AgentState::Waiting));
    }

    #[test]
    fn extracts_the_thread_id_from_a_rollout_name() {
        assert_eq!(
            codex_rollout_thread_id(Path::new(
                "/home/u/.codex/sessions/2026/09/10/rollout-2026-09-10T17-18-30-01a08a9c-8b7b-7530-be66-8da1fee75728.jsonl"
            )),
            Some("01a08a9c-8b7b-7530-be66-8da1fee75728")
        );
        assert_eq!(
            codex_rollout_thread_id(Path::new("not-a-rollout.jsonl")),
            None
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn resumed_codex_session_matches_its_rollout_filename() {
        assert!(rollout_has_session_id(
            Path::new("rollout-2026-08-17T22-58-56-01a0103b-98d7-7581-b338-6407764039a9.jsonl"),
            "01a0103b-98d7-7581-b338-6407764039a9"
        ));
        assert!(!rollout_has_session_id(
            Path::new("rollout-2026-08-21T23-36-17-01a024f7-3c9e-7670-926d-4bd8338eeae6.jsonl"),
            "01a0103b-98d7-7581-b338-6407764039a9"
        ));
    }

    #[test]
    fn appended_events_update_a_resumed_rollout_state() {
        let path = std::env::temp_dir().join(format!(
            "agent-status-indicator-resume-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(
            &path,
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
        )
        .unwrap();
        let mut analyzer = SessionAnalyzer::default();
        assert_eq!(
            analyzer.analyze_jsonl(&path, "codex").unwrap().state,
            Some(AgentState::Working)
        );
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            file,
            "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\"}}}}"
        )
        .unwrap();
        assert_eq!(
            analyzer.analyze_jsonl(&path, "codex").unwrap().state,
            Some(AgentState::Ready)
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn primary_rollout_filter_rejects_subagent_header() {
        let path = std::env::temp_dir().join(format!(
            "agent-status-indicator-subagent-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(
            &path,
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/project\",\"thread_source\":\"subagent\"}}\n",
        )
        .unwrap();
        assert_eq!(primary_codex_rollout_cwd(&path), None);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn guardian_review_threads_are_not_conversations() {
        // The guardian review agent shares the session directory with real
        // conversations; listing it produced a row the app rejected with
        // "already open in another app".
        let path = std::env::temp_dir().join(format!(
            "agent-status-indicator-guardian-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(
            &path,
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/project\",\"thread_source\":\"guardian_review\",\"source\":{\"subagent\":{\"other\":\"guardian\"}}}}\n",
        )
        .unwrap();
        let header = codex_rollout_header(&path).expect("header");
        assert!(!header.user_thread);
        assert_eq!(primary_codex_rollout_cwd(&path), None);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn user_threads_keep_their_project() {
        let path = std::env::temp_dir().join(format!(
            "agent-status-indicator-user-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(
            &path,
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/project\",\"thread_source\":\"user\",\"source\":\"vscode\"}}\n",
        )
        .unwrap();
        let header = codex_rollout_header(&path).expect("header");
        assert!(header.user_thread);
        assert_eq!(
            primary_codex_rollout_cwd(&path),
            Some(PathBuf::from("/project"))
        );
        let _ = std::fs::remove_file(path);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn resumed_session_uses_thread_id_when_process_cwd_is_missing() {
        let session_id = "01a0103b-98d7-7581-b338-6407764039a9";
        let path =
            std::env::temp_dir().join(format!("rollout-2026-08-23T12-00-00-{session_id}.jsonl"));
        std::fs::write(
            &path,
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
        )
        .unwrap();
        let cwd = PathBuf::from("/project/resumed");
        let mut analyzer = SessionAnalyzer {
            codex_rollouts: vec![(path.clone(), cwd.clone())],
            codex_indexed_at: Some(Instant::now()),
            ..Default::default()
        };
        let facts = analyzer
            .analyze_codex(Path::new(""), Some(session_id))
            .unwrap();
        assert_eq!(facts.state, Some(AgentState::Working));
        assert_eq!(facts.cwd, Some(cwd));
        let _ = std::fs::remove_file(path);
    }
}
