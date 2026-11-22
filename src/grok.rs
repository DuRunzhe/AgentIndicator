//! Grok CLI (`grok`) sessions.
//!
//! Grok stores one directory per conversation under
//! `$GROK_HOME/sessions/<url-encoded-cwd>/<session-id>/`:
//!
//! * `summary.json` — title, working directory, timestamps and current model;
//! * `signals.json` — the model and the live context-window figures;
//! * `updates.jsonl` — the ACP update stream, which is the turn timeline.
//!
//! State is read from the tail of `updates.jsonl`: a `turn_completed` update
//! closes a turn, `pending_interaction` means the agent is waiting for the user,
//! and an in-flight tool call or streamed chunk means it is working. The older
//! shape without `turn_completed` leaves the state to the detector's
//! process-activity fallback rather than guessing.

use crate::agent_sessions::{
    bind_sessions, canonical_key, clean_text, content_text, percent_decode, read_small_file,
    text_ends_with_question, AgentFacts, Candidate, LiveSession,
};
use crate::model::{AgentState, ContextUsage};
use crate::timestamps::parse_rfc3339_millis;
use serde_json::Value;
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

/// How much of the update stream is replayed. The question heuristic only needs
/// the current turn; a turn larger than this still ends with its closing chunk
/// in the tail.
const TAIL_BYTES: u64 = 2 * 1024 * 1024;
const CACHE_LIFETIME: Duration = Duration::from_secs(86_400);

#[derive(Default)]
pub struct GrokAnalyzer {
    cache: HashMap<PathBuf, Cached>,
}

struct Cached {
    signature: Signature,
    facts: AgentFacts,
    last_access: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Signature {
    summary: (Option<SystemTime>, u64),
    signals: (Option<SystemTime>, u64),
    updates: (Option<SystemTime>, u64),
}

impl GrokAnalyzer {
    /// Resolve one session per live Grok process in `cwd`.
    pub fn analyze(&mut self, cwd: &Path, live: &[LiveSession]) -> Vec<Option<AgentFacts>> {
        if live.is_empty() {
            return Vec::new();
        }
        let Some(home) = grok_home() else {
            return live.iter().map(|_| None).collect();
        };
        self.analyze_in(&home.join("sessions"), cwd, live)
    }

    fn analyze_in(
        &mut self,
        root: &Path,
        cwd: &Path,
        live: &[LiveSession],
    ) -> Vec<Option<AgentFacts>> {
        let candidates = session_candidates(root, cwd);
        if candidates.is_empty() {
            return live.iter().map(|_| None).collect();
        }
        bind_sessions(live, &candidates)
            .into_iter()
            .map(|claim| claim.and_then(|index| self.read_facts(&candidates[index].path)))
            .collect()
    }

    fn read_facts(&mut self, directory: &Path) -> Option<AgentFacts> {
        let summary_path = directory.join("summary.json");
        let signals_path = directory.join("signals.json");
        let updates_path = directory.join("updates.jsonl");
        let signature = Signature {
            summary: file_state(&summary_path),
            signals: file_state(&signals_path),
            updates: file_state(&updates_path),
        };
        if let Some(cached) = self.cache.get_mut(directory) {
            cached.last_access = Instant::now();
            if cached.signature == signature {
                return Some(cached.facts.clone());
            }
        }
        let summary = read_json(&summary_path)?;
        let signals = read_json(&signals_path).unwrap_or(Value::Null);
        let updates_age = updates_path
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| SystemTime::now().duration_since(modified).ok());
        let facts = facts_from(
            &summary,
            &signals,
            &read_tail(&updates_path).unwrap_or_default(),
            updates_age,
        );
        self.cache.insert(
            directory.to_path_buf(),
            Cached {
                signature,
                facts: facts.clone(),
                last_access: Instant::now(),
            },
        );
        self.cache
            .retain(|path, cached| path.is_dir() && cached.last_access.elapsed() < CACHE_LIFETIME);
        Some(facts)
    }
}

fn grok_home() -> Option<PathBuf> {
    std::env::var_os("GROK_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".grok")))
}

fn file_state(path: &Path) -> (Option<SystemTime>, u64) {
    match path.metadata() {
        Ok(metadata) => (metadata.modified().ok(), metadata.len()),
        Err(_) => (None, 0),
    }
}

fn millis_time(millis: i64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_millis(millis.max(0) as u64)
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn read_tail(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let offset = size.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if offset > 0 {
        // The first line is probably cut in the middle; drop it.
        if let Some(newline) = text.find('\n') {
            text.drain(..=newline);
        }
    }
    Some(text)
}

/// The working directory a session group belongs to. Grok URL-encodes the path
/// into the directory name; a path that does not fit in a file name is stored
/// in a `.cwd` marker inside the group instead.
fn group_cwd(group: &Path) -> Option<PathBuf> {
    if let Some(marker) = read_small_file(&group.join(".cwd")) {
        let marker = marker.lines().next().unwrap_or_default().trim();
        if !marker.is_empty() {
            return Some(PathBuf::from(marker));
        }
    }
    let name = group.file_name()?.to_str()?;
    Some(PathBuf::from(percent_decode(name)))
}

fn session_candidates(root: &Path, cwd: &Path) -> Vec<Candidate> {
    let Some(groups) = matching_groups(root, cwd) else {
        return Vec::new();
    };
    let mut candidates: Vec<(Candidate, bool)> = Vec::new();
    for group in groups {
        let Ok(entries) = fs::read_dir(&group) else {
            continue;
        };
        for entry in entries.flatten() {
            let directory = entry.path();
            if !directory.is_dir() {
                continue;
            }
            let summary_path = directory.join("summary.json");
            let Some(summary) = read_json(&summary_path) else {
                continue;
            };
            // The recorded cwd is authoritative: it survives symlinks and a
            // group slug that does not decode to the same string.
            if let Some(recorded) = summary["info"]["cwd"].as_str() {
                if canonical_key(Path::new(recorded)) != canonical_key(cwd) {
                    continue;
                }
            }
            let summary_modified = summary_path
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok());
            let parsed = summary["updated_at"]
                .as_str()
                .and_then(parse_rfc3339_millis)
                .or_else(|| {
                    summary["last_active_at"]
                        .as_str()
                        .and_then(parse_rfc3339_millis)
                })
                .map(millis_time);
            // Binding asks "is this file being written now?", so the file's own
            // mtime participates: a resumed conversation's `updated_at` is old
            // until its first new turn, while the file is already being touched.
            let modified = match (parsed, summary_modified) {
                (Some(parsed), Some(modified)) => parsed.max(modified),
                (Some(parsed), None) => parsed,
                (None, Some(modified)) => modified,
                (None, None) => SystemTime::UNIX_EPOCH,
            };
            let created = summary["created_at"]
                .as_str()
                .and_then(parse_rfc3339_millis)
                .map(millis_time)
                .unwrap_or(modified);
            let parented = ["parent_session_id", "parentSessionId"]
                .iter()
                .any(|key| summary[*key].as_str().is_some_and(|id| !id.is_empty()));
            candidates.push((
                Candidate {
                    path: directory,
                    created,
                    modified,
                },
                parented,
            ));
        }
    }
    // Grok writes subagent sessions into the same tree: they have a parent and
    // are written while the main conversation is idle, so binding to the newest
    // file would make the main row mirror a subagent. Keep top-level sessions
    // whenever the directory has one; a fork is parented too and is only used
    // when no top-level session exists. That trades a missing title on a rare
    // resumed fork for never showing one conversation's state on another.
    let has_top_level = candidates.iter().any(|(_, parented)| !parented);
    candidates
        .into_iter()
        .filter(|(_, parented)| !has_top_level || !parented)
        .map(|(candidate, _)| candidate)
        .collect()
}

fn matching_groups(root: &Path, cwd: &Path) -> Option<Vec<PathBuf>> {
    let key = canonical_key(cwd);
    let mut groups = Vec::new();
    for entry in fs::read_dir(root).ok()?.flatten() {
        let group = entry.path();
        if !group.is_dir() {
            continue;
        }
        if group_cwd(&group)
            .as_deref()
            .is_some_and(|path| canonical_key(path) == key)
        {
            groups.push(group);
        }
    }
    Some(groups)
}

pub(crate) struct UsageRecord {
    pub at: i64,
    pub input: u64,
    pub input_cached: u64,
    pub output: u64,
    pub cost: Option<f64>,
}

/// The provider-recorded usage of every completed turn in one update stream.
///
/// A turn may be re-emitted as its usage grows, so the last record for a
/// `prompt_id` wins; summing the earlier ones would multiply the turn. The
/// returned order follows the file, which is enough for the day buckets.
pub(crate) fn turn_usage_records(path: &Path) -> Vec<UsageRecord> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let fallback = path
        .metadata()
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|delta| delta.as_millis() as i64)
        .unwrap_or(0);
    let mut records: Vec<UsageRecord> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();
    let mut without_id = 0usize;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let update = &value["params"]["update"];
        if update["sessionUpdate"] != "turn_completed" {
            continue;
        }
        let usage = &update["usage"];
        if !usage.is_object() {
            continue;
        }
        let number = |names: &[&str]| {
            names
                .iter()
                .find_map(|name| usage[*name].as_u64())
                .unwrap_or(0)
        };
        let input = number(&["inputTokens", "input_tokens"]);
        let output = number(&["outputTokens", "output_tokens"]);
        let cached_read = number(&["cachedReadTokens", "cacheReadInputTokens"]);
        let cache_creation = number(&["cacheCreationTokens", "cacheCreationInputTokens"]);
        let cost = ["costUsdTicks", "cost_usd_ticks"]
            .iter()
            .find_map(|name| usage[*name].as_f64())
            .filter(|ticks| *ticks > 0.0)
            .map(|ticks| ticks / 1e10);
        let at = value["params"]["_meta"]["agentTimestampMs"]
            .as_i64()
            .or_else(|| value["timestamp"].as_str().and_then(parse_rfc3339_millis))
            .or_else(|| value["timestamp"].as_i64().map(normalize_millis))
            .or_else(|| {
                value["params"]["_meta"]["agentTimestamp"]
                    .as_i64()
                    .map(normalize_millis)
            })
            .unwrap_or(fallback);
        let record = UsageRecord {
            at,
            input,
            input_cached: cached_read.saturating_add(cache_creation),
            output,
            cost,
        };
        let key = update["prompt_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                let key = format!("turn_completed:{without_id}");
                without_id += 1;
                key
            });
        if let Some(position) = positions.get(&key).copied() {
            records[position] = record;
        } else {
            positions.insert(key, records.len());
            records.push(record);
        }
    }
    records
}

/// Timestamps are seconds in ACP notifications and milliseconds in the xAI
/// updates. Anything smaller than this is seconds.
fn normalize_millis(value: i64) -> i64 {
    if value > 0 && value < 100_000_000_000 {
        value.saturating_mul(1_000)
    } else {
        value
    }
}

fn facts_from(
    summary: &Value,
    signals: &Value,
    updates: &str,
    updates_age: Option<Duration>,
) -> AgentFacts {
    let mut facts = AgentFacts::default();
    let timeline = Timeline::replay(updates);
    facts.state = timeline.state;
    // A turn can be interrupted after an earlier `turn_completed`: the new
    // turn's chunks and tool calls are in the log, but the end record never
    // arrives. While the stream is being written the turn is running; once it
    // has been quiet for a minute, leave the state to the detector's
    // process-activity fallback instead of reporting Working forever.
    if timeline.turn_open
        && facts.state == Some(AgentState::Working)
        && updates_age.is_some_and(|age| age > Duration::from_secs(60))
    {
        facts.state = None;
    }
    facts.model = summary["current_model_id"]
        .as_str()
        .and_then(clean_text)
        .or_else(|| signals["primaryModelId"].as_str().and_then(clean_text));
    let tokens = signals["contextTokensUsed"]
        .as_u64()
        .or_else(|| signals["context_tokens_used"].as_u64());
    let window = signals["contextWindowTokens"]
        .as_u64()
        .or_else(|| signals["context_window_tokens"].as_u64());
    if let (Some(used_tokens), Some(window_tokens)) = (tokens, window) {
        if window_tokens > 0 {
            facts.context = Some(ContextUsage {
                used_tokens,
                window_tokens,
            });
        }
    }
    facts.title = summary["generated_title"]
        .as_str()
        .and_then(clean_text)
        .or_else(|| summary["session_summary"].as_str().and_then(clean_text));
    facts
}

/// The state machine over `updates.jsonl`. Only turn-boundary events decide the
/// state; metadata updates in between leave it alone.
struct Timeline {
    state: Option<AgentState>,
    last_text: String,
    /// Whether the turn at the end of the replayed window is still open. An
    /// earlier completed turn must not hide an interrupted later one.
    turn_open: bool,
}

impl Timeline {
    fn replay(text: &str) -> Self {
        let mut timeline = Timeline {
            state: None,
            last_text: String::new(),
            turn_open: false,
        };
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let update = &value["params"]["update"];
            timeline.apply(update);
        }
        timeline
    }

    fn apply(&mut self, update: &Value) {
        match update["sessionUpdate"].as_str() {
            Some("user_message_chunk") => {
                self.last_text.clear();
                self.turn_open = true;
                self.state = Some(AgentState::Working);
            }
            Some("agent_message_chunk") => {
                if let Some(text) = content_text(&update["content"]) {
                    self.last_text.push_str(&text);
                    // Bound the tail: only the closing sentence matters.
                    if self.last_text.len() > 4_000 {
                        let keep = self.last_text.len() - 2_000;
                        self.last_text.drain(..keep);
                    }
                }
                self.turn_open = true;
                self.state = Some(AgentState::Working);
            }
            Some("agent_thought_chunk" | "response_started") => {
                self.turn_open = true;
                self.state = Some(AgentState::Working);
            }
            Some("tool_call" | "tool_call_update") => {
                self.turn_open = true;
                self.state = Some(AgentState::Working);
            }
            Some("pending_interaction") => {
                self.turn_open = true;
                self.state = Some(AgentState::Waiting);
            }
            Some("interaction_resolved") => {
                self.turn_open = true;
                self.state = Some(AgentState::Working);
            }
            Some("turn_completed") => {
                self.turn_open = false;
                self.state = Some(if turn_failed(update) {
                    AgentState::Error
                } else if text_ends_with_question(&self.last_text) {
                    AgentState::WaitingReply
                } else {
                    AgentState::Ready
                });
                self.last_text.clear();
            }
            // Usage, plan, mode and session-metadata updates say nothing about
            // whether a turn is running.
            _ => {}
        }
    }
}

fn turn_failed(update: &Value) -> bool {
    // `error_kind` can be a plain category string or a tagged object depending
    // on the CLI version; either way a non-null, non-"none" value is a failure.
    for key in ["error_kind", "errorKind"] {
        if let Some(kind) = update.get(key) {
            if !kind.is_null() && kind.as_str() != Some("none") && kind.as_str() != Some("") {
                return true;
            }
        }
    }
    update["stop_reason"]
        .as_str()
        .or_else(|| update["stopReason"].as_str())
        .is_some_and(|reason| {
            let reason = reason.to_ascii_lowercase();
            reason.contains("error") || reason.contains("fail")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_sessions::unix_seconds;
    use serde_json::json;
    use std::time::Duration;

    fn scratch(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "asi-grok-{tag}-{}-{}",
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

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn live(started: u64) -> LiveSession {
        LiveSession {
            started: at(started),
        }
    }

    fn session(signals: Value, updates: &[Value]) -> (PathBuf, PathBuf) {
        let dir = scratch("session");
        let root = dir.join("sessions");
        let session = root
            .join("%2FUsers%2Fme%2Fproject")
            .join("019edf9c-0000-7000-8000-000000000001");
        fs::create_dir_all(&session).expect("session dir");
        let summary = json!({
            "info": { "id": "s1", "cwd": "/Users/me/project" },
            "created_at": "2026-06-19T11:20:40.686261Z",
            "updated_at": "2026-06-19T11:31:12.282793Z",
            "current_model_id": "grok-build",
            "generated_title": "Fix the parser",
        });
        write(&session.join("summary.json"), &summary.to_string());
        write(&session.join("signals.json"), &signals.to_string());
        let lines = updates
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        write(&session.join("updates.jsonl"), &format!("{lines}\n"));
        (root, session)
    }

    fn update(kind: &str, extra: Value) -> Value {
        let mut update = json!({ "sessionUpdate": kind });
        if let Some(extra) = extra.as_object() {
            for (key, value) in extra {
                update[key] = value.clone();
            }
        }
        json!({ "params": { "update": update } })
    }

    fn analyze(root: &Path, cwd: &str, started: u64) -> Option<AgentFacts> {
        let mut analyzer = GrokAnalyzer::default();
        let facts = analyzer.analyze_in(root, Path::new(cwd), &[live(started)]);
        facts.into_iter().next().flatten()
    }

    #[test]
    fn a_completed_turn_is_ready_or_waits_for_the_answer() {
        let (root, _) = session(
            json!({ "primaryModelId": "grok-build", "contextTokensUsed": 40_000, "contextWindowTokens": 512_000 }),
            &[
                update(
                    "user_message_chunk",
                    json!({"content": {"type": "text", "text": "fix it"}}),
                ),
                update(
                    "agent_message_chunk",
                    json!({"content": {"type": "text", "text": "Done."}}),
                ),
                update(
                    "turn_completed",
                    json!({"prompt_id": "p1", "stop_reason": "end_turn"}),
                ),
            ],
        );
        let started = unix_seconds(
            root.join("%2FUsers%2Fme%2Fproject/019edf9c-0000-7000-8000-000000000001/summary.json")
                .metadata()
                .unwrap()
                .modified()
                .unwrap(),
        );
        let facts = analyze(&root, "/Users/me/project", started).expect("facts");
        assert_eq!(facts.state, Some(AgentState::Ready));
        assert_eq!(facts.model.as_deref(), Some("grok-build"));
        assert_eq!(facts.title.as_deref(), Some("Fix the parser"));
        assert_eq!(
            facts.context,
            Some(ContextUsage {
                used_tokens: 40_000,
                window_tokens: 512_000
            })
        );

        let (root, _) = session(
            json!({}),
            &[
                update("user_message_chunk", json!({"content": "fix it"})),
                update(
                    "agent_message_chunk",
                    json!({"content": "Should I delete it?"}),
                ),
                update("turn_completed", json!({"prompt_id": "p1"})),
            ],
        );
        assert_eq!(
            analyze(&root, "/Users/me/project", started).unwrap().state,
            Some(AgentState::WaitingReply)
        );
    }

    #[test]
    fn a_pending_interaction_waits_for_the_user() {
        let (root, _) = session(
            json!({}),
            &[
                update("user_message_chunk", json!({"content": "deploy"})),
                update(
                    "agent_message_chunk",
                    json!({"content": "Running the release."}),
                ),
                update("pending_interaction", json!({"interaction": "permission"})),
            ],
        );
        let started = unix_seconds(
            root.join("%2FUsers%2Fme%2Fproject/019edf9c-0000-7000-8000-000000000001/summary.json")
                .metadata()
                .unwrap()
                .modified()
                .unwrap(),
        );
        assert_eq!(
            analyze(&root, "/Users/me/project", started).unwrap().state,
            Some(AgentState::Waiting)
        );
    }

    #[test]
    fn an_in_flight_tool_call_is_working() {
        let (root, _) = session(
            json!({}),
            &[
                update("user_message_chunk", json!({"content": "run tests"})),
                update(
                    "tool_call",
                    json!({"toolCallId": "c1", "title": "run_terminal_command", "status": "in_progress"}),
                ),
            ],
        );
        let started = unix_seconds(
            root.join("%2FUsers%2Fme%2Fproject/019edf9c-0000-7000-8000-000000000001/summary.json")
                .metadata()
                .unwrap()
                .modified()
                .unwrap(),
        );
        assert_eq!(
            analyze(&root, "/Users/me/project", started).unwrap().state,
            Some(AgentState::Working)
        );
    }

    #[test]
    fn a_failed_turn_is_an_error() {
        let (root, _) = session(
            json!({}),
            &[
                update("user_message_chunk", json!({"content": "go"})),
                update(
                    "turn_completed",
                    json!({"prompt_id": "p1", "error_kind": "rate_limit"}),
                ),
            ],
        );
        let started = unix_seconds(
            root.join("%2FUsers%2Fme%2Fproject/019edf9c-0000-7000-8000-000000000001/summary.json")
                .metadata()
                .unwrap()
                .modified()
                .unwrap(),
        );
        assert_eq!(
            analyze(&root, "/Users/me/project", started).unwrap().state,
            Some(AgentState::Error)
        );
    }

    #[test]
    fn the_tail_keeps_the_latest_turn() {
        let (root, _) = session(
            json!({}),
            &[
                update("user_message_chunk", json!({"content": "one"})),
                update("turn_completed", json!({"prompt_id": "p1"})),
                update("user_message_chunk", json!({"content": "two"})),
                update("agent_message_chunk", json!({"content": "still here?"})),
            ],
        );
        let started = unix_seconds(
            root.join("%2FUsers%2Fme%2Fproject/019edf9c-0000-7000-8000-000000000001/summary.json")
                .metadata()
                .unwrap()
                .modified()
                .unwrap(),
        );
        assert_eq!(
            analyze(&root, "/Users/me/project", started).unwrap().state,
            Some(AgentState::Working)
        );
    }

    #[test]
    fn a_turn_without_an_end_event_is_left_to_process_activity() {
        let (root, _) = session(
            json!({}),
            &[update("usage_update", json!({"inputTokens": 10}))],
        );
        let started = unix_seconds(
            root.join("%2FUsers%2Fme%2Fproject/019edf9c-0000-7000-8000-000000000001/summary.json")
                .metadata()
                .unwrap()
                .modified()
                .unwrap(),
        );
        assert!(analyze(&root, "/Users/me/project", started)
            .expect("facts")
            .state
            .is_none());
    }

    #[test]
    fn an_interrupted_turn_after_an_earlier_completion_is_left_to_process_activity() {
        let lines = [
            update("user_message_chunk", json!({"content": "one"})),
            update("turn_completed", json!({"prompt_id": "p1"})),
            update("user_message_chunk", json!({"content": "two"})),
            update(
                "tool_call",
                json!({"toolCallId": "c1", "status": "in_progress"}),
            ),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        let facts = facts_from(
            &json!({}),
            &json!({}),
            &lines,
            Some(Duration::from_secs(120)),
        );
        assert_eq!(facts.state, None);

        // The same age on a finished turn must keep its ready state: only the
        // open turn is a guess that can go stale.
        let finished = update("turn_completed", json!({"prompt_id": "p1"})).to_string();
        let facts = facts_from(
            &json!({}),
            &json!({}),
            &finished,
            Some(Duration::from_secs(120)),
        );
        assert_eq!(facts.state, Some(AgentState::Ready));
    }

    #[test]
    fn usage_keeps_the_last_record_of_a_turn() {
        let dir = scratch("usage");
        let path = dir.join("updates.jsonl");
        let line = |prompt: &str, input: u64, output: u64, cached: u64| {
            json!({
                "timestamp": 1_781_868_672,
                "params": {
                    "update": {
                        "sessionUpdate": "turn_completed",
                        "prompt_id": prompt,
                        "usage": {
                            "inputTokens": input,
                            "outputTokens": output,
                            "cachedReadTokens": cached,
                            "cacheCreationTokens": 2,
                            "costUsdTicks": 1_500_000_000
                        }
                    }
                }
            })
            .to_string()
        };
        write(
            &path,
            &format!(
                "{}\n{}\n{}\n",
                line("p1", 100, 10, 40),
                line("p1", 120, 12, 50),
                line("p2", 7, 3, 0)
            ),
        );
        let records = turn_usage_records(&path);
        assert_eq!(records.len(), 2, "the re-emitted turn is counted once");
        assert_eq!(records[0].input, 120);
        assert_eq!(records[0].input_cached, 52);
        assert_eq!(records[0].output, 12);
        assert_eq!(records[0].cost, Some(0.15));
        assert_eq!(records[0].at, 1_781_868_672_000);
    }

    #[test]
    fn a_subagent_session_does_not_take_the_main_rows_place() {
        // Grok writes subagent conversations into the same sessions tree. The
        // subagent file is newer, but the main row must keep the top-level
        // conversation's title and state.
        let dir = scratch("subagent");
        let root = dir.join("sessions");
        let group = root.join("%2FUsers%2Fme%2Fproject");
        for (id, parent, title, updated) in [
            ("main", None, "Main", "2026-06-19T11:31:12.282793Z"),
            ("sub", Some("main"), "Sub", "2026-06-19T11:45:00.000000Z"),
        ] {
            let session = group.join(id);
            fs::create_dir_all(&session).expect("session dir");
            let mut summary = json!({
                "info": { "id": id, "cwd": "/Users/me/project" },
                "created_at": "2026-06-19T11:20:40.686261Z",
                "updated_at": updated,
                "generated_title": title
            });
            if let Some(parent) = parent {
                summary["parent_session_id"] = json!(parent);
            }
            write(&session.join("summary.json"), &summary.to_string());
        }
        let main = group.join("main/summary.json");
        let started = main.metadata().expect("summary").modified().expect("mtime");
        let mut analyzer = GrokAnalyzer::default();
        let facts = analyzer
            .analyze_in(
                &root,
                Path::new("/Users/me/project"),
                &[LiveSession { started }],
            )
            .into_iter()
            .next()
            .flatten()
            .expect("facts");
        assert_eq!(facts.title.as_deref(), Some("Main"));
    }

    #[test]
    fn a_encoded_group_name_matches_the_process_directory() {
        assert_eq!(
            group_cwd(Path::new("/tmp/%2FUsers%2Fme%2Fmy%20project")).as_deref(),
            Some(Path::new("/Users/me/my project"))
        );
        // A group that exceeded the name limit carries the real path in `.cwd`.
        let dir = scratch("cwd-marker");
        write(&dir.join(".cwd"), "/Users/me/a-very-long-project\n");
        assert_eq!(
            group_cwd(&dir).as_deref(),
            Some(Path::new("/Users/me/a-very-long-project"))
        );
    }
}
