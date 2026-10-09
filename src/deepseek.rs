use crate::model::{AgentState, ContextUsage};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime},
};

const TAIL_BYTES: usize = 256 * 1024;
const TAIL_LINES: usize = 500;

#[derive(Clone, Debug, Default)]
pub struct DeepSeekFacts {
    pub state: Option<AgentState>,
    pub model: Option<String>,
    pub context: Option<ContextUsage>,
}

struct Cached {
    modified: SystemTime,
    size: u64,
    facts: DeepSeekFacts,
    last_access: Instant,
}

#[derive(Default)]
pub struct DeepSeekAnalyzer {
    cache: HashMap<PathBuf, Cached>,
}

impl DeepSeekAnalyzer {
    pub fn analyze(&mut self, cwd: &Path) -> Option<DeepSeekFacts> {
        let home = dirs::home_dir()?.join(".dsh");
        let session = latest_session(cwd, &home)?;
        let metadata = session.metadata().ok()?;
        let modified = metadata.modified().ok()?;
        if let Some(cached) = self.cache.get_mut(&session) {
            cached.last_access = Instant::now();
            // Size and modification time alone decide whether the cache is
            // current. An extra "checked recently" window here used to win over
            // them, so an approval written just after the previous scan kept its
            // stale answer until the window expired.
            if cached.modified == modified && cached.size == metadata.len() {
                return Some(cached.facts.clone());
            }
        }
        let text = read_session_tail(&session)?;
        let mut facts = parse_signals(&text);
        if let Some(session_id) = session
            .parent()
            .and_then(Path::file_name)
            .and_then(|v| v.to_str())
        {
            apply_projection(&mut facts, session_id, &home, modified);
        }
        self.cache.insert(
            session,
            Cached {
                modified,
                size: metadata.len(),
                facts: facts.clone(),
                last_access: Instant::now(),
            },
        );
        prune_cache(&mut self.cache);
        Some(facts)
    }
}

fn prune_cache(cache: &mut HashMap<PathBuf, Cached>) {
    cache.retain(|path, entry| {
        path.is_file() && entry.last_access.elapsed() < Duration::from_secs(86_400)
    });
    while cache.len() > 200 {
        let oldest = cache
            .iter()
            .min_by_key(|(_, entry)| entry.last_access)
            .map(|(path, _)| path.clone());
        if let Some(path) = oldest {
            cache.remove(&path);
        } else {
            break;
        }
    }
}

fn latest_session(cwd: &Path, home: &Path) -> Option<PathBuf> {
    let preferred = home.join("sessions").join(encode_project_key(cwd));
    newest_session_under(&preferred).or_else(|| newest_session_under(&home.join("sessions")))
}

fn newest_session_under(root: &Path) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    collect_sessions(root, 0, &mut candidates);
    candidates
        .into_iter()
        .filter_map(|path| Some((path.metadata().ok()?.modified().ok()?, path)))
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

fn collect_sessions(root: &Path, depth: usize, output: &mut Vec<PathBuf>) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = root.read_dir() else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_sessions(&path, depth + 1, output);
        } else if matches!(
            path.file_name().and_then(|v| v.to_str()),
            Some("session.jsonl" | "session.jsonl.zstd")
        ) {
            output.push(path);
        }
    }
}

fn encode_project_key(cwd: &Path) -> String {
    let mut readable = String::new();
    let mut separator = false;
    for ch in cwd.to_string_lossy().chars() {
        if matches!(ch, '/' | '\\' | ':') {
            if !separator {
                readable.push('-');
            }
            separator = true;
        } else if ch != '~' && (ch.is_ascii_alphanumeric() || "._-".contains(ch)) {
            readable.push(ch);
            separator = false;
        } else {
            readable.push_str(&format!("~{:04X}", ch as u32));
            separator = false;
        }
    }
    let key: String = readable.trim_start_matches('-').chars().take(251).collect();
    format!("--{}--", if key.is_empty() { "root" } else { &key })
}

/// The last [`TAIL_BYTES`] of a session log, whatever encoding it uses. Shared
/// with the desktop app's session reader, which reads the same log shape.
pub fn read_session_tail(path: &Path) -> Option<String> {
    if path.extension().and_then(|v| v.to_str()) == Some("zstd") {
        return read_zstd_tail(path);
    }
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(size.saturating_sub(TAIL_BYTES as u64)))
        .ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The tail of a zstd-compressed log, decompressed as a stream so only the tail
/// is retained; a missing `zstd` binary makes this return `None` rather than
/// failing the scan.
fn read_zstd_tail(path: &Path) -> Option<String> {
    for command in ["zstd", "/opt/homebrew/bin/zstd", "/usr/local/bin/zstd"] {
        let Ok(mut child) = Command::new(command)
            .arg("-dc")
            .arg(path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let mut tail = VecDeque::with_capacity(TAIL_BYTES);
        let mut buffer = [0_u8; 8192];
        if let Some(mut stdout) = child.stdout.take() {
            while let Ok(count) = stdout.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                for byte in &buffer[..count] {
                    if tail.len() == TAIL_BYTES {
                        tail.pop_front();
                    }
                    tail.push_back(*byte);
                }
            }
        }
        if child.wait().ok().is_some_and(|status| status.success()) && !tail.is_empty() {
            return Some(
                String::from_utf8_lossy(&tail.into_iter().collect::<Vec<_>>()).into_owned(),
            );
        }
    }
    None
}

/// Whether an `llm/retry` record reports a spent retry budget.
///
/// The plugin retries a failed step automatically (`mode: "normal"` carries
/// `retry` and `maxRetries`); once the attempt count reaches the budget the step
/// cannot recover on its own, which is the failure the tray reports. A retry
/// below the budget is transient, and `step/start` / `turn/end` clear the
/// projection, so a recovered session stops reporting it.
pub fn retries_exhausted(data: &Value) -> bool {
    let (Some(retry), Some(max)) = (data["retry"].as_u64(), data["maxRetries"].as_u64()) else {
        return false;
    };
    max > 0 && retry >= max
}

/// The retry budget a record reports, when it reports one.
///
/// `mode: "always"` deliberately carries no budget: those retries never stop on
/// their own, so their absence is meaningful rather than a missing field.
pub fn retry_budget(data: &Value) -> Option<(u64, u64)> {
    if data["mode"].as_str() == Some("always") {
        return Some((1, 0));
    }
    Some((data["retry"].as_u64()?, data["maxRetries"].as_u64()?))
}

/// How one event type bears on the log-level state.
///
/// The categories are the *semantics* the state machine needs; the event names
/// are only the data contract that carries them. Keeping the mapping in one
/// table (rather than as `match` arms scattered through a loop) is what makes an
/// unlisted event visible: [`EventRole::Unknown`] is counted and reported by the
/// diagnostic instead of being silently skipped, which is how the earlier
/// mis-named failure events went unnoticed for so long.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventRole {
    /// Opens a turn.
    TurnStart,
    /// Closes a turn on its own; not evidence that failure was recovered.
    TurnEnd,
    /// The conversation moved on: the strongest generic evidence that whatever
    /// went wrong before it is no longer current.
    Progress,
    /// A budgeted retry that spent its budget.
    Failure,
    /// An approval prompt is open / answered.
    ApprovalAsked,
    ApprovalDecided,
    /// Nothing the log-level state depends on.
    Inert,
    /// A type this build does not know. Counted, never guessed at.
    Unknown,
}

/// The events whose arrival means the conversation moved past a failure.
///
/// Deliberately excludes the turn and step boundaries: a failed turn closes
/// itself, so treating `turn/end` as progress is what made an offline session
/// read as recovered.
pub fn event_role(event_type: &str) -> EventRole {
    match event_type {
        "turn/start" => EventRole::TurnStart,
        "turn/end" => EventRole::TurnEnd,
        // `assistant/attempt` is deliberately absent: it records that an attempt
        // was *started*, and the very attempt the retry plugin makes after
        // spending its budget emits one. Counting it cleared each failure
        // immediately after recording it, so an offline session still read as
        // recovered. Progress is substantive evidence instead — content
        // produced, an action taken, or something the user said.
        "assistant/message"
        | "assistant/live-chunk"
        | "tool/call"
        | "tool/result"
        | "user/message" => EventRole::Progress,
        "llm/retry" => EventRole::Failure,
        "approval/asked" => EventRole::ApprovalAsked,
        "approval/decided" => EventRole::ApprovalDecided,
        // The question pair is distinguished by the tool name, not the event
        // type, so both calls and results start as Inert and the caller marks
        // them by name.
        _ if is_known_event(event_type) => EventRole::Inert,
        _ => EventRole::Unknown,
    }
}

/// The event types this build recognizes enough to classify safely. Types
/// outside the list are reported as unknown rather than assumed harmless.
fn is_known_event(event_type: &str) -> bool {
    matches!(
        event_type,
        "agent-preset/selected"
            | "agent/inbox/spliced"
            | "approval/policy"
            | "command/done"
            | "command/run"
            | "compaction/end"
            | "compaction/prune"
            | "compaction/start"
            | "compaction/summary"
            | "deliverables/presented"
            | "developer/message"
            | "feedback/message-delete"
            | "feedback/message-put"
            | "feedback/record"
            | "goal/change"
            | "hook/invoked"
            | "hook/result"
            | "image/offload"
            | "llm/retry-started"
            | "model/selection"
            | "permission/preset"
            | "plan/mode"
            | "request/context"
            | "request/header"
            | "sandbox/mode"
            | "schedule/change"
            | "session-log-deepseek/delivery-accepted"
            | "session/end-seed"
            | "session/title"
            | "session/title-llm-request"
            | "step/end"
            | "step/start"
            | "subagent/catalog"
            | "subagent/descriptor"
            | "subagent/model-selection-policy"
            | "system/message"
            | "team/member"
            | "team/message/delivered"
            | "team/message/queued"
            | "team/task"
            | "todo/write"
            | "tool-workflow/agent-end"
            | "tool-workflow/agent-start"
            | "tool-workflow/run-end"
            | "tool-workflow/run-start"
            | "tool/ptc-dispatch"
            | "tool/ptc-dispatch-start"
            | "web/deepseek-search-llm-request"
            | "workspace/changes"
    )
}

/// The state a log tail carries, derived by walking the events in order.
///
/// The failure is a *position* in the log, not a latched flag: it is current
/// only while no later event shows the conversation moved on. That is what makes
/// the offline shape (retries spent, turn closed, nothing after) report a
/// failure while a recovered one does not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timeline {
    /// Sequence of the newest spent retry inside an open turn.
    failed_at: Option<u64>,
    /// Sequence of the newest event that moved the conversation on.
    progress_at: Option<u64>,
    turn_open: bool,
    unknown: u32,
    /// Retry records whose budget was unreadable, so whether they were spent
    /// could not be decided.
    unjudged: u32,
    /// The timestamp of the event currently being applied.
    pending_time: Option<u64>,
    /// Wall-clock start of the turn currently in flight, from the event's own
    /// `time`. This is what a row's duration measures: how long the current
    /// *run* has been going, not how old the conversation is.
    turn_started_at: Option<u64>,
    /// Wall-clock end of the most recent turn that closed, so a finished
    /// conversation can report how long its last run took instead of showing
    /// nothing.
    turn_ended_at: Option<u64>,
    /// Wall-clock start of the most recent turn that closed.
    last_turn_started_at: Option<u64>,
}

/// The events of a log tail, in order, for the state machine to consume.
pub fn session_events(text: &str) -> impl Iterator<Item = Value> + '_ {
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
}

impl Timeline {
    /// A turn opened; a failure recorded in an earlier turn cannot be current.
    pub fn turn_started(&mut self) {
        self.turn_started_at = self.pending_time;
        self.turn_open = true;
        self.failed_at = None;
    }

    /// A turn closed on its own. Deliberately *not* recovery: a failed turn
    /// closes itself, which is the shape an offline session leaves behind.
    pub fn turn_ended(&mut self) {
        if self.turn_open {
            self.last_turn_started_at = self.turn_started_at;
            self.turn_ended_at = self.pending_time;
        }
        self.turn_open = false;
    }

    /// The event's own timestamp, set before each role is dispatched. Events
    /// carry `time` in milliseconds; the log is the only place a turn's start
    /// is recorded.
    pub fn at(&mut self, millis: Option<u64>) {
        self.pending_time = millis;
    }

    /// How long the current run has lasted, or `None` when the log showed no
    /// turn at all. An open turn measures up to `now`; a closed one reports the
    /// span it actually took.
    pub fn run_duration(&self, now_millis: u64) -> Option<Duration> {
        if self.turn_open {
            let started = self.turn_started_at?;
            return Some(Duration::from_millis(now_millis.saturating_sub(started)));
        }
        let (started, ended) = (self.last_turn_started_at?, self.turn_ended_at?);
        Some(Duration::from_millis(ended.saturating_sub(started)))
    }

    /// The conversation moved on.
    pub fn progressed(&mut self, seq: u64) {
        self.progress_at = Some(seq);
    }

    /// Record a spent retry budget, when the payload says one was spent and a
    /// turn is open (the retry plugin only appends inside an open turn).
    ///
    /// A record whose budget cannot be read is counted instead of assumed
    /// harmless: a renamed `maxRetries` would otherwise turn every failure into
    /// a silent "still retrying".
    pub fn failed(&mut self, seq: u64, data: &Value) {
        if retry_budget(data).is_none() {
            self.unjudged += 1;
            return;
        }
        if self.turn_open && retries_exhausted(data) {
            self.failed_at = Some(seq);
        }
    }

    pub fn unjudged_retries(&self) -> u32 {
        self.unjudged
    }

    pub fn saw_unknown(&mut self) {
        self.unknown += 1;
    }

    pub fn unknown_events(&self) -> u32 {
        self.unknown
    }

    /// A failure is current while no event after it moved the conversation on.
    pub fn failure_current(&self) -> bool {
        match (self.failed_at, self.progress_at) {
            (None, _) => false,
            // Nothing has happened since the failure. This is the offline shape:
            // the retry budget is spent and the driver had nothing left to do.
            (Some(_), None) => true,
            (Some(failed), Some(progress)) => failed > progress,
        }
    }
}

fn parse_signals(text: &str) -> DeepSeekFacts {
    let mut facts = DeepSeekFacts::default();
    let mut approvals = HashSet::new();
    let mut questions = HashSet::new();
    let mut timeline = Timeline::default();
    let mut reply_requested = false;
    let mut last_text: Option<String> = None;
    let lines: Vec<_> = text.lines().rev().take(TAIL_LINES).collect();
    for line in lines.into_iter().rev() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let data = &event["data"];
        match event["type"].as_str() {
            Some("user/message") => {
                reply_requested = false;
                last_text = None;
                timeline.progressed(event["seq"].as_u64().unwrap_or_default());
            }
            Some("assistant/message") => {
                timeline.progressed(event["seq"].as_u64().unwrap_or_default());
                last_text = last_text_block(&data["message"]["content"]);
                if let Some(model) = data["message"]["source"]["model"].as_str() {
                    facts.model = Some(model.trim().into());
                }
            }
            Some("turn/start") => timeline.turn_started(),
            Some("turn/end") => {
                // Closing the turn is not recovery: an offline session spends
                // its retries, closes the turn and stops there, which is exactly
                // the failure worth reporting. Only later progress clears it.
                timeline.turn_ended();
                reply_requested = last_text
                    .take()
                    .is_some_and(|text| ends_with_question(&text));
            }
            Some("approval/asked") => {
                if let Some(id) = data["id"].as_str() {
                    approvals.insert(id.to_owned());
                }
            }
            Some("approval/decided") => {
                if let Some(id) = data["id"].as_str() {
                    approvals.remove(id);
                }
            }
            // An attempt that was started, not one that succeeded: the retry
            // plugin's final attempt emits this too, so it must not clear a
            // failure. It is inert for every other purpose as well.
            Some("assistant/attempt") => {}
            Some("tool/call") => {
                timeline.progressed(event["seq"].as_u64().unwrap_or_default());
                if data["name"] == "ask_user_question" {
                    if let Some(id) = data["callId"].as_str() {
                        questions.insert(id.to_owned());
                    }
                }
            }
            Some("tool/result") => {
                timeline.progressed(event["seq"].as_u64().unwrap_or_default());
                if let Some(id) = data["message"]["source"]["callId"].as_str() {
                    questions.remove(id);
                }
            }
            Some("llm/retry") => {
                // The session log carries no error event: `agent/error` is a
                // live-bus signal that is never written to it. A failed step
                // reaches the log only as an `llm/retry` record, so an exhausted
                // retry budget is what a failure looks like here. Whether it is
                // still current is decided by the events after it.
                timeline.failed(event["seq"].as_u64().unwrap_or_default(), &data);
            }

            _ => {}
        }
    }
    // Waiting states outrank a failure, matching the tray's own ordering: a
    // prompt that needs the user is actionable even if the step also failed.
    facts.state = if !questions.is_empty() || reply_requested {
        Some(AgentState::WaitingReply)
    } else if !approvals.is_empty() {
        Some(AgentState::Waiting)
    } else if timeline.failure_current() {
        Some(AgentState::Error)
    } else {
        None
    };
    facts
}

fn apply_projection(
    facts: &mut DeepSeekFacts,
    session_id: &str,
    home: &Path,
    session_modified: SystemTime,
) {
    let cache_path = home.join("storages/session_projcache.json");
    let Ok(file) = File::open(&cache_path) else {
        return;
    };
    let Ok(root) = serde_json::from_reader::<_, Value>(file) else {
        return;
    };
    let session = &root["tables"]["sessions"][session_id];
    let stats = &session["rows"]["sessionStats"]["val"];
    let pressure = &session["rows"]["contextPressure"]["val"];
    if let (Some(used_tokens), Some(window_tokens)) = (
        json_u64(&pressure["pressureTokens"]).or_else(|| json_u64(&pressure["surfaceTokens"])),
        json_u64(&pressure["contextWindow"]),
    ) {
        facts.context = Some(ContextUsage {
            used_tokens,
            window_tokens,
        });
    }
    if facts.state.is_none() {
        let pending = stats["pendingCalls"]
            .as_object()
            .is_some_and(|calls| !calls.is_empty());
        if json_truthy(&stats["openStep"]) || pending {
            facts.state = Some(AgentState::Working);
        } else {
            let cache_is_current = cache_path
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .is_some_and(|cache_modified| {
                    cache_modified + Duration::from_secs(1) >= session_modified
                });
            if cache_is_current {
                facts.state = Some(AgentState::Ready);
            }
        }
    }
}

fn json_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|number| number.is_finite() && *number > 0.0)
            .map(|number| number.round() as u64)
    })
}

fn json_truthy(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        _ => true,
    }
}

fn last_text_block(content: &Value) -> Option<String> {
    content
        .as_array()?
        .iter()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .last()
        .map(str::to_owned)
}

fn ends_with_question(text: &str) -> bool {
    matches!(text.trim_end_matches(|c: char| c.is_whitespace() || "\"'”’）)]】}。.!！*_`~～".contains(c)).chars().last(), Some('?' | '？'))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_deepseek_approval() {
        let facts = parse_signals("{\"type\":\"approval/asked\",\"data\":{\"id\":\"a\"}}\n");
        assert_eq!(facts.state, Some(AgentState::Waiting));
    }
    fn timeline_of(events: &[&str]) -> Timeline {
        let text: String = events.iter().map(|e| format!("{e}\n")).collect();
        let mut timeline = Timeline::default();
        for event in session_events(&text) {
            let data = &event["data"];
            let seq = event["seq"].as_u64().unwrap_or_default();
            match event_role(event["type"].as_str().unwrap_or_default()) {
                EventRole::TurnStart => timeline.turn_started(),
                EventRole::TurnEnd => timeline.turn_ended(),
                EventRole::Progress => timeline.progressed(seq),
                EventRole::Failure => timeline.failed(seq, data),
                EventRole::Unknown => timeline.saw_unknown(),
                _ => {}
            }
        }
        timeline
    }

    #[test]
    fn an_unknown_event_is_counted_and_changes_nothing() {
        // The whole point of classifying events: a type this build does not
        // know must be visible, not silently treated as progress or as a
        // failure. Its presence leaves the timing untouched either way.
        let known = [
            r#"{"seq":1,"type":"turn/start","data":{}}"#,
            r#"{"seq":2,"type":"llm/retry","data":{"retry":5,"maxRetries":5,"mode":"normal"}}"#,
        ];
        let with_unknown = [
            r#"{"seq":1,"type":"turn/start","data":{}}"#,
            r#"{"seq":2,"type":"llm/retry","data":{"retry":5,"maxRetries":5,"mode":"normal"}}"#,
            r#"{"seq":3,"type":"some/future-event","data":{"whatever":true}}"#,
            r#"{"seq":4,"type":"another/new-one","data":{}}"#,
        ];
        let baseline = timeline_of(&known);
        let extended = timeline_of(&with_unknown);
        assert!(baseline.failure_current());
        assert!(
            extended.failure_current(),
            "an unknown event must not clear a failure"
        );
        assert_eq!(extended.unknown_events(), 2);
        assert_eq!(baseline.unknown_events(), 0);
    }

    #[test]
    fn a_started_attempt_is_not_evidence_of_recovery() {
        // The exact live shape that defeated the previous rule: the retry
        // plugin spends its budget, then starts the attempt that fails, and
        // that attempt emits `assistant/attempt`. Counting it as progress
        // cleared the failure immediately after recording it.
        let events = [
            r#"{"seq":1,"type":"turn/start","data":{}}"#,
            r#"{"seq":2,"type":"step/start","data":{"turn":1,"step":1}}"#,
            r#"{"seq":3,"type":"user/message","data":{}}"#,
            r#"{"seq":4,"type":"assistant/attempt","data":{}}"#,
            r#"{"seq":5,"type":"llm/retry","data":{"retry":1,"maxRetries":5,"mode":"normal"}}"#,
            r#"{"seq":6,"type":"assistant/attempt","data":{}}"#,
            r#"{"seq":7,"type":"llm/retry","data":{"retry":5,"maxRetries":5,"mode":"normal"}}"#,
            r#"{"seq":8,"type":"assistant/attempt","data":{}}"#,
            r#"{"seq":9,"type":"step/end","data":{"turn":1,"step":1}}"#,
            r#"{"seq":10,"type":"turn/end","data":{"turn":1}}"#,
        ];
        assert!(
            timeline_of(&events).failure_current(),
            "an offline session must stay failed: attempt/step-end/turn-end are not recovery"
        );
        // Real content after it is recovery.
        let mut recovered = events.to_vec();
        recovered.push(r#"{"seq":11,"type":"assistant/message","data":{}}"#);
        assert!(!timeline_of(&recovered).failure_current());
    }

    #[test]
    fn failure_currency_follows_the_event_order() {
        let start = r#"{"seq":1,"type":"turn/start","data":{}}"#;
        let fail =
            r#"{"seq":2,"type":"llm/retry","data":{"retry":5,"maxRetries":5,"mode":"normal"}}"#;
        let close = r#"{"seq":3,"type":"turn/end","data":{}}"#;
        let progress = r#"{"seq":4,"type":"assistant/message","data":{}}"#;
        // Retrying inside the budget is not a failure at all.
        assert!(!timeline_of(&[
            start,
            r#"{"seq":2,"type":"llm/retry","data":{"retry":2,"maxRetries":5,"mode":"normal"}}"#
        ])
        .failure_current());
        // Spent budget, turn still open.
        assert!(timeline_of(&[start, fail]).failure_current());
        // The offline shape: spent budget, turn closed, nothing after it.
        assert!(timeline_of(&[start, fail, close]).failure_current());
        // Progress after the failure clears it, whichever side of the boundary.
        assert!(!timeline_of(&[start, fail, progress]).failure_current());
        assert!(!timeline_of(&[start, fail, close, progress]).failure_current());
        // A new turn cannot inherit the previous turn's failure.
        assert!(!timeline_of(&[start, fail, close, start]).failure_current());
    }

    #[test]
    fn a_spent_retry_budget_is_a_failure_in_the_cli_log_too() {
        // Both readers share this rule; the CLI log has no error event either.
        // A retry is only ever appended inside an open turn.
        let line = |retry: u64, max: &str| {
            format!(
                "{{\"type\":\"turn/start\",\"data\":{{\"turn\":1}}}}\n{{\"type\":\"step/start\",\"data\":{{\"turn\":1,\"step\":2}}}}\n{{\"type\":\"llm/retry\",\"data\":{{\"turn\":1,\"step\":2,\"mode\":\"normal\",\"retry\":{retry},\"maxRetries\":{max},\"failure\":{{\"message\":\"boom\"}}}}}}\n"
            )
        };
        assert_eq!(parse_signals(&line(1, "5")).state, None);
        assert_eq!(parse_signals(&line(5, "5")).state, Some(AgentState::Error));
        // Closing the turn is not recovery: an offline session ends exactly
        // there (retries spent, turn closed, nothing after it).
        let closed = format!(
            "{}{{\"type\":\"turn/end\",\"data\":{{\"turn\":1}}}}\n",
            line(5, "5")
        );
        assert_eq!(parse_signals(&closed).state, Some(AgentState::Error));
        // No budget to spend: not a terminal failure.
        let always = "{\"type\":\"turn/start\",\"data\":{\"turn\":1}}\n{\"type\":\"llm/retry\",\"data\":{\"mode\":\"always\",\"retry\":9}}\n";
        assert_eq!(parse_signals(always).state, None);
        // Evidence that the conversation moved on clears it.
        let after_progress = format!(
            "{}{{\"type\":\"assistant/message\",\"data\":{{}}}}\n",
            line(5, "5")
        );
        assert_eq!(parse_signals(&after_progress).state, None);
        assert!(retries_exhausted(
            &serde_json::json!({ "retry": 5, "maxRetries": 5 })
        ));
        assert!(!retries_exhausted(&serde_json::json!({ "retry": 5 })));
    }

    #[test]
    fn detects_question_at_turn_end() {
        let text = "{\"type\":\"assistant/message\",\"data\":{\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Continue?\"}],\"source\":{\"model\":\"deepseek-v3\"}}}}\n{\"type\":\"turn/end\",\"data\":{}}\n";
        let facts = parse_signals(text);
        assert_eq!(facts.state, Some(AgentState::WaitingReply));
        assert_eq!(facts.model.as_deref(), Some("deepseek-v3"));
    }
}
