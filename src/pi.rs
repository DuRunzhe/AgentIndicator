use crate::model::{AgentState, ContextUsage};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

const TAIL_BYTES: usize = 2 * 1024 * 1024;
const TAIL_LINES: usize = 1000;

/// A pi session file is created right when its pi process starts a new
/// session. Two live pi processes sharing a directory therefore own two
/// distinct files, and only files whose creation time matches a process start
/// within this window are treated as that process's fresh session.
const ANCHOR_WINDOW_SECONDS: u64 = 90;

#[derive(Clone, Debug, Default)]
pub struct PiFacts {
    pub state: Option<AgentState>,
    pub model: Option<String>,
    pub context: Option<ContextUsage>,
}

/// One live pi process that the detector found in a directory.
pub struct LiveSession {
    pub started: SystemTime,
}

struct Cached {
    session_modified: SystemTime,
    session_size: u64,
    models_modified: Option<SystemTime>,
    store_modified: Option<SystemTime>,
    facts: PiFacts,
    last_access: Instant,
}

struct Candidate {
    path: PathBuf,
    created: SystemTime,
    modified: SystemTime,
}

#[derive(Default)]
pub struct PiAnalyzer {
    cache: HashMap<PathBuf, Cached>,
}

impl PiAnalyzer {
    /// Resolve one session file per live pi process that shares `cwd` and
    /// return the parsed facts in the same order as `live`.
    ///
    /// A pi session is bound to the process that created it: files whose
    /// creation time matches a process start are claimed first, and any
    /// remaining processes pair with the remaining files by recency only when
    /// the file shows activity after that process started. Without this, two
    /// pi processes in one directory both read the most recently written
    /// session and report the active one's state, and a brand-new process that
    /// has not materialized its own session file yet would inherit the newest
    /// stale session of a dead conversation.
    pub fn analyze(&mut self, cwd: &Path, live: &[LiveSession]) -> Vec<Option<PiFacts>> {
        if live.is_empty() {
            return Vec::new();
        }
        let Some(agent_dir) = dirs::home_dir().map(|home| home.join(".pi/agent")) else {
            return live.iter().map(|_| None).collect();
        };
        let root = agent_dir.join("sessions").join(encode_project_key(cwd));
        let candidates = session_candidates(&root);
        if candidates.is_empty() {
            return live.iter().map(|_| None).collect();
        }
        let mut output: Vec<Option<PiFacts>> = vec![None; live.len()];
        for (index, claim) in assign_sessions(live, &candidates).into_iter().enumerate() {
            if let Some(claim) = claim {
                output[index] = self.read_facts(&candidates[claim].path, &agent_dir);
            }
        }
        output
    }

    fn read_facts(&mut self, session: &Path, agent_dir: &Path) -> Option<PiFacts> {
        let metadata = session.metadata().ok()?;
        let session_modified = metadata.modified().ok()?;
        let models = agent_dir.join("models.json");
        let models_modified = models
            .metadata()
            .ok()
            .and_then(|entry| entry.modified().ok());
        // Catalog providers resolve their window from models-store.json, so a
        // refreshed store must also invalidate cached facts.
        let store_modified = agent_dir
            .join("models-store.json")
            .metadata()
            .ok()
            .and_then(|entry| entry.modified().ok());
        if let Some(cached) = self.cache.get_mut(session) {
            cached.last_access = Instant::now();
            if cached.session_modified == session_modified
                && cached.session_size == metadata.len()
                && cached.models_modified == models_modified
                && cached.store_modified == store_modified
            {
                return Some(cached.facts.clone());
            }
        }
        let facts = parse_signals(&read_tail(session)?, agent_dir);
        self.cache.insert(
            session.to_path_buf(),
            Cached {
                session_modified,
                session_size: metadata.len(),
                models_modified,
                store_modified,
                facts: facts.clone(),
                last_access: Instant::now(),
            },
        );
        self.cache.retain(|path, cached| {
            path.is_file() && cached.last_access.elapsed().as_secs() < 86_400
        });
        Some(facts)
    }
}

pub fn encode_project_key(cwd: &Path) -> String {
    let normalized = cwd.to_string_lossy();
    let trimmed = normalized.trim_start_matches(|ch| matches!(ch, '/' | '\\' | ':'));
    format!("--{}--", trimmed.replace(['/', '\\', ':'], "-"))
}

fn session_candidates(root: &Path) -> Vec<Candidate> {
    let Ok(entries) = root.read_dir() else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                let metadata = path.metadata().ok()?;
                if metadata.is_file() {
                    let created = session_created(&path).unwrap_or(SystemTime::UNIX_EPOCH);
                    let modified = metadata.modified().unwrap_or(created);
                    return Some(Candidate {
                        path,
                        created,
                        modified,
                    });
                }
            }
            None
        })
        .collect()
}

/// Slack (seconds) allowed between a process's start and a session file's last
/// write when deciding the process wrote the file. Uptime comes from `ps`
/// whole-second etime and filesystem mtime clocks can be coarse, so a fresh
/// session file written right at boot can lag the start by a few seconds.
const RECENT_WRITE_TOLERANCE_SECONDS: u64 = 5;

/// A pi process may only own a session file that was written at (or within
/// tolerance of) its start. A file whose last write clearly predates the
/// process start belongs to an earlier conversation.
///
/// This matters because pi does not materialize its session file until the
/// first user message arrives: a brand-new process idling at the empty prompt
/// has no file of its own, and adopting the directory's most recently written
/// old file would leak that dead conversation's tail state (e.g. an old
/// "…还需要做什么吗？" turn reported as WaitingReply) into the fresh process.
fn written_while_alive(file: &Candidate, process: &LiveSession) -> bool {
    let started = unix_seconds(process.started);
    let modified = unix_seconds(file.modified);
    started.saturating_sub(modified) <= RECENT_WRITE_TOLERANCE_SECONDS
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map(|delta| delta.as_secs())
        .unwrap_or(0)
}

/// Assign each live pi process one session file. Returns an index into
/// `candidates` per entry of `live` (or None when there is no fit).
fn assign_sessions(live: &[LiveSession], candidates: &[Candidate]) -> Vec<Option<usize>> {
    let mut claims: Vec<Option<usize>> = vec![None; live.len()];
    let mut claimed = vec![false; candidates.len()];

    // 1. Anchor: a pi process creates its fresh session file at startup, so a
    //    file created within ANCHOR_WINDOW_SECONDS of a process start belongs
    //    to that process. Claim the closest pair first so a slow-starting
    //    sibling cannot steal another process's file.
    loop {
        let mut best: Option<(usize, usize)> = None;
        let mut best_delta = u64::MAX;
        for (index, process) in live.iter().enumerate() {
            if claims[index].is_some() {
                continue;
            }
            for (candidate, file) in candidates.iter().enumerate() {
                if claimed[candidate] {
                    continue;
                }
                if let Some(delta) = seconds_between(file.created, process.started) {
                    if delta <= ANCHOR_WINDOW_SECONDS
                        && delta < best_delta
                        && written_while_alive(file, process)
                    {
                        best = Some((index, candidate));
                        best_delta = delta;
                    }
                }
            }
        }
        let Some((index, candidate)) = best else {
            break;
        };
        claims[index] = Some(candidate);
        claimed[candidate] = true;
    }

    // 2. Remaining processes (typically ones that resumed an older session and
    //    are writing it again) pair with the remaining files by recency: the
    //    process started most recently owns the file written most recently.
    //    A file is only offered to a process when its last write is at or
    //    after that process's start; otherwise the file belongs to a session
    //    the process never wrote, and the process is left unbound so it shows
    //    its default Ready state instead of a stale session's facts.
    let mut unbound: Vec<usize> = (0..live.len())
        .filter(|index| claims[*index].is_none())
        .collect();
    unbound.sort_by_key(|index| std::cmp::Reverse(live[*index].started));
    let mut free: Vec<usize> = (0..candidates.len())
        .filter(|candidate| !claimed[*candidate])
        .collect();
    free.sort_by_key(|candidate| std::cmp::Reverse(candidates[*candidate].modified));
    for index in unbound {
        if let Some(position) = free
            .iter()
            .position(|candidate| written_while_alive(&candidates[*candidate], &live[index]))
        {
            claims[index] = Some(free.remove(position));
        }
    }
    claims
}

fn seconds_between(a: SystemTime, b: SystemTime) -> Option<u64> {
    if a >= b {
        a.duration_since(b).ok().map(|delta| delta.as_secs())
    } else {
        b.duration_since(a).ok().map(|delta| delta.as_secs())
    }
}

/// pi names session files like `2026-09-08T15-12-35-453Z_<uuid>.jsonl` with
/// the UTC creation time first. Parse it without a chrono dependency.
fn session_created(path: &Path) -> Option<SystemTime> {
    let name = path.file_name()?.to_str()?;
    let bytes = name.as_bytes();
    if bytes.len() < 24 || bytes[10] != b'T' || bytes[23] != b'Z' {
        return None;
    }
    let year = digits(&bytes[0..4])? as i64;
    let month = digits(&bytes[5..7])? as i64;
    let day = digits(&bytes[8..10])? as i64;
    let hour = digits(&bytes[11..13])? as i64;
    let minute = digits(&bytes[14..16])? as i64;
    let second = digits(&bytes[17..19])? as i64;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let seconds = civil_seconds(year, month, day, hour, minute, second);
    let seconds = u64::try_from(seconds).ok()?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

fn digits(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || !bytes.iter().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(
        bytes
            .iter()
            .fold(0u32, |value, byte| value * 10 + u32::from(byte - b'0')),
    )
}

/// Convert a UTC civil time to seconds since the Unix epoch using Howard
/// Hinnant's days-from-civil algorithm.
fn civil_seconds(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146097 + day_of_era - 719468;
    days * 86_400 + hour * 3_600 + minute * 60 + second
}

fn read_tail(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(size.saturating_sub(TAIL_BYTES as u64)))
        .ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn parse_signals(text: &str, agent_dir: &Path) -> PiFacts {
    let mut facts = PiFacts::default();
    let mut provider = None;
    let mut model_id = None;
    let mut task_state = None;
    let mut reply_requested = false;
    let mut pending_tools = HashSet::new();
    let mut last_usage = None;

    for line in text
        .lines()
        .rev()
        .take(TAIL_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry["type"] != "message" {
            continue;
        }
        let message = &entry["message"];
        match message["role"].as_str() {
            Some("user") => {
                // A user message starts a new turn: the agent is generating or
                // executing tools until its next assistant message arrives.
                // Without this, the gap between the user message and the next
                // assistant event (often tens of seconds to minutes) would keep
                // reporting the previous turn's Ready state.
                reply_requested = false;
                task_state = Some(AgentState::Working);
                // A message sent while a tool call is in flight (the user
                // steering the agent) interrupts that call: pi never writes its
                // toolResult. Leaving it pending would pin the session to
                // Working for the rest of the conversation.
                pending_tools.clear();
            }
            Some("toolResult") => {
                if let Some(id) = message["toolCallId"].as_str() {
                    pending_tools.remove(id);
                }
            }
            Some("assistant") => {
                let current_provider = normalize(message["provider"].as_str());
                let current_model = normalize(message["model"].as_str());
                if let Some(model) = current_model.as_ref() {
                    facts.model = Some(
                        current_provider
                            .as_ref()
                            .map_or_else(|| model.clone(), |p| format!("{p}/{model}")),
                    );
                    provider = current_provider;
                    model_id = current_model;
                }
                let calls: Vec<_> = message["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|block| block["type"] == "toolCall")
                    .filter_map(|block| block["id"].as_str())
                    .collect();
                for id in &calls {
                    pending_tools.insert((*id).to_owned());
                }
                match message["stopReason"].as_str() {
                    Some("toolUse") if !calls.is_empty() => {
                        task_state = Some(AgentState::Working);
                        reply_requested = false;
                    }
                    Some("toolUse") => {
                        task_state = Some(AgentState::Working);
                        reply_requested = false;
                    }
                    Some("error" | "aborted") => {
                        // The turn ended before its tool calls ran (or they were
                        // interrupted), so pi will never write toolResult entries
                        // for these ids. Leaving them pending would pin the state
                        // to Working forever, even after later turns finish.
                        for id in &calls {
                            pending_tools.remove(*id);
                        }
                        task_state = Some(AgentState::Ready);
                        reply_requested = false;
                    }
                    Some("stop" | "length") => {
                        task_state = Some(AgentState::Ready);
                        reply_requested = message["stopReason"] == "stop"
                            && last_assistant_text(message)
                                .is_some_and(|text| ends_with_question(&text));
                    }
                    _ if !calls.is_empty() => {
                        task_state = Some(AgentState::Working);
                        reply_requested = false;
                    }
                    _ => {}
                }
                if !matches!(message["stopReason"].as_str(), Some("aborted" | "error")) {
                    last_usage = usage_tokens(&message["usage"]);
                }
            }
            _ => {}
        }
    }
    if !pending_tools.is_empty() {
        task_state = Some(AgentState::Working);
        reply_requested = false;
    }
    facts.state = if reply_requested {
        Some(AgentState::WaitingReply)
    } else {
        task_state
    };
    facts.context = last_usage
        .zip(context_window(
            agent_dir,
            provider.as_deref(),
            model_id.as_deref(),
        ))
        .map(|(used_tokens, window_tokens)| ContextUsage {
            used_tokens,
            window_tokens,
        });
    facts
}

fn normalize(value: Option<&str>) -> Option<String> {
    let value = value?.replace(['\r', '\n', '|'], " ");
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!value.is_empty()).then(|| value.chars().take(80).collect())
}

fn usage_tokens(usage: &Value) -> Option<u64> {
    let total = usage["totalTokens"].as_u64().filter(|total| *total > 0);
    total.or_else(|| {
        let total = ["input", "output", "cacheRead", "cacheWrite"]
            .into_iter()
            .filter_map(|key| usage[key].as_u64())
            .sum();
        (total > 0).then_some(total)
    })
}

/// Resolve a model's context window. User-defined providers live in
/// `models.json` under `/providers/{id}/models`, while providers pi fetches
/// from a catalog (e.g. a ZAI Coding Plan's zai-coding-cn models) are cached
/// in `models-store.json` as `{id: {models: [...]}}` and never appear in
/// models.json. Either source may own the provider; a custom entry wins over
/// the catalog cache.
fn context_window(agent_dir: &Path, provider: Option<&str>, model: Option<&str>) -> Option<u64> {
    let provider = provider?;
    let model = model?;
    let read_json = |name: &str| {
        fs::read(agent_dir.join(name))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .unwrap_or(Value::Null)
    };
    let custom = read_json("models.json");
    let store = read_json("models-store.json");
    custom
        .pointer(&format!("/providers/{}/models", escape(provider)))
        .or_else(|| store.pointer(&format!("/{}/models", escape(provider))))
        .and_then(Value::as_array)?
        .iter()
        .find(|entry| entry["id"] == model)?["contextWindow"]
        .as_u64()
        .filter(|value| *value > 0)
}

fn escape(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn last_assistant_text(message: &Value) -> Option<String> {
    message["content"]
        .as_array()?
        .iter()
        .rev()
        .find(|block| block["type"] == "text")?["text"]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn ends_with_question(text: &str) -> bool {
    text.trim_end_matches(|ch: char| {
        matches!(ch, '"' | '\'' | ')' | ']' | '}' | '。' | '！' | '!' | '.')
    })
    .ends_with(['?', '？'])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "agent-status-indicator-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn catalog_provider_resolves_window_from_models_store() {
        // Regression: providers fetched from a catalog (e.g. the ZAI Coding
        // Plan's zai-coding-cn) only exist in models-store.json, never in the
        // user's models.json; their context window must still resolve.
        let dir = scratch_dir("pi-store");
        std::fs::write(
            dir.join("models-store.json"),
            r#"{"zai-coding-cn":{"models":[{"id":"glm-5.3","contextWindow":1000000}]}}"#,
        )
        .unwrap();
        let facts = parse_signals(
            r#"{"type":"message","message":{"role":"assistant","provider":"zai-coding-cn","model":"glm-5.3","stopReason":"stop","content":[{"type":"text","text":"Done."}],"usage":{"totalTokens":20715}}}"#,
            &dir,
        );
        let context = facts.context.expect("context usage");
        assert_eq!(context.used_tokens, 20_715);
        assert_eq!(context.window_tokens, 1_000_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn custom_models_win_over_the_catalog_cache() {
        let dir = scratch_dir("pi-custom");
        std::fs::write(
            dir.join("models.json"),
            r#"{"providers":{"ai-relay":{"models":[{"id":"gpt-5.6-sol","contextWindow":400000}]}}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("models-store.json"),
            r#"{"ai-relay":{"models":[{"id":"gpt-5.6-sol","contextWindow":1}]}}"#,
        )
        .unwrap();
        let facts = parse_signals(
            r#"{"type":"message","message":{"role":"assistant","provider":"ai-relay","model":"gpt-5.6-sol","stopReason":"stop","content":[],"usage":{"totalTokens":10}}}"#,
            &dir,
        );
        assert_eq!(facts.context.map(|c| c.window_tokens), Some(400_000));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn encodes_project_key() {
        assert_eq!(
            encode_project_key(Path::new("/Users/a/project")),
            "--Users-a-project--"
        );
    }

    #[test]
    fn tool_call_is_working() {
        let facts = parse_signals(
            r#"{"type":"message","message":{"role":"assistant","provider":"openai","model":"gpt-5","stopReason":"toolUse","content":[{"type":"toolCall","id":"a"}]}}"#,
            Path::new("/missing"),
        );
        assert_eq!(facts.state, Some(AgentState::Working));
        assert_eq!(facts.model.as_deref(), Some("openai/gpt-5"));
    }

    #[test]
    fn a_steering_message_drops_the_interrupted_tool_call() {
        // The user can send a message while a tool call is in flight; pi never
        // writes a toolResult for that call, so it must not keep the session
        // Working after the new turn ends.
        let facts = parse_signals(
            concat!(
                r#"{"type":"message","message":{"role":"assistant","stopReason":"toolUse","content":[{"type":"toolCall","id":"interrupted"}]}}"#,
                "\n",
                r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"等等，先别改"}]}}"#,
                "\n",
                r#"{"type":"message","message":{"role":"assistant","stopReason":"toolUse","content":[{"type":"toolCall","id":"next"}]}}"#,
                "\n",
                r#"{"type":"message","message":{"role":"toolResult","toolCallId":"next"}}"#,
                "\n",
                r#"{"type":"message","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"完成。"}]}}"#
            ),
            Path::new("/missing"),
        );
        assert_eq!(facts.state, Some(AgentState::Ready));
    }

    #[test]
    fn files_without_tool_results_do_not_clear_real_pending_calls() {
        // Sanity check for the same code path: a tool call that is still
        // running (no result, no user message) keeps reporting Working.
        let facts = parse_signals(
            concat!(
                r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"跑一下"}]}}"#,
                "\n",
                r#"{"type":"message","message":{"role":"assistant","stopReason":"toolUse","content":[{"type":"toolCall","id":"running"}]}}"#
            ),
            Path::new("/missing"),
        );
        assert_eq!(facts.state, Some(AgentState::Working));
    }

    #[test]
    fn stopped_question_waits_for_reply() {
        let facts = parse_signals(
            r#"{"type":"message","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"Continue?"}]}}"#,
            Path::new("/missing"),
        );
        assert_eq!(facts.state, Some(AgentState::WaitingReply));
    }

    #[test]
    fn errored_tool_call_does_not_stick_working() {
        // A tool call whose turn errored out before executing never receives a
        // toolResult. Its id must not keep the state Working forever.
        let facts = parse_signals(
            concat!(
                r#"{"type":"message","message":{"role":"assistant","stopReason":"toolUse","content":[{"type":"toolCall","id":"a"}]}}"#,
                "\n",
                r#"{"type":"message","message":{"role":"toolResult","toolCallId":"a"}}"#,
                "\n",
                r#"{"type":"message","message":{"role":"assistant","stopReason":"error","content":[{"type":"thinking"},{"type":"toolCall","id":"b"}]}}"#,
                "\n",
                r#"{"type":"message","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"Done."}]}}"#
            ),
            Path::new("/missing"),
        );
        assert_eq!(facts.state, Some(AgentState::Ready));
    }

    #[test]
    fn completed_tool_call_is_ready() {
        let facts = parse_signals(
            concat!(
                r#"{"type":"message","message":{"role":"assistant","stopReason":"toolUse","content":[{"type":"toolCall","id":"a"}]}}"#,
                "\n",
                r#"{"type":"message","message":{"role":"toolResult","toolCallId":"a"}}"#,
                "\n",
                r#"{"type":"message","message":{"role":"assistant","stopReason":"stop","content":[]}}"#
            ),
            Path::new("/missing"),
        );
        assert_eq!(facts.state, Some(AgentState::Ready));
    }

    #[test]
    fn user_message_starts_a_working_turn() {
        // After a completed turn, a user message means the agent is generating
        // or running tools again; the old Ready state must not linger while no
        // assistant event has been written yet.
        let facts = parse_signals(
            concat!(
                r#"{"type":"message","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"Done."}]}}"#,
                "\n",
                r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"继续"}]}}"#
            ),
            Path::new("/missing"),
        );
        assert_eq!(facts.state, Some(AgentState::Working));
    }

    #[test]
    fn user_reply_clears_waiting_reply_and_starts_working() {
        let facts = parse_signals(
            concat!(
                r#"{"type":"message","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"继续吗？"}]}}"#,
                "\n",
                r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"是"}]}}"#
            ),
            Path::new("/missing"),
        );
        assert_eq!(facts.state, Some(AgentState::Working));
    }

    fn live(started_ago: u64) -> LiveSession {
        LiveSession {
            started: SystemTime::now() - Duration::from_secs(started_ago),
        }
    }

    fn candidate(created_ago: u64, modified_ago: u64) -> Candidate {
        let now = SystemTime::now();
        Candidate {
            path: PathBuf::from("unused"),
            created: now - Duration::from_secs(created_ago),
            modified: now - Duration::from_secs(modified_ago),
        }
    }

    #[test]
    fn session_created_parses_pi_file_names() {
        let path = Path::new("2026-09-08T15-12-35-453Z_01a08194-007d-724a-80b9-7194e96353f9.jsonl");
        let created = session_created(path).unwrap();
        let epoch = created
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(epoch, 1788880355);
        assert!(session_created(Path::new("not-a-session.jsonl")).is_none());
    }

    #[test]
    fn each_live_pi_keeps_its_own_session() {
        // Mirror the reported bug: two pi processes share a directory. The one
        // started most recently created a fresh session and is now writing it,
        // while the older process resumed an old session and is idle.
        let idle_session = candidate(3 * 86_400, 3_600);
        let busy_session = candidate(49, 1);
        let sessions = [idle_session, busy_session];

        let idle_first = assign_sessions(&[live(2 * 86_400), live(50)], &sessions);
        assert_eq!(idle_first[0], Some(0), "idle process keeps its old session");
        assert_eq!(
            idle_first[1],
            Some(1),
            "working process keeps its new session"
        );

        // Results stay aligned with the caller's process order.
        let busy_first = assign_sessions(&[live(50), live(2 * 86_400)], &sessions);
        assert_eq!(busy_first[0], Some(1));
        assert_eq!(busy_first[1], Some(0));
    }

    #[test]
    fn without_creation_anchor_sessions_pair_by_recency() {
        // Both processes resumed old sessions, so neither file creation matches
        // a process start. The most recently started process owns the file
        // written most recently.
        let older_file = candidate(3 * 86_400, 7_200);
        let newer_file = candidate(2 * 86_400, 300);
        let sessions = [older_file, newer_file];
        let claims = assign_sessions(&[live(7_200), live(300)], &sessions);
        assert_eq!(claims[0], Some(0));
        assert_eq!(claims[1], Some(1));
    }

    #[test]
    fn stale_sessions_are_not_claimed_by_a_single_live_pi() {
        // A directory can accumulate sessions from runs that already exited. A
        // single live process must bind to its own file, not necessarily the
        // newest one.
        let live_session = candidate(60, 5);
        let stale = candidate(4 * 86_400, 86_400);
        let sessions = [stale, live_session];
        let claims = assign_sessions(&[live(55)], &sessions);
        assert_eq!(claims[0], Some(1), "fresh file belongs to the live process");
    }

    #[test]
    fn fresh_pi_without_own_file_is_not_bound_to_a_stale_session() {
        // Regression: pi materializes its session file only once the first user
        // message arrives. A newly started process idling at the empty prompt
        // therefore has no file of its own, and must not adopt the directory's
        // most recently written old session whose tail question would misreport
        // it as WaitingReply.
        let old_session = candidate(2 * 86_400, 3_600);
        let claims = assign_sessions(&[live(90)], &[old_session]);
        assert_eq!(claims[0], None, "fresh process stays unbound (Ready)");
    }

    #[test]
    fn fresh_pi_ignores_all_old_files_regardless_of_their_recency() {
        let old_recent = candidate(3 * 86_400, 600);
        let old_older = candidate(5 * 86_400, 86_400);
        let claims = assign_sessions(&[live(300)], &[old_older, old_recent]);
        assert_eq!(claims[0], None, "no pre-start file may be adopted");
    }

    #[test]
    fn anchor_ignores_another_sessions_file_whose_writes_stopped_before_start() {
        // A file created inside the anchor window by an earlier process that has
        // since exited must not be claimed once its writes stopped before the
        // new process started.
        let dead_session = candidate(60, 60);
        let claims = assign_sessions(&[live(30)], &[dead_session]);
        assert_eq!(claims[0], None);
    }

    #[test]
    fn resumed_session_with_recent_writes_still_binds_by_recency() {
        // A process that resumed an old session and is actively appending to it
        // keeps owning that file.
        let resumed = candidate(3 * 86_400, 10);
        let claims = assign_sessions(&[live(120)], &[resumed]);
        assert_eq!(claims[0], Some(0));
    }

    #[test]
    fn stale_files_never_win_over_a_live_process_own_anchor() {
        // A fresh file anchored to the live process beats a more recently
        // modified stale file that predates the process.
        let stale = candidate(3 * 86_400, 30);
        let own = candidate(20, 1);
        let claims = assign_sessions(&[live(15)], &[stale, own]);
        assert_eq!(claims[0], Some(1), "own anchored file wins over stale file");
    }
}
