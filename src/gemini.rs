//! Gemini CLI sessions.
//!
//! A `gemini` process runs in a project directory, and its conversations live
//! under `~/.gemini/tmp/<project>/chats/`. Older CLI releases wrote one whole
//! `session-*.json` object; newer ones append JSONL records (message upserts,
//! `$patch` tool results, `$set` metadata) that have to be replayed to recover
//! the conversation. Both shapes are read here.
//!
//! The record also carries the one signal the terminal cannot show from the
//! process tree: a tool call's `status` is `awaiting_approval` while Gemini CLI
//! is asking the user to confirm it.

use crate::agent_sessions::{
    bind_sessions, canonical_key, clean_text, content_text, created_or_modified, modified_or_epoch,
    read_small_file, text_ends_with_question, AgentFacts, Candidate, LiveSession,
};
use crate::model::{AgentState, ContextUsage};
use crate::timestamps::parse_rfc3339_millis;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

/// A fixed one-megaton-class window covers every current Gemini model; the
/// 1.5 Pro generation was the one exception with a 2M window.
const DEFAULT_CONTEXT_WINDOW: u64 = 1_048_576;
const GEMINI_15_PRO_WINDOW: u64 = 2_097_152;

/// How long a session may stay "working" without a write before its state is
/// left to the detector's process-activity fallback.
///
/// Gemini CLI writes a message only when a tool call changes state or a model
/// response finishes. A turn interrupted mid-tool can therefore leave an
/// `executing`/terminal tool call as the last record forever, which used to
/// keep an idle process reporting "working" until it exited. A long tool or
/// model call still has a process-tree child (or writes again), so the fallback
/// keeps those working; five minutes is long enough not to mistake a slow
/// model response for an idle one.
const IDLE_WORKING_LIMIT: Duration = Duration::from_secs(300);

#[derive(Default)]
pub struct GeminiAnalyzer {
    cache: HashMap<PathBuf, Cached>,
}

struct Cached {
    modified: Option<SystemTime>,
    length: u64,
    facts: AgentFacts,
    last_access: Instant,
}

impl GeminiAnalyzer {
    /// Resolve one session per live Gemini process in `cwd`.
    pub fn analyze(&mut self, cwd: &Path, live: &[LiveSession]) -> Vec<Option<AgentFacts>> {
        if live.is_empty() {
            return Vec::new();
        }
        let Some(root) = gemini_root() else {
            return live.iter().map(|_| None).collect();
        };
        self.analyze_in(&root, cwd, live)
    }

    pub(crate) fn analyze_in(
        &mut self,
        root: &Path,
        cwd: &Path,
        live: &[LiveSession],
    ) -> Vec<Option<AgentFacts>> {
        self.analyze_root(root, cwd, live)
            .unwrap_or_else(|| live.iter().map(|_| None).collect())
    }

    fn analyze_root(
        &mut self,
        root: &Path,
        cwd: &Path,
        live: &[LiveSession],
    ) -> Option<Vec<Option<AgentFacts>>> {
        let project = project_dir(root, cwd)?;
        let candidates = session_candidates(&project.join("chats"));
        if candidates.is_empty() {
            return Some(live.iter().map(|_| None).collect());
        }
        Some(
            bind_sessions(live, &candidates)
                .into_iter()
                .map(|claim| claim.and_then(|index| self.read_facts(&candidates[index].path)))
                .collect(),
        )
    }

    fn read_facts(&mut self, session: &Path) -> Option<AgentFacts> {
        let metadata = session.metadata().ok()?;
        let modified = metadata.modified().ok();
        if let Some(cached) = self.cache.get_mut(session) {
            cached.last_access = Instant::now();
            if cached.modified == modified && cached.length == metadata.len() {
                let mut facts = cached.facts.clone();
                apply_idle_fallback(&mut facts, cached.modified);
                return Some(facts);
            }
        }
        let mut facts = facts_from_session(&load_session(session)?);
        self.cache.insert(
            session.to_path_buf(),
            Cached {
                modified,
                length: metadata.len(),
                facts: facts.clone(),
                last_access: Instant::now(),
            },
        );
        self.cache.retain(|path, cached| {
            path.is_file() && cached.last_access.elapsed() < Duration::from_secs(86_400)
        });
        apply_idle_fallback(&mut facts, modified);
        Some(facts)
    }
}

fn gemini_root() -> Option<PathBuf> {
    let home = std::env::var_os("GEMINI_CLI_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)?;
    Some(home.join(".gemini/tmp"))
}

/// The project directory Gemini assigned to `cwd`.
///
/// The CLI keeps a `projects.json` registry of path → slug and also writes a
/// `.project_root` marker inside every project directory. The registry is the
/// primary lookup; the marker covers a registry that has been deleted while the
/// session trees survive.
fn project_dir(root: &Path, cwd: &Path) -> Option<PathBuf> {
    let key = canonical_key(cwd);
    if let Some(value) = read_json(&root.parent()?.join("projects.json")) {
        if let Some(projects) = value["projects"].as_object() {
            for (path, slug) in projects {
                let Some(slug) = slug.as_str() else { continue };
                if canonical_key(Path::new(path)) != key {
                    continue;
                }
                let directory = root.join(slug);
                if directory.is_dir() {
                    return Some(directory);
                }
            }
        }
    }
    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let directory = entry.path();
        if directory.join("chats").is_dir()
            && read_small_file(&directory.join(".project_root"))
                .is_some_and(|marker| canonical_key(Path::new(marker.trim())) == key)
        {
            return Some(directory);
        }
    }
    None
}

fn session_candidates(chats: &Path) -> Vec<Candidate> {
    let Ok(entries) = fs::read_dir(chats) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            if !name.starts_with("session-") {
                return None;
            }
            if !matches!(path.extension()?.to_str()?, "json" | "jsonl") {
                return None;
            }
            let metadata = path.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            Some(Candidate {
                created: created_or_modified(&path),
                modified: modified_or_epoch(&path),
                path,
            })
        })
        .collect()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// A replayed conversation. `messages` is what later records have not removed.
#[derive(Clone, Debug, Default)]
pub(crate) struct Session {
    messages: Vec<Value>,
    summary: Option<String>,
}

/// Read a session file in either the whole-object or the append-only shape.
pub(crate) fn load_session(path: &Path) -> Option<Session> {
    parse_session_text(&String::from_utf8_lossy(&fs::read(path).ok()?))
}

/// Parse session text in either shape. Kept separate from the file read so the
/// replay rules can be tested without a scratch directory.
fn parse_session_text(text: &str) -> Option<Session> {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        if let Some(messages) = value["messages"].as_array() {
            return Some(Session {
                messages: messages.clone(),
                summary: value["summary"].as_str().and_then(clean_text),
            });
        }
    }
    replay(text)
}

/// Replay the append-only JSONL format. Newer Gemini CLI releases write one
/// record per line: a message record upserts by `id`, `$patch` fills in tool
/// results, `$set` rewrites metadata (and legacy checkpoints), and `$rewindTo`
/// drops the message it names and everything after it.
fn replay(text: &str) -> Option<Session> {
    let mut session = Session::default();
    let mut seen = false;
    for line in text.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        seen = true;
        if let Some(target) = record["$rewindTo"].as_str() {
            match session
                .messages
                .iter()
                .position(|message| message["id"] == target)
            {
                Some(index) => session.messages.truncate(index),
                None => session.messages.clear(),
            }
        } else if let Some(patch) = record.get("$patch") {
            apply_patch(&mut session.messages, patch);
        } else if let Some(set) = record.get("$set") {
            if let Some(messages) = set["messages"].as_array() {
                session.messages = messages.clone();
            }
            if let Some(summary) = set["summary"].as_str().and_then(clean_text) {
                session.summary = Some(summary);
            }
        } else if record["sessionId"].is_string() && record["projectHash"].is_string() {
            // Partial metadata: the file header that carries the title when a
            // session has one.
            if let Some(summary) = record["summary"].as_str().and_then(clean_text) {
                session.summary = Some(summary);
            }
            if let Some(messages) = record["messages"].as_array() {
                for message in messages {
                    upsert(&mut session.messages, message.clone());
                }
            }
        } else if record["id"].is_string() {
            upsert(&mut session.messages, record);
        }
    }
    (seen && (!session.messages.is_empty() || session.summary.is_some())).then_some(session)
}

fn upsert(messages: &mut Vec<Value>, message: Value) {
    if let Some(id) = message["id"].as_str() {
        if let Some(existing) = messages.iter_mut().find(|existing| existing["id"] == id) {
            *existing = message;
            return;
        }
    }
    messages.push(message);
}

fn apply_patch(messages: &mut Vec<Value>, patch: &Value) {
    if patch["id"].is_string() {
        apply_single_patch(messages, patch);
    }
    if let Some(updates) = patch["updates"].as_array() {
        for update in updates {
            apply_single_patch(messages, update);
        }
    }
    if let Some(remove) = patch["removeIds"].as_array() {
        for id in remove.iter().filter_map(Value::as_str) {
            messages.retain(|message| message["id"] != id);
        }
    }
    if let Some(order) = patch["orderIds"].as_array() {
        let ids: Vec<&str> = order.iter().filter_map(Value::as_str).collect();
        let (ordered, prefix): (Vec<Value>, Vec<Value>) = messages
            .drain(..)
            .partition(|message| ids.contains(&message["id"].as_str().unwrap_or("")));
        messages.extend(prefix);
        messages.extend(ordered);
    }
}

fn apply_single_patch(messages: &mut [Value], patch: &Value) {
    let Some(id) = patch["id"].as_str() else {
        return;
    };
    let Some(message) = messages.iter_mut().find(|message| message["id"] == id) else {
        return;
    };
    if let Some(content) = patch.get("content") {
        message["content"] = content.clone();
    }
    let Some(tool_patches) = patch["toolCalls"].as_array() else {
        return;
    };
    let Some(tool_calls) = message.get_mut("toolCalls").and_then(Value::as_array_mut) else {
        return;
    };
    for patch in tool_patches {
        let Some(patch_id) = patch["id"].as_str() else {
            continue;
        };
        if let Some(call) = tool_calls.iter_mut().find(|call| call["id"] == patch_id) {
            if let Some(result) = patch.get("result") {
                call["result"] = result.clone();
            }
        }
    }
}

/// Turn a long-idle "working" record back into "let the process tree decide".
/// Waiting, waiting-for-reply, ready and error are explicit session facts and
/// must survive; only the open-ended working state is a guess that can go stale.
fn apply_idle_fallback(facts: &mut AgentFacts, modified: Option<SystemTime>) {
    if facts.state != Some(AgentState::Working) {
        return;
    }
    let idle = modified
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > IDLE_WORKING_LIMIT);
    if idle {
        facts.state = None;
    }
}

pub(crate) fn facts_from_session(session: &Session) -> AgentFacts {
    let mut facts = AgentFacts::default();
    if let Some(state) = state_from_messages(&session.messages) {
        facts.state = Some(state);
    }
    if let Some(gemini) = session
        .messages
        .iter()
        .rev()
        .find(|message| message["type"] == "gemini" && message["model"].is_string())
    {
        facts.model = gemini["model"].as_str().and_then(clean_text);
    }
    if let Some(gemini) = session
        .messages
        .iter()
        .rev()
        .find(|message| message["type"] == "gemini" && message["tokens"].is_object())
    {
        let tokens = &gemini["tokens"];
        let used = tokens["total"]
            .as_u64()
            .filter(|total| *total > 0)
            .unwrap_or_else(|| token_sum(tokens));
        if used > 0 {
            let window_tokens = gemini["model"]
                .as_str()
                .or(facts.model.as_deref())
                .map(context_window)
                .unwrap_or(DEFAULT_CONTEXT_WINDOW);
            facts.context = Some(ContextUsage {
                used_tokens: used,
                window_tokens,
            });
        }
    }
    facts.title = session
        .summary
        .clone()
        .or_else(|| first_user_title(&session.messages));
    facts
}

fn first_user_title(messages: &[Value]) -> Option<String> {
    messages
        .iter()
        .filter(|message| message["type"] == "user")
        .filter_map(|message| content_text(&message["content"]))
        .map(|text| text.trim().to_owned())
        // Slash commands and injected context are not what the conversation is
        // about; wait for the first real prompt.
        .find(|text| {
            !text.is_empty()
                && !text.starts_with('/')
                && !text.starts_with('<')
                && !is_injected_prompt(text)
        })
        .and_then(|text| clean_text(&text))
}

/// `/init` records the CLI's expanded instruction prompt as a user message.
/// It is not what the conversation is about, so it must not become the title.
fn is_injected_prompt(text: &str) -> bool {
    let text = text.trim_start();
    text.starts_with(
        "You are an AI agent that brings the power of Gemini directly into the terminal",
    ) || text.starts_with("This is the Gemini CLI")
        || text.starts_with("You are Gemini")
}

fn state_from_messages(messages: &[Value]) -> Option<AgentState> {
    for message in messages.iter().rev() {
        match message["type"].as_str() {
            // Informational records never describe a turn; keep looking at the
            // last event that does.
            Some("info" | "warning") => continue,
            Some("user") => return Some(AgentState::Working),
            Some("error") => return Some(AgentState::Error),
            Some("gemini") => return Some(gemini_state(message)),
            _ => continue,
        }
    }
    None
}

fn gemini_state(message: &Value) -> AgentState {
    let Some(calls) = message["toolCalls"].as_array() else {
        return finished_state(message);
    };
    if calls.is_empty() {
        return finished_state(message);
    }
    let mut awaiting = false;
    let mut pending = false;
    let mut cancelled = 0;
    for call in calls {
        match tool_state(call) {
            ToolState::Awaiting => awaiting = true,
            ToolState::Pending => pending = true,
            ToolState::Terminal => {
                if call["status"] == "cancelled" {
                    cancelled += 1;
                }
            }
        }
    }
    if awaiting {
        return AgentState::Waiting;
    }
    if pending {
        return AgentState::Working;
    }
    // Every tool finished, so the loop is about to ask the model for the next
    // step. That is still part of the turn: only a later `gemini` message with
    // no tool calls closes it. An interrupted tool call is the exception — the
    // loop stops there and the session returns to the prompt.
    if cancelled == calls.len() {
        AgentState::Ready
    } else {
        AgentState::Working
    }
}

fn finished_state(message: &Value) -> AgentState {
    match content_text(&message["content"]) {
        Some(text) if text_ends_with_question(&text) => AgentState::WaitingReply,
        _ => AgentState::Ready,
    }
}

enum ToolState {
    Awaiting,
    Pending,
    Terminal,
}

/// A tool call is finished from the status the CLI recorded, with the result as
/// the fallback for records written before the status field existed.
fn tool_state(call: &Value) -> ToolState {
    match call["status"].as_str() {
        Some("awaiting_approval") => ToolState::Awaiting,
        Some("validating" | "scheduled" | "executing") => ToolState::Pending,
        Some("success" | "error" | "cancelled") => ToolState::Terminal,
        _ if call.get("result").is_some_and(|result| !result.is_null()) => ToolState::Terminal,
        _ => ToolState::Pending,
    }
}

fn token_sum(tokens: &Value) -> u64 {
    ["input", "output", "thoughts", "tool"]
        .into_iter()
        .map(|key| tokens[key].as_u64().unwrap_or(0))
        .sum()
}

fn context_window(model: &str) -> u64 {
    if model.to_ascii_lowercase().contains("1.5-pro") {
        GEMINI_15_PRO_WINDOW
    } else {
        DEFAULT_CONTEXT_WINDOW
    }
}

/// One assistant message's token counts, in the tray table's shared shape.
pub(crate) struct MessageUsage {
    pub at: i64,
    pub input: u64,
    pub input_cached: u64,
    pub output: u64,
}

/// Every billed message in a session file, in the order the records replayed.
/// `input` counts the tool-use prompt too, and `output` counts thinking, so the
/// columns mean the same thing they do for the other agents.
pub(crate) fn usage_records(path: &Path) -> Vec<MessageUsage> {
    let Some(session) = load_session(path) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    session
        .messages
        .iter()
        .filter(|message| message["type"] == "gemini")
        .filter_map(|message| {
            if let Some(id) = message["id"].as_str() {
                if !seen.insert(id.to_owned()) {
                    return None;
                }
            }
            let tokens = message.get("tokens")?.as_object()?;
            let at = message["timestamp"]
                .as_str()
                .and_then(parse_rfc3339_millis)?;
            Some(MessageUsage {
                at,
                input: tokens.get("input").and_then(Value::as_u64).unwrap_or(0)
                    + tokens.get("tool").and_then(Value::as_u64).unwrap_or(0),
                input_cached: tokens.get("cached").and_then(Value::as_u64).unwrap_or(0),
                output: tokens.get("output").and_then(Value::as_u64).unwrap_or(0)
                    + tokens.get("thoughts").and_then(Value::as_u64).unwrap_or(0),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_sessions::unix_seconds;
    use serde_json::json;
    use std::time::Duration;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn live(started: u64) -> LiveSession {
        LiveSession {
            started: at(started),
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "asi-gemini-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).expect("parent");
        fs::write(path, text).expect("write");
    }

    fn session(messages: Value) -> String {
        json!({
            "sessionId": "s1",
            "projectHash": "hash",
            "startTime": "2026-05-23T13:23:51.979Z",
            "lastUpdated": "2026-05-23T13:24:20.988Z",
            "messages": messages,
            "kind": "main"
        })
        .to_string()
    }

    fn gemini(content: Value, tool_calls: Value, tokens: Value, model: &str) -> Value {
        json!({
            "id": format!("g-{}", unix_seconds(SystemTime::now())),
            "timestamp": "2026-05-23T13:24:20.988Z",
            "type": "gemini",
            "content": content,
            "toolCalls": tool_calls,
            "tokens": tokens,
            "model": model
        })
    }

    fn fact_session(value: &str) -> AgentFacts {
        facts_from_session(&parse_session_text(value).expect("session"))
    }

    #[test]
    fn an_idle_gemini_process_binds_to_its_own_session() {
        let dir = scratch("bind");
        let root = dir.join("tmp");
        write(
            &dir.join("projects.json"),
            &json!({ "projects": { "/work/app": "app" } }).to_string(),
        );
        let chats = root.join("app/chats");
        for name in ["session-old", "session-new"] {
            write(
                &chats.join(format!("{name}.json")),
                &session(json!([{ "id": "u", "type": "user", "content": "hi" }])),
            );
        }
        let mut analyzer = GeminiAnalyzer::default();
        let live = [live(unix_seconds(modified_or_epoch(
            &chats.join("session-new.json"),
        )))];
        let facts = analyzer.analyze_in(&root, Path::new("/work/app"), &live);
        assert_eq!(facts.len(), 1);
        assert!(facts[0].is_some());
    }

    #[test]
    fn a_user_message_and_its_tool_call_report_working() {
        let text = session(json!([
            { "id": "u1", "timestamp": "2026-05-23T13:23:51.979Z", "type": "user", "content": [{"text": "list files"}] },
            {
                "id": "g1", "timestamp": "2026-05-23T13:23:55.000Z", "type": "gemini",
                "content": "",
                "toolCalls": [{ "id": "c1", "name": "list_directory", "status": "executing", "args": {} }]
            }
        ]));
        assert_eq!(fact_session(&text).state, Some(AgentState::Working));
    }

    #[test]
    fn an_awaiting_approval_tool_waits_for_confirmation() {
        let text = session(json!([
            {
                "id": "g1", "timestamp": "2026-05-23T13:23:55.000Z", "type": "gemini",
                "content": "",
                "toolCalls": [{ "id": "c1", "name": "run_shell_command", "status": "awaiting_approval", "args": {} }]
            }
        ]));
        assert_eq!(fact_session(&text).state, Some(AgentState::Waiting));
    }

    #[test]
    fn a_finished_turn_reports_ready_or_waiting_for_an_answer() {
        let ready = session(json!([
            { "id": "u1", "type": "user", "content": "hi" },
            gemini(json!("How can I help?"), json!([]), json!({"input": 10, "output": 3, "thoughts": 1, "total": 14}), "gemini-3.1-pro-preview")
        ]));
        let facts = fact_session(&ready);
        assert_eq!(facts.state, Some(AgentState::WaitingReply));
        assert_eq!(facts.model.as_deref(), Some("gemini-3.1-pro-preview"));
        assert_eq!(
            facts.context,
            Some(ContextUsage {
                used_tokens: 14,
                window_tokens: DEFAULT_CONTEXT_WINDOW
            })
        );

        let done = session(json!([
            { "id": "u1", "type": "user", "content": "hi" },
            gemini(json!("Done."), json!([]), json!({"input": 10, "output": 3, "total": 13}), "gemini-2.5-flash")
        ]));
        assert_eq!(fact_session(&done).state, Some(AgentState::Ready));
    }

    #[test]
    fn a_tool_result_keeps_working_until_the_model_answers() {
        // The tool is finished, but the loop has not produced the final answer
        // yet; reporting Ready here would look like a completed turn. A model
        // preamble before the call does not change that.
        let text = session(json!([
            {
                "id": "g1", "type": "gemini", "content": "Let me read that file.",
                "toolCalls": [{ "id": "c1", "status": "success", "result": [{"text": "ok"}] }]
            }
        ]));
        assert_eq!(fact_session(&text).state, Some(AgentState::Working));

        let cancelled = session(json!([
            {
                "id": "g1", "type": "gemini", "content": "",
                "toolCalls": [{ "id": "c1", "status": "cancelled" }]
            }
        ]));
        assert_eq!(fact_session(&cancelled).state, Some(AgentState::Ready));
    }

    #[test]
    fn an_error_record_is_the_error_state() {
        let text = session(json!([
            { "id": "u1", "type": "user", "content": "hi" },
            { "id": "e1", "type": "error", "content": "[API Error: quota exhausted]" }
        ]));
        assert_eq!(fact_session(&text).state, Some(AgentState::Error));
        // A quiet info line after the error does not erase it.
        let text = session(json!([
            { "id": "e1", "type": "error", "content": "[API Error]" },
            { "id": "i1", "type": "info", "content": "This request failed." }
        ]));
        assert_eq!(fact_session(&text).state, Some(AgentState::Error));
    }

    #[test]
    fn the_append_only_format_replays_patches_and_metadata() {
        let lines = [
            json!({
                "sessionId": "s1", "projectHash": "hash",
                "startTime": "2026-05-23T13:23:51.979Z",
                "lastUpdated": "2026-05-23T13:24:20.988Z",
                "summary": "Fix the parser"
            }),
            json!({
                "id": "u1", "timestamp": "2026-05-23T13:23:51.979Z", "type": "user",
                "content": [{"text": "fix the parser"}]
            }),
            json!({
                "id": "g1", "timestamp": "2026-05-23T13:23:55.000Z", "type": "gemini",
                "content": "",
                "toolCalls": [{"id": "c1", "name": "read_file", "status": "executing", "args": {}}],
                "model": "gemini-3.1-pro-preview"
            }),
            // The tool result arrives as a patch to the message, not a new one.
            json!({
                "$patch": {
                    "updates": [{ "id": "g1", "toolCalls": [{ "id": "c1", "result": [{"text": "ok"}] }] }]
                }
            }),
            json!({
                "id": "g2", "timestamp": "2026-05-23T13:24:20.988Z", "type": "gemini",
                "content": "The parser is fixed.",
                "toolCalls": [],
                "model": "gemini-3.1-pro-preview",
                "tokens": {"input": 100, "cached": 80, "output": 10, "thoughts": 2, "tool": 3, "total": 115}
            }),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        let session = parse_session_text(&lines).expect("session");
        let facts = facts_from_session(&session);
        assert_eq!(session.messages.len(), 3);
        assert_eq!(
            session.messages[1]["toolCalls"][0]["result"][0]["text"], "ok",
            "the patch filled in the result"
        );
        assert_eq!(facts.state, Some(AgentState::Ready));
        assert_eq!(facts.title.as_deref(), Some("Fix the parser"));
        assert_eq!(
            facts.context,
            Some(ContextUsage {
                used_tokens: 115,
                window_tokens: DEFAULT_CONTEXT_WINDOW
            })
        );
    }

    #[test]
    fn the_first_real_prompt_becomes_the_title() {
        let text = session(json!([
            { "id": "u0", "type": "user", "content": "/init" },
            { "id": "u0b", "type": "user", "content": "\nYou are an AI agent that brings the power of Gemini directly into the terminal. Your task is to analyze the directory." },
            { "id": "u1", "type": "user", "content": "<session_context>files</session_context>" },
            { "id": "u2", "type": "user", "content": [{"text": "  implement the\n  importer "}] },
            { "id": "g1", "type": "gemini", "content": "ok" }
        ]));
        assert_eq!(
            fact_session(&text).title.as_deref(),
            Some("implement the importer")
        );
    }

    #[test]
    fn an_idle_working_record_falls_back_to_process_activity() {
        let mut working = AgentFacts {
            state: Some(AgentState::Working),
            ..AgentFacts::default()
        };
        let old = SystemTime::now()
            .checked_sub(Duration::from_secs(600))
            .expect("past");
        apply_idle_fallback(&mut working, Some(old));
        assert_eq!(working.state, None);

        let mut recent = AgentFacts {
            state: Some(AgentState::Working),
            ..AgentFacts::default()
        };
        apply_idle_fallback(&mut recent, Some(SystemTime::now()));
        assert_eq!(recent.state, Some(AgentState::Working));

        // A finished/error/waiting fact is not a guess and must survive.
        for state in [
            AgentState::Ready,
            AgentState::Waiting,
            AgentState::WaitingReply,
            AgentState::Error,
        ] {
            let mut facts = AgentFacts {
                state: Some(state),
                ..AgentFacts::default()
            };
            apply_idle_fallback(&mut facts, Some(old));
            assert_eq!(facts.state, Some(state));
        }
    }

    #[test]
    fn usage_records_count_tools_and_thinking_in_the_shared_shape() {
        let dir = scratch("usage");
        let path = dir.join("session.json");
        write(
            &path,
            &session(json!([
                { "id": "u1", "type": "user", "content": "hi" },
                {
                    "id": "g1", "timestamp": "2026-05-23T13:24:20.988Z", "type": "gemini",
                    "content": "ok",
                    "tokens": {"input": 100, "cached": 80, "output": 10, "thoughts": 2, "tool": 3, "total": 115}
                }
            ])),
        );
        let records = usage_records(&path);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].input, 103);
        assert_eq!(records[0].input_cached, 80);
        assert_eq!(records[0].output, 12);
        assert_eq!(records[0].at, 1_779_542_660_988);
    }
}
