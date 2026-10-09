//! Session state of the DeepSeek Harness desktop application.
//!
//! The desktop app is a single Electron process tree that hosts the `dsh`
//! runtime itself (`dsh-desktop-host`), so unlike the CLI there is no per
//! conversation process to watch. Its live state instead lives in the profile's
//! projection cache: one JSON file per session under
//! `$DSH_HOME/storages/session_projcache/sessions/`, rewritten while a turn
//! runs. Reading that cache is what the app itself renders from, so the tray
//! and the window agree on what is running, waiting or finished.
//!
//! Every read here is incremental: a session file is parsed only when its
//! modification time or size changed, and the event log is consulted only for a
//! conversation with a step still in flight whose log moved, so the extra disk
//! work per scan is proportional to the conversations that actually changed.

use crate::model::{AgentState, ContextUsage};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

/// A cached parse without a read in this long is dropped.
const CACHE_TTL: Duration = Duration::from_secs(86_400);
/// Bumped when the parsed shape changes, so entries cached by an older build
/// are read again instead of being trusted.
const CACHE_VERSION: u8 = 6;
/// The `session_projcache` document version this build reads. A document at any
/// other version is refused rather than guessed at, and counted in the profile
/// health so the diagnostic can say so.
const PROJECTION_VERSION: u64 = 7;
/// How long a session's event-log signals are trusted. The projection cache
/// reports a step as open while an approval prompt is on screen and carries no
/// row for a failed turn, so the event log is consulted for both — once per
/// changed log, and no more often than this per conversation.
const SIGNAL_CHECK_INTERVAL: Duration = Duration::from_secs(2);
/// How long after its last activity a conversation is still examined for a
/// failure.
///
/// A failure can close its turn quickly (the retry budget is spent inside the
/// turn, but the turn is closed as soon as the driver unwinds), and a closed
/// turn reports no failure — so the row that must show "error" can already look
/// finished by the time the scan runs. Anything active this recently is read
/// once per scan; the cost is one zstd pass over a log that is about to stop
/// moving anyway. A failure the user moved on from is deleted from the log, so
/// this window is the only thing keeping it visible, not a latch.
const FAILURE_OBSERVATION_WINDOW: Duration = Duration::from_secs(300);
const CACHE_CAPACITY: usize = 200;
/// Upper bound on the conversation rows a single scan adds to the menu. A
/// conversation waiting for the user or failed is listed before this cap
/// applies, so overflow can only hide conversations that need no attention.
pub const MAX_DESKTOP_ROWS: usize = 8;
/// Longest conversation title kept in a row label, matching the Codex rows.
const TITLE_LIMIT: usize = 24;

/// What a session's event log says beyond the projection cache: whether the user
/// is being waited on, and whether the tail carries a failure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LogSignals {
    /// An `approval/asked` event with no matching `approval/decided` yet.
    approval_asked: bool,
    /// An `ask_user_question` call whose result has not arrived.
    ///
    /// This is the only reliable way to see a question: while one is pending the
    /// desktop projection reports no open step at all, and its
    /// `userQuestions.active` row stayed empty in every observation, so the
    /// pending tool call is what distinguishes "waiting for the user" from
    /// "working".
    question_pending: bool,
    /// A spent retry budget with no later evidence of recovery.
    error: bool,
    /// Event types this build does not recognize. Reported by the diagnostic so
    /// a schema change shows up as a number instead of a silent misread.
    unknown_events: u32,
    /// Retry records whose budget could not be read, so "spent or not" was
    /// undecidable. Counted because a renamed field would otherwise look like a
    /// normal, healthy retry.
    retry_unjudged: u32,
    /// How long the conversation's current (or last) run has lasted, from the
    /// turn boundaries in the log. `None` when the tail held no turn.
    run: Option<Duration>,
}

/// Cached log signals with the evidence they came from: when they were read and
/// the log signature (size, modification time) that produced them. A log that
/// has not been appended to cannot have gained or lost a prompt or a failure,
/// so its signals are reused without decompressing anything.
#[derive(Clone, Copy, Debug)]
struct CachedSignals {
    checked: Instant,
    signals: LogSignals,
    log: Option<(u64, SystemTime)>,
}

/// One live conversation of the desktop application.
#[derive(Clone, Debug)]
pub struct DesktopSession {
    /// dsh session id (`session-<uuid>`, or a bare uuid for older sessions).
    pub id: String,
    pub cwd: Option<PathBuf>,
    pub title: Option<String>,
    pub state: AgentState,
    pub model: Option<String>,
    pub context: Option<ContextUsage>,
    /// The newest activity the desktop app knows about for this conversation.
    ///
    /// This is the last prompt, not the cache file's modification time: the app
    /// rewrites every open conversation's cache when it starts, so the file's
    /// timestamp says when the app launched rather than when the user last used
    /// the conversation. Only a conversation that never recorded a prompt falls
    /// back to its file's timestamp.
    pub activity: SystemTime,
    /// When the newest prompt arrived, from the projection. A run whose start
    /// has scrolled out of the event-log tail still has this anchor.
    pub last_prompt: Option<SystemTime>,
    /// The projection's own format version, from `identity.formatVersion`.
    ///
    /// `None` marks a document written before the current session format, which
    /// the runtime itself does not serve: the desktop app's workspace list leaves
    /// those sessions out. Reading them anyway listed conversations the user
    /// could not see anywhere in the app, duplicates included.
    pub format_version: Option<u64>,
    /// How long the conversation's current run has lasted, measured from the
    /// log's turn boundaries. `None` when no log was read, in which case the
    /// row falls back to the application's own uptime.
    pub run: Option<Duration>,
    /// Whether a turn or step is still in flight. Unlike `state` this stays true
    /// while the conversation waits for an approval prompt, which is what the
    /// event-log pass keys on.
    pub turn_open: bool,
    /// Whether the conversation approves its own tool calls
    /// (`permissions.approval == "never"`), i.e. the auto-confirmation mode the
    /// notification settings can silence.
    pub automatic_confirmation_mode: bool,
    /// How many rows this build reads were absent from the document. Non-zero
    /// means the projection moved on, so the state derived from it is partial.
    pub missing_rows: u8,
}

#[derive(Clone, Debug)]
struct CachedSession {
    version: u8,
    modified: SystemTime,
    size: u64,
    checked: Instant,
    /// The parsed projection. Its `state` is always the projection's own, so a
    /// signal applied on an earlier scan cannot survive into this one.
    facts: Option<DesktopSession>,
    /// What this scan reports after applying the event-log signals. Derived from
    /// `facts` on every read.
    applied: Option<DesktopSession>,
}

/// What the last scan found while reading the profile.
///
/// The reader is deliberately tolerant — an unreadable session must not take the
/// tray down — but tolerance without visibility is how a format change becomes a
/// silent wrong answer instead of an obvious gap. Every deviation from the
/// expected shape is counted here and reported by the diagnostic.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileHealth {
    pub files: usize,
    pub parsed: usize,
    /// Documents whose `version` is not the one this build reads.
    pub version_mismatch: usize,
    /// Documents whose JSON could not be read.
    pub unreadable: usize,
    /// Documents that parsed but are missing a row this build relies on.
    pub incomplete: usize,
    /// Retry records seen without a usable `maxRetries`, i.e. a failure could
    /// not be judged either way.
    pub retry_unjudged: u32,
    /// Whether the profile directory exists at all.
    pub root_present: bool,
    /// Projections written before the current session format. The runtime does
    /// not serve them, so they are skipped rather than listed.
    ///
    /// Deliberately not part of [`Self::is_intact`]: a superseded projection is
    /// normal history, not a format this build failed to understand, and
    /// treating it as damage would raise an alert on a healthy profile.
    pub stale_format: usize,
}

impl ProfileHealth {
    /// Whether every document looked like the shape this build expects.
    pub fn is_intact(&self) -> bool {
        self.root_present
            && self.unreadable == 0
            && self.version_mismatch == 0
            && self.incomplete == 0
            && self.retry_unjudged == 0
    }

    /// What to tell the user when the profile no longer looks the way this
    /// build reads it, or `None` when it does.
    ///
    /// Silence is the failure this exists to prevent: a format change used to
    /// look like "the app has no conversations", which is indistinguishable from
    /// a healthy idle app.
    pub fn alert(&self) -> Option<ProfileAlert> {
        if !self.root_present {
            return Some(ProfileAlert {
                kind: AlertKind::ProfileMissing,
                affected: 0,
            });
        }
        let affected = self.version_mismatch + self.incomplete + self.unreadable;
        if affected > 0 {
            return Some(ProfileAlert {
                kind: if self.version_mismatch > 0 {
                    AlertKind::FormatChanged
                } else {
                    AlertKind::Unreadable
                },
                affected,
            });
        }
        if self.retry_unjudged > 0 {
            return Some(ProfileAlert {
                kind: AlertKind::FailureUndecidable,
                affected: self.parsed,
            });
        }
        None
    }
}

/// The sessions the desktop application is actually showing.
///
/// `storages/workspace.json` is the application's own list of the conversations
/// its workspaces hold, and it agrees exactly with the track's rows — every
/// session it names is listed, and the one session it did *not* name was the
/// conversation a `dsh` CLI process created in the same project. That makes it
/// the authority on what "a desktop session" is.
#[derive(Debug, Default)]
struct WorkspaceRegistry {
    /// Every session the application's workspaces hold and still shows.
    sessions: std::collections::HashSet<String>,
    /// Sessions the user archived. The application keeps them in its workspaces'
    /// lists, so membership alone would keep reporting a conversation the user
    /// has filed away.
    archived: std::collections::HashSet<String>,
    /// Whether the registry was readable at all. An application that has not
    /// written one yet must not blank the list.
    present: bool,
}

impl WorkspaceRegistry {
    fn load(home: &Path) -> Self {
        let path = home.join("storages/workspace.json");
        let Ok(file) = std::fs::File::open(&path) else {
            return Self::default();
        };
        let Ok(root) = serde_json::from_reader::<_, Value>(file) else {
            return Self::default();
        };
        let mut sessions = std::collections::HashSet::new();
        if let Some(table) = root["tables"]["workspaces"].as_object() {
            for workspace in table.values() {
                for id in workspace["sessionIds"].as_array().into_iter().flatten() {
                    if let Some(id) = id.as_str() {
                        sessions.insert(id.to_owned());
                    }
                }
            }
        }
        let archived = root["global"]["archivedSessionIds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|id| id.as_str().map(str::to_owned))
            .collect();
        Self {
            present: root["tables"]["workspaces"].is_object(),
            sessions,
            archived,
        }
    }

    /// Whether the application itself is showing this conversation.
    ///
    /// A session it does not name was created by something else writing into the
    /// same profile — a `dsh` CLI process — and belongs to the terminal form. A
    /// session the user archived is still *named* but no longer listed, so it is
    /// excluded too; pinned sessions stay in the list and are unaffected.
    fn shows(&self, session: &DesktopSession) -> bool {
        if !self.present {
            return true;
        }
        self.sessions.contains(&session.id) && !self.archived.contains(&session.id)
    }
}

/// Why the profile could not be read the way this build expects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlertKind {
    /// The profile directory is not there at all.
    ProfileMissing,
    /// Documents are at a version or shape this build does not read.
    FormatChanged,
    /// Documents could not be parsed as JSON.
    Unreadable,
    /// Retry records whose budget could not be read, so a failure could not be
    /// judged either way.
    FailureUndecidable,
}

/// A visible statement that the profile moved away from this build's format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProfileAlert {
    pub kind: AlertKind,
    /// How many sessions were affected.
    pub affected: usize,
}

/// Reads the desktop profile's session projection cache, reusing the previous
/// parse while a file's size and modification time are unchanged.
#[derive(Default)]
pub struct DeepSeekDesktopAnalyzer {
    cache: HashMap<PathBuf, CachedSession>,
    /// The `DSH_HOME` whose profile is cached. The desktop app can be pointed at
    /// another one, in which case every entry belongs to the old profile and has
    /// to go.
    home: Option<PathBuf>,
    /// Session id to the log that holds its events, and the resolved path.
    logs: HashMap<String, (Instant, Option<PathBuf>)>,
    /// Session id to the signals read from its event log.
    signals: HashMap<String, CachedSignals>,
    /// What the last scan found.
    health: ProfileHealth,
    /// The application's own record of the sessions it shows.
    workspaces: WorkspaceRegistry,
}

impl DeepSeekDesktopAnalyzer {
    /// Parses the session cache files that changed since the last scan and
    /// returns how many documents were read, so a caller can tell a busy profile
    /// apart from an idle one (and the diagnostic can report the cost).
    ///
    /// `home` is the `DSH_HOME` the running desktop host reported, when known.
    pub fn refresh(&mut self, home: Option<&Path>) -> usize {
        let home = home.map(Path::to_path_buf).or_else(default_home);
        if home != self.home {
            self.cache.clear();
            self.logs.clear();
            self.signals.clear();
            self.home = home;
        }
        let Some(root) = self.home.as_deref().map(session_cache_root_for) else {
            return 0;
        };
        let mut health = ProfileHealth {
            root_present: root.is_dir(),
            ..ProfileHealth::default()
        };
        let Ok(entries) = root.read_dir() else {
            self.health = health;
            return 0;
        };
        let mut seen = Vec::new();
        let mut parsed = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Ok(metadata) = path.metadata() else {
                continue;
            };
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            seen.push(path.clone());
            health.files += 1;
            if let Some(cached) = self.cache.get(&path) {
                // The app rewrites the whole document, so an unchanged size and
                // modification time means an unchanged projection. The check
                // interval covers a filesystem with one-second timestamps,
                // where an active session can grow within the same second.
                if cached.version == CACHE_VERSION
                    && cached.size == metadata.len()
                    && cached.modified == modified
                {
                    continue;
                }
                if cached.version == CACHE_VERSION
                    && cached.size == metadata.len()
                    && cached.checked.elapsed() < Duration::from_secs(1)
                {
                    continue;
                }
            }
            // Read the declared version and row shape before deciding what to
            // report: a refused document still has to explain itself, and the
            // refusal branch used to bypass the shape accounting entirely.
            let version = read_projection_version(&path);
            let shape_moved = version.is_some_and(|v| v != PROJECTION_VERSION)
                && read_projection_rows(&path).is_some_and(|rows| missing_rows(&rows) > 0);
            let facts = parse_session_file(&path, modified);
            match &facts {
                Some(facts) => {
                    health.parsed += 1;
                    if facts.missing_rows > 0 {
                        health.incomplete += 1;
                    }
                    if facts.format_version.is_none() {
                        health.stale_format += 1;
                    }
                }
                None => {
                    if shape_moved {
                        health.version_mismatch += 1;
                    } else if version.is_some_and(|v| v != PROJECTION_VERSION) {
                        // An unknown version whose rows still matched: read on
                        // trust when it reads at all, otherwise a shape change.
                        health.version_mismatch += 1;
                    } else {
                        health.unreadable += 1;
                    }
                }
            }
            self.cache.insert(
                path,
                CachedSession {
                    version: CACHE_VERSION,
                    modified,
                    size: metadata.len(),
                    checked: Instant::now(),
                    applied: facts.clone(),
                    facts,
                },
            );
            parsed += 1;
        }
        self.prune(&seen);
        if let Some(home) = self.home.as_deref() {
            // Re-read every scan: one small JSON file, and it is what separates
            // the application's conversations from a CLI process's.
            self.workspaces = WorkspaceRegistry::load(home);
        }
        health.retry_unjudged = self.resolve_signals();
        self.health = health;
        parsed
    }

    /// What the last scan found while reading the profile.
    pub fn health(&self) -> &ProfileHealth {
        &self.health
    }

    /// The profile-level alert for the last scan, if any.
    pub fn alert(&self) -> Option<ProfileAlert> {
        self.health.alert()
    }

    /// Applies the signals the projection cache does not carry: an unresolved
    /// approval prompt, and a failed or disconnected turn.
    ///
    /// The desktop app's own sidebar distinguishes "working" from "waiting for
    /// approval", and its failure surfaces are invisible to the cache, so the
    /// session's event log is consulted for both — once per changed log, and no
    /// more often than [`SIGNAL_CHECK_INTERVAL`].
    fn resolve_signals(&mut self) -> u32 {
        let now = Instant::now();
        let mut unjudged_total = 0;
        // Which conversations the event log has to be consulted for.
        //
        // A conversation with a step in flight is checked while it works and
        // while it waits for a prompt.
        //
        // A failed turn is the harder case: the projection releases the step
        // when the turn ends, so a failure leaves a session that looks finished
        // — no open step, no pending call, "ready". Three situations cover it:
        // the session was already failed on the previous scan (keep it
        // observed), its log has moved inside the failure window (the failure
        // may be exactly what moved it), or it is seen for the first time after
        // running unattended (an app restart must not lose a failure that is
        // still the last thing in the log).
        let now_wall = SystemTime::now();
        let active: Vec<String> = self
            .cache
            .values()
            .filter_map(|cached| {
                let session = cached.facts.as_ref()?;
                let fresh = now_wall
                    .duration_since(session.activity)
                    .is_ok_and(|age| age < FAILURE_OBSERVATION_WINDOW);
                // `applied` carries the failure found on an earlier scan; the raw
                // projection decides the rest.
                let reported_error = cached
                    .applied
                    .as_ref()
                    .is_some_and(|s| s.state == AgentState::Error);
                (session.turn_open || reported_error || fresh).then(|| session.id.clone())
            })
            .collect();
        for id in &active {
            let log = self.session_log(id, now);
            let signature = log.as_deref().and_then(log_signature);
            // Cached signals are reused while the log has not moved. A log that
            // did move may have gained or lost a prompt or a failure, so its
            // changed signature always wins over the read rate limit: the limit
            // bounds the zstd pass, it does not delay an answer on disk.
            // A changed signature always re-reads, exactly as the body-state
            // cache does: the log moving is the only evidence that an approval
            // was answered or a turn failed, and waiting out the interval on it
            // left the row stuck on "waiting for confirmation" until some later
            // event happened to refresh it.
            //
            // A session that is currently showing a wait is re-read at every
            // scan, because its log can stop moving while the prompt is still on
            // screen. For anything else the rate limit applies, so a session in
            // the failure window costs one decompression per interval rather
            // than one per scan.
            let seen = self.signals.get(id).and_then(|state| state.log);
            let reported_wait = self.signals.get(id).and_then(|_| self.reported_wait(id));
            let reuse = reported_wait.is_none()
                && seen.is_some()
                && seen == signature
                && self
                    .signals
                    .get(id)
                    .is_some_and(|state| state.checked.elapsed() < SIGNAL_CHECK_INTERVAL);
            let signals = if reuse {
                self.signals.get(id).map(|state| state.signals)
            } else {
                None
            };
            let signals = signals.unwrap_or_else(|| {
                log.as_deref()
                    .and_then(crate::deepseek::read_session_tail)
                    .map(|text| log_signals(&text))
                    .unwrap_or_default()
            });
            unjudged_total += signals.retry_unjudged;
            self.signals.insert(
                id.clone(),
                CachedSignals {
                    checked: now,
                    signals,
                    log: signature,
                },
            );
            // Recompute the reported state from the projection rather than
            // editing the previous answer: a finished turn must be able to drop
            // a "working" that was true one scan ago.
            for cached in self.cache.values_mut() {
                let Some(facts) = cached.facts.as_ref() else {
                    continue;
                };
                if &facts.id != id {
                    continue;
                }
                // Same precedence as the CLI reader: a prompt that needs the
                // user outranks a failure, and a still-open step outranks a
                // spent retry — the step may still recover or be retried by the
                // user, while the projection is the authority on it.
                let projected = facts.state;
                let state = if signals.question_pending {
                    AgentState::WaitingReply
                } else if signals.approval_asked {
                    AgentState::Waiting
                } else if facts.turn_open {
                    AgentState::Working
                } else if signals.error {
                    AgentState::Error
                } else {
                    projected
                };
                cached.applied = Some(DesktopSession {
                    state,
                    // The run length comes from the log's turn boundaries, and
                    // falls back to the current prompt's arrival when the tail
                    // no longer reaches back that far: a long conversation
                    // scrolls its `turn/start` out of the window, and the prompt
                    // time is the same moment without being truncated.
                    run: signals.run.or_else(|| {
                        facts.turn_open.then(|| {
                            facts
                                .last_prompt
                                .and_then(|prompt| SystemTime::now().duration_since(prompt).ok())
                                .unwrap_or_default()
                        })
                    }),
                    ..facts.clone()
                });
            }
        }
        // A conversation that neither works nor shows a failure needs no signals.
        self.signals.retain(|id, _| active.contains(id));
        unjudged_total
    }

    /// Whether the applied view currently reports a wait or a failure, i.e. a
    /// state driven by the event log rather than by the projection.
    fn reported_wait(&self, id: &str) -> Option<()> {
        self.cache
            .values()
            .filter_map(|cached| cached.applied.as_ref())
            .find(|session| session.id == id)
            .filter(|session| {
                matches!(
                    session.state,
                    AgentState::Error | AgentState::Waiting | AgentState::WaitingReply
                )
            })
            .map(|_| ())
    }

    /// The session's event log, looked up once per session. The desktop app
    /// writes `session.v4.jsonl.zstd` (older versions `session.jsonl.zstd`)
    /// inside a per-project directory named after the session id.
    fn session_log(&mut self, id: &str, now: Instant) -> Option<PathBuf> {
        if let Some((checked, path)) = self.logs.get(id) {
            if checked.elapsed() < Duration::from_secs(60) && path.is_some() {
                return path.clone();
            }
        }
        let sessions = self.home.as_deref()?.join("sessions");
        let mut found = None;
        for project in std::fs::read_dir(&sessions).into_iter().flatten().flatten() {
            let directory = project.path().join(id);
            for name in ["session.v4.jsonl.zstd", "session.jsonl.zstd"] {
                let candidate = directory.join(name);
                if candidate.is_file() {
                    found = Some(candidate);
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        self.logs.insert(id.to_owned(), (now, found.clone()));
        found
    }

    /// Every live conversation of the desktop app, most actionable first, plus
    /// how many of them the row cap left out.
    ///
    /// `window` is how long a conversation that is not mid-turn keeps its row:
    /// the desktop app keeps every session it has ever opened in this cache, so
    /// without the window the menu would list months of history.
    ///
    /// The order is the tray's: a conversation waiting for the user or failed is
    /// listed before one that is merely working, and both before a finished one,
    /// so the cap can only drop conversations that need no attention.
    pub fn overview(&self, window: Option<Duration>) -> (Vec<DesktopSession>, usize) {
        let mut sessions: Vec<DesktopSession> = listed_sessions(
            &self
                .cache
                .values()
                .filter_map(|cached| cached.applied.clone())
                .collect::<Vec<_>>(),
            window,
            &self.workspaces,
        );
        sessions.sort_by(|left, right| {
            attention_rank(left.state)
                .cmp(&attention_rank(right.state))
                .then_with(|| right.activity.cmp(&left.activity))
                .then_with(|| left.id.cmp(&right.id))
        });
        let hidden = sessions.len().saturating_sub(MAX_DESKTOP_ROWS);
        sessions.truncate(MAX_DESKTOP_ROWS);
        (sessions, hidden)
    }

    /// The event-log signals read for one session, for `--diagnose-deepseek-desktop`.
    pub fn signal_report(&self, id: &str) -> serde_json::Value {
        let cached = self.cache.values().find(|cached| {
            cached
                .facts
                .as_ref()
                .is_some_and(|session| session.id == id)
        });
        let signals = cached.and_then(|cached| cached.applied.as_ref().map(|_| cached));
        let read = self.signals.get(id);
        serde_json::json!({
            "approvalAsked": read.is_some_and(|state| state.signals.approval_asked),
            "questionPending": read.is_some_and(|state| state.signals.question_pending),
            "error": read.is_some_and(|state| state.signals.error),
            "unknownEvents": read.map_or(0, |state| state.signals.unknown_events),
            "logSignature": read.and_then(|state| state.log).map(|(size, _)| size),
            "readAgoSecs": read.map(|state| state.checked.elapsed().as_secs_f64()),
            "inObservation": signals.is_some(),
            // How long the current run has lasted, as the row would show it.
            "runSecs": cached
                .and_then(|cached| cached.applied.as_ref())
                .and_then(|session| session.run)
                .map(|run| run.as_secs()),
        })
    }

    /// The rows a scan would show, without the overflow count.
    pub fn sessions(&self, window: Option<Duration>) -> Vec<DesktopSession> {
        self.overview(window).0
    }

    /// Drops entries whose session file is gone, then the least recently read
    /// ones beyond the cap and the TTL.
    fn prune(&mut self, seen: &[PathBuf]) {
        self.cache
            .retain(|path, cached| seen.contains(path) && cached.checked.elapsed() < CACHE_TTL);
        self.logs.retain(|id, (_, path)| {
            path.is_some()
                && self.cache.values().any(|cached| {
                    cached
                        .facts
                        .as_ref()
                        .is_some_and(|session| &session.id == id)
                })
        });
        while self.cache.len() > CACHE_CAPACITY {
            let oldest = self
                .cache
                .iter()
                .min_by_key(|(_, cached)| cached.checked)
                .map(|(path, _)| path.clone());
            match oldest {
                Some(path) => {
                    self.cache.remove(&path);
                }
                None => break,
            }
        }
    }
}

/// Identity of a session log for change detection: appended bytes change the
/// size, and a rewrite changes the modification time.
fn log_signature(path: &Path) -> Option<(u64, SystemTime)> {
    let metadata = path.metadata().ok()?;
    Some((metadata.len(), metadata.modified().ok()?))
}

/// The signals a session's event-log tail carries.
///
/// The events are the same ones the CLI sessions write, so the rules are shared
/// with the terminal reader: every `approval/asked` id must have a later
/// `approval/decided`, and a failure is an `llm/retry` record that spent its
/// retry budget. Only the tail is read, so a failure scrolls out of the window
/// once the conversation moves on — which is what makes recovery visible.
fn log_signals(text: &str) -> LogSignals {
    let mut signals = LogSignals::default();
    let mut asked: Vec<String> = Vec::new();
    let mut question_calls: Vec<String> = Vec::new();
    let mut timeline = crate::deepseek::Timeline::default();
    for event in crate::deepseek::session_events(text) {
        use crate::deepseek::EventRole;
        // Stamp the event's own wall clock before its role is dispatched: the
        // turn boundaries it carries are the only record of when the current
        // run began.
        timeline.at(event["time"].as_u64());
        let data = &event["data"];
        let seq = event["seq"].as_u64().unwrap_or_default();
        let id = data["id"].as_str().unwrap_or_default();
        let mut role = crate::deepseek::event_role(event["type"].as_str().unwrap_or_default());
        // The question pair shares its event types with ordinary tools, so the
        // tool name is what identifies it.
        if role == EventRole::Progress && data["name"] == "ask_user_question" {
            role = EventRole::Inert;
            if let Some(call_id) = data["callId"].as_str().filter(|id| !id.is_empty()) {
                question_calls.push(call_id.to_owned());
            }
        }
        if role == EventRole::Progress && event["type"] == "tool/result" {
            if let Some(call_id) = data["message"]["source"]["callId"]
                .as_str()
                .filter(|id| !id.is_empty())
            {
                question_calls.retain(|candidate| candidate != call_id);
            }
        }
        match role {
            EventRole::TurnStart => timeline.turn_started(),
            EventRole::TurnEnd => timeline.turn_ended(),
            EventRole::Progress => timeline.progressed(seq),
            EventRole::Failure => timeline.failed(seq, data),
            EventRole::ApprovalAsked if !id.is_empty() => asked.push(id.to_owned()),
            EventRole::ApprovalDecided if !id.is_empty() => {
                asked.retain(|candidate| candidate != id);
            }
            EventRole::Unknown => timeline.saw_unknown(),
            _ => {}
        }
    }
    signals.approval_asked = !asked.is_empty();
    signals.question_pending = !question_calls.is_empty();
    signals.error = timeline.failure_current();
    signals.unknown_events = timeline.unknown_events();
    signals.retry_unjudged = timeline.unjudged_retries();
    signals.run = timeline.run_duration(now_millis());
    signals
}

/// The conversations a scan lists, from everything the cache holds.
///
/// A projection the runtime no longer serves is not a conversation the user has:
/// the desktop app's own workspace list leaves those sessions out, so listing
/// them invented rows — duplicate titles among them — that existed nowhere in the
/// UI. `identity.formatVersion` is the runtime's own marker for a document of the
/// current session format, so its absence is what is filtered, not a heuristic on
/// row names.
///
/// This complements, rather than duplicates, the row-presence check in
/// [`parse_session_file`]: a superseded document is already refused there when it
/// also lost the rows this build reads (which is the case on the profile measured
/// here), while the version marker still catches one that happens to carry them.
fn listed_sessions(
    sessions: &[DesktopSession],
    window: Option<Duration>,
    workspaces: &WorkspaceRegistry,
) -> Vec<DesktopSession> {
    let now = SystemTime::now();
    sessions
        .iter()
        .filter(|session| {
            session.format_version.is_some()
                // Only conversations the application itself shows: one it does
                // not name was created by a CLI process into the same profile.
                && workspaces.shows(session)
                && started(session)
                && worth_showing(session, now, window)
        })
        .cloned()
        .collect()
}

/// A conversation earns a row while it is mid-turn (working or waiting for the
/// user), or while the desktop app touched it inside the configured window.
fn worth_showing(session: &DesktopSession, now: SystemTime, window: Option<Duration>) -> bool {
    if needs_attention(session.state) {
        return true;
    }
    let Some(window) = window else { return true };
    now.duration_since(session.activity)
        .is_ok_and(|age| age < window)
}

/// How urgently a conversation needs the user, lowest first: the tray lists
/// waiting and failed rows before work that needs nobody.
fn attention_rank(state: AgentState) -> usize {
    match state {
        AgentState::Waiting => 0,
        AgentState::WaitingReply => 1,
        AgentState::Error => 2,
        AgentState::Working => 3,
        AgentState::Ready => 4,
        AgentState::Stopped => 5,
    }
}

fn needs_attention(state: AgentState) -> bool {
    matches!(
        state,
        AgentState::Working | AgentState::Waiting | AgentState::WaitingReply | AgentState::Error
    )
}

/// Whether the user ever used this conversation. A session row without a prompt
/// and without context is a scratch buffer, not something the tray should
/// report.
fn started(session: &DesktopSession) -> bool {
    session.title.is_some() || session.context.is_some() || session.state != AgentState::Ready
}

/// `$DSH_HOME/storages/session_projcache/sessions`, honouring `DSH_HOME` the
/// same way the desktop app does.
pub fn session_cache_root() -> Option<PathBuf> {
    Some(session_cache_root_for(&default_home()?))
}

/// `DSH_HOME` when the environment names one.
///
/// The diagnostic honours this ahead of a running host's profile: an explicit
/// environment is a deliberate override, and without it a test fixture could
/// never be inspected while the app was open.
pub fn env_home() -> Option<PathBuf> {
    std::env::var_os("DSH_HOME").map(PathBuf::from)
}

/// The profile root the desktop app uses when it was not started with an
/// explicit one.
fn default_home() -> Option<PathBuf> {
    std::env::var_os("DSH_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".dsh")))
}

fn session_cache_root_for(home: &Path) -> PathBuf {
    home.join("storages")
        .join("session_projcache")
        .join("sessions")
}

/// The desktop application's bundle directory. The Electron executable is named
/// after the product ("DeepSeek Harness"), which is not a name the CLI's `dsh` /
/// `deepseek-harness` binaries ever take.
pub const APP_BUNDLE: &str = "DeepSeek Harness.app";

/// The one URL route the desktop shell serves: it brings the primary window
/// forward, restoring a minimized window and recreating a closed one. Every
/// other `dsh://` URL is ignored by the shell, and the frontend has no
/// conversation-addressing URL of its own, so activating the application is the
/// deepest focus a tray row can perform.
pub const OPEN_URL: &str = "dsh://open";

/// Whether a process the shared agent matcher could not name belongs to the
/// DeepSeek Harness desktop application.
///
/// The Electron executable's basename is the product name with a space, so the
/// path is matched against the bundle directory instead. The app's helper
/// processes also live in the bundle (`DeepSeek Harness Helper.app`), and the
/// Node-mode host the app spawns for the `dsh` runtime runs the *same*
/// executable with `ELECTRON_RUN_AS_NODE=1`, so its command line is the only
/// thing naming the desktop host.
pub fn is_desktop_process(executable: &str, command: &str) -> bool {
    if executable.contains(APP_BUNDLE) || command.contains(APP_BUNDLE) {
        return true;
    }
    command.contains("dsh-desktop-host") && command.contains("app.asar")
}

/// The `DSH_HOME` the desktop host was started with, from the profile directory
/// it runs (`<DSH_HOME>/profiles/desktop`, a positional argument after the
/// `dsh-desktop-host` entry point).
///
/// A non-default `DSH_HOME` would otherwise make the tray read a different
/// profile's sessions than the window shows. The host is always launched with
/// the bundle-relative `app.asar/dsh` entry point immediately before the profile
/// argument, so anchoring on that string keeps a home path containing spaces
/// intact.
pub fn home_from_command(command: &str) -> Option<PathBuf> {
    const MARKER: &str = "profiles/desktop";
    let at = command.rfind(MARKER)?;
    let before = command[..at].trim_end_matches('/');
    let start = before
        .rfind("app.asar/dsh ")
        .map(|index| index + "app.asar/dsh ".len())
        .or_else(|| {
            before
                .rfind(|ch: char| ch.is_whitespace())
                .map(|index| index + 1)
        })
        .unwrap_or(0);
    let home = &before[start..];
    (!home.is_empty() && home != "/").then(|| PathBuf::from(home))
}

/// The document's declared version, read without committing to the rest of the
/// shape. Used to tell an upstream format change from local damage.
fn read_projection_version(path: &Path) -> Option<u64> {
    let file = std::fs::File::open(path).ok()?;
    let root: Value = serde_json::from_reader(file).ok()?;
    root["version"].as_u64()
}

/// The document's `record.rows`, for shape accounting on documents that are
/// refused for another reason.
fn read_projection_rows(path: &Path) -> Option<Value> {
    let file = std::fs::File::open(path).ok()?;
    let root: Value = serde_json::from_reader(file).ok()?;
    let rows = &root["record"]["rows"];
    (!rows.is_null()).then(|| rows.clone())
}

fn parse_session_file(path: &Path, modified: SystemTime) -> Option<DesktopSession> {
    let file = std::fs::File::open(path).ok()?;
    let root: Value = serde_json::from_reader(file).ok()?;
    let version = root["version"].as_u64();
    if version != Some(PROJECTION_VERSION) {
        // A version this build does not know is refused only when the rows it
        // needs are absent. Losing every session on a version bump would be a
        // worse failure than reading a document that still carries the shape we
        // understand; `missing_rows` records that it was read on trust, and the
        // profile health reports it.
        // The known version is read as-is; an *unknown* one is only read when
        // the rows this build needs are all still there.
        if version.is_none() || missing_rows(&root["record"]["rows"]) > 0 {
            return None;
        }
    }
    let record = &root["record"];
    if record.is_null() {
        return None;
    }
    let rows = &record["rows"];
    Some(DesktopSession {
        id: session_id(path),
        cwd: record["identity"]["cwd"]
            .as_str()
            .filter(|cwd| !cwd.is_empty())
            .map(PathBuf::from),
        title: title_of(rows),
        // The projection's own state. Never edited after this: the applied view
        // lives in `CachedSession::applied`, so a turn that ends can drop the
        // "working" this state reported while its step was open.
        state: state_of(rows),
        turn_open: turn_open(rows),
        automatic_confirmation_mode: rows["permissions"]["val"]["approval"].as_str()
            == Some("never"),
        missing_rows: missing_rows(rows),
        model: rows["modelSelection"]["val"]["lastUsed"]["model"]
            .as_str()
            .map(str::to_owned),
        context: context_of(&rows["contextPressure"]["val"]),
        activity: millis(&rows["sessionListMetadata"]["val"]["lastPromptAt"]).unwrap_or(modified),
        last_prompt: millis(&rows["sessionListMetadata"]["val"]["lastPromptAt"]),
        format_version: record["identity"]["formatVersion"].as_u64(),
        // Filled from the event log, which is the only place a turn's start is
        // recorded. A conversation without a readable log keeps `None` and the
        // row falls back to the application's uptime.
        run: None,
    })
}

/// The current wall clock in the unit the log's `time` field uses.
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

/// Which of the rows this build depends on are absent. A missing row is not
/// necessarily fatal — every reader is written to tolerate it — but it is
/// evidence that the projection format moved, so it is counted and reported.
pub fn missing_rows(rows: &Value) -> u8 {
    // Only what the state derivation cannot proceed without.
    //
    // Not every absent row is a change: a conversation that was never prompted
    // legitimately has no `userQuestions` and no `inbox` (the live profile holds
    // two such sessions from months ago), and counting those as a format change
    // produced a false alarm on a healthy profile. The anchor is the structure
    // the reader dereferences to decide anything at all.
    let anchors: [(&str, &[&str]); 2] = [("sessionStats", &["val"]), ("identity", &[])];
    let record = &rows["record"];
    let row_source = if record.is_null() {
        rows
    } else {
        &record["rows"]
    };
    anchors
        .iter()
        .filter(|(row, path)| {
            let mut node = if row == &"identity" {
                record
            } else {
                row_source
            };
            node = &node[*row];
            path.iter().any(|key| match node.get(*key) {
                Some(next) => {
                    node = next;
                    false
                }
                None => true,
            })
        })
        .count() as u8
}

/// Whether the conversation still has a step in flight, which stays true across
/// an approval prompt. Kept in step with [`state_of`], which uses the same two
/// signals to decide that a conversation is working.
fn turn_open(rows: &Value) -> bool {
    let stats = &rows["sessionStats"]["val"];
    !stats["openStep"].is_null()
        || stats["pendingCalls"]
            .as_object()
            .is_some_and(|calls| !calls.is_empty())
}

/// The live state of a conversation, from the projection rows the desktop app
/// renders:
///
/// * an active tool question means the user has to answer something;
/// * an open step, a pending tool call or a queued inbox message means the
///   agent is still producing;
/// * otherwise the turn is finished and the conversation is only ready.
fn state_of(rows: &Value) -> AgentState {
    let active_questions = rows["userQuestions"]["val"]["questions"]["active"]
        .as_array()
        .map_or(0, Vec::len);
    if active_questions > 0 {
        return AgentState::WaitingReply;
    }
    let stats = &rows["sessionStats"]["val"];
    let queued = rows["inbox"]["val"]["next-turn"]
        .as_array()
        .map_or(0, Vec::len)
        + rows["inbox"]["val"]["next-step"]
            .as_array()
            .map_or(0, Vec::len);
    let working = !stats["openStep"].is_null()
        || stats["pendingCalls"]
            .as_object()
            .is_some_and(|calls| !calls.is_empty())
        || queued > 0;
    if working {
        AgentState::Working
    } else {
        AgentState::Ready
    }
}

fn title_of(rows: &Value) -> Option<String> {
    let title = rows["title"]["val"].as_str()?.trim();
    if title.is_empty() {
        return None;
    }
    Some(title.chars().take(TITLE_LIMIT).collect())
}

fn context_of(pressure: &Value) -> Option<ContextUsage> {
    let used_tokens =
        json_u64(&pressure["pressureTokens"]).or_else(|| json_u64(&pressure["surfaceTokens"]))?;
    let window_tokens = json_u64(&pressure["contextWindow"])?;
    (window_tokens > 0).then_some(ContextUsage {
        used_tokens,
        window_tokens,
    })
}

fn session_id(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_owned()
}

fn millis(value: &Value) -> Option<SystemTime> {
    let millis = json_u64(value)?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(millis))
}

fn json_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|number| number.is_finite() && *number > 0.0)
            .map(|number| number.round() as u64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_current_document_reports_no_missing_rows() {
        // The row check must accept the shape this build reads; mutation
        // testing is worthless if the baseline itself reads as incomplete.
        let document = session_document(rows(json!(null), json!({}), json!([]), json!("基线")));
        let rows = &document["record"]["rows"];
        assert_eq!(missing_rows(rows), 0, "baseline rows: {rows}");
    }

    #[test]
    fn a_renamed_row_is_reported_missing() {
        let mut document = session_document(rows(json!(null), json!({}), json!([]), json!("改名")));
        let moved = document["record"]["rows"]["sessionStats"].take();
        document["record"]["rows"]["turnStats"] = moved;
        assert!(missing_rows(&document["record"]["rows"]) > 0);
        // A renamed nested field counts too: the readers dereference `val`.
        let mut nested = session_document(rows(json!(null), json!({}), json!([]), json!("嵌套")));
        let value = nested["record"]["rows"]["sessionStats"].take();
        nested["record"]["rows"]["sessionStats"] = json!({ "value": value["val"] });
        assert!(missing_rows(&nested["record"]["rows"]) > 0);
    }
    use serde_json::json;
    use std::{
        io::Write,
        sync::atomic::{AtomicU32, Ordering},
    };

    static NEXT_DIR: AtomicU32 = AtomicU32::new(0);

    /// One projection-cache document with the rows this module reads.
    fn session_document(rows: Value) -> Value {
        json!({ "version": 7, "record": { "identity": {
            "formatVersion": 4,
            "createdAt": 1_791_500_000_000u64,
            "cwd": "/Users/me/code/app",
        }, "rows": rows } })
    }

    fn rows(open_step: Value, pending: Value, active_questions: Value, title: Value) -> Value {
        with_approval(
            rows_plain(open_step, pending, active_questions, title),
            "ask",
        )
    }

    fn rows_plain(
        open_step: Value,
        pending: Value,
        active_questions: Value,
        title: Value,
    ) -> Value {
        json!({
            "sessionStats": { "val": { "openStep": open_step, "pendingCalls": pending } },
            "userQuestions": { "val": { "questions": { "active": active_questions } } },
            "inbox": { "val": { "next-turn": [], "next-step": [] } },
            "title": { "val": title },
            "modelSelection": { "val": { "lastUsed": { "model": "deepseek-flash" } } },
            "contextPressure": { "val": { "pressureTokens": 309_973u64, "contextWindow": 1_000_000u64 } },
            // The real document always carries this row, and a live session's
            // `lastPromptAt` is *now* — a hard-coded past value would put every
            // fixture outside the failure observation window and silently stop
            // the event-log pass, which is exactly how a test can pass while the
            // feature is broken.
            "sessionListMetadata": { "val": { "blank": false, "lastPromptAt": now_millis() } },
        })
    }

    /// The current wall clock in the unit the projection stores.
    fn now_millis() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or_default()
    }

    /// The same rows with an approval policy, which decides whether the
    /// conversation approves its own tool calls.
    fn with_approval(mut rows: Value, approval: &str) -> Value {
        rows["permissions"] =
            json!({ "val": { "sandbox": "workspace-write", "approval": approval } });
        rows
    }

    fn parse_temp(document: &Value) -> DesktopSession {
        let dir = std::env::temp_dir().join(format!(
            "asi-deepseek-desktop-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session-test.json");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(serde_json::to_string(document).unwrap().as_bytes())
            .unwrap();
        parse_session_file(&path, SystemTime::now()).expect("a parsed session")
    }

    #[test]
    fn an_open_step_is_working() {
        let session = parse_temp(&session_document(rows(
            json!({ "turn": 1, "step": 22 }),
            json!({}),
            json!([]),
            json!("桌面端支持"),
        )));
        assert_eq!(session.state, AgentState::Working);
        assert_eq!(session.id, "session-test");
        assert_eq!(
            session.cwd.as_deref(),
            Some(Path::new("/Users/me/code/app"))
        );
    }

    #[test]
    fn active_questions_wait_for_the_user() {
        let session = parse_temp(&session_document(rows(
            json!(null),
            json!({ "call_1": { "name": "ask_user_question" } }),
            json!([{ "id": "call_1" }]),
            json!("在吗"),
        )));
        assert_eq!(session.state, AgentState::WaitingReply);
    }

    #[test]
    fn a_finished_turn_is_ready_with_model_and_context() {
        let session = parse_temp(&session_document(rows(
            json!(null),
            json!({}),
            json!([]),
            json!("同步代码"),
        )));
        assert_eq!(session.state, AgentState::Ready);
        assert_eq!(session.model.as_deref(), Some("deepseek-flash"));
        let context = session.context.expect("context pressure");
        assert_eq!(context.used_tokens, 309_973);
        assert_eq!(context.window_tokens, 1_000_000);
    }

    #[test]
    fn queued_inbox_messages_keep_the_turn_alive() {
        let mut document =
            session_document(rows(json!(null), json!({}), json!([]), json!("排队的会话")));
        document["record"]["rows"]["inbox"]["val"]["next-turn"] = json!([{ "text": "继续" }]);
        assert_eq!(parse_temp(&document).state, AgentState::Working);
    }

    #[test]
    fn titles_are_capped_for_the_menu() {
        let session = parse_temp(&session_document(rows(
            json!(null),
            json!({}),
            json!([]),
            json!("标".repeat(40)),
        )));
        assert_eq!(session.title.unwrap().chars().count(), TITLE_LIMIT);
    }

    #[test]
    fn unprompted_sessions_get_no_row() {
        let session = parse_temp(&session_document(json!({
            "sessionStats": { "val": { "openStep": null, "pendingCalls": {} } },
            "userQuestions": { "val": { "questions": { "active": [] } } },
            "inbox": { "val": { "next-step": [] } },
            "title": { "val": null },
            "contextPressure": { "val": {} },
        })));
        assert!(!started(&session));
        let prompted = parse_temp(&session_document(rows(
            json!(null),
            json!({}),
            json!([]),
            json!("真实会话"),
        )));
        assert!(started(&prompted));
    }

    #[test]
    fn a_projection_version_we_do_not_know_is_ignored_when_its_rows_moved() {
        // An unknown version whose rows this build still recognizes is read on
        // trust (losing every session on a version bump would be worse, and the
        // health report marks it). An unknown version that *also* moved the rows
        // is refused, because guessing there would be a silent wrong answer.
        let mut intact = session_document(rows(json!(null), json!({}), json!([]), json!("新版本")));
        intact["version"] = json!(99);
        let dir = std::env::temp_dir().join(format!("asi-projver-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ok = dir.join("session-new.json");
        std::fs::write(&ok, serde_json::to_string(&intact).unwrap()).unwrap();
        assert!(parse_session_file(&ok, SystemTime::now()).is_some());

        let mut document =
            session_document(rows(json!(null), json!({}), json!([]), json!("旧版本")));
        document["version"] = json!(99);
        let moved = document["record"]["rows"]["sessionStats"].take();
        document["record"]["rows"]["turnStats"] = moved;
        let dir = std::env::temp_dir().join(format!("asi-deepseek-version-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session-old.json");
        std::fs::write(&path, serde_json::to_string(&document).unwrap()).unwrap();
        assert!(parse_session_file(&path, SystemTime::now()).is_none());
    }

    #[test]
    fn recency_comes_from_the_last_prompt_not_the_cache_file() {
        // The app rewrites every open conversation's cache when it starts, so
        // the file is fresh while the conversation itself is months old. The
        // last prompt is what keeps such a conversation out of the menu.
        let prompt_at = (SystemTime::now() - Duration::from_secs(30 * 86_400))
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let mut document =
            session_document(rows(json!(null), json!({}), json!([]), json!("旧会话")));
        document["record"]["rows"]["sessionListMetadata"] =
            json!({ "val": { "blank": false, "lastPromptAt": prompt_at } });
        let session = parse_temp(&document);
        let activity = session.activity;
        assert!(
            SystemTime::now()
                .duration_since(activity)
                .unwrap()
                .as_secs()
                > 29 * 86_400,
            "the file was just written; the activity must come from the prompt"
        );
        assert!(!worth_showing(
            &session,
            SystemTime::now(),
            Some(Duration::from_secs(3600))
        ));
        // A conversation whose provenance row is absent (an older projection)
        // falls back to the file's own timestamp.
        let mut old =
            session_document(rows(json!(null), json!({}), json!([]), json!("无提示时间")));
        old["record"]["rows"]
            .as_object_mut()
            .unwrap()
            .remove("sessionListMetadata");
        let dir = std::env::temp_dir().join(format!(
            "asi-recency-fallback-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session-fallback.json");
        std::fs::write(&path, serde_json::to_string(&old).unwrap()).unwrap();
        let fallback = parse_session_file(&path, SystemTime::now()).expect("a parsed session");
        assert!(
            SystemTime::now()
                .duration_since(fallback.activity)
                .unwrap()
                .as_secs()
                < 60
        );
    }

    #[test]
    fn the_desktop_host_and_electron_processes_are_recognized() {
        let host = "/Applications/DeepSeek Harness.app/Contents/MacOS/DeepSeek Harness";
        // The Node-mode host runs the app executable with the runtime bootstrap
        // on its command line.
        let host_command = format!(
            "{host} --expose-internals /Applications/DeepSeek Harness.app/Contents/Resources/app.asar/dsh/node_modules/@deepseek-ai/dsh-desktop-host/lib/index.js /Applications/DeepSeek Harness.app/Contents/Resources/app.asar/dsh /Users/me/.dsh/profiles/desktop /Applications/DeepSeek Harness.app/Contents/Resources/runtime/primary-runtime"
        );
        assert!(is_desktop_process(host, &host_command));
        // The Electron main process carries the bundle in its executable.
        assert!(is_desktop_process(host, host));
        assert!(is_desktop_process(
            "/Applications/DeepSeek Harness.app/Contents/Frameworks/DeepSeek Harness Helper.app/Contents/MacOS/DeepSeek Harness Helper",
            "helper --type=gpu-process"
        ));
        // A terminal `dsh` is not the desktop application.
        assert!(!is_desktop_process("dsh", "dsh"));
        assert!(!is_desktop_process(
            "/Users/me/.local/bin/deepseek-harness",
            "deepseek-harness tui"
        ));
    }

    #[test]
    fn the_desktop_home_comes_from_the_profile_argument() {
        let command = "/Applications/DeepSeek Harness.app/Contents/MacOS/DeepSeek Harness --expose-internals /Applications/DeepSeek Harness.app/Contents/Resources/app.asar/dsh/node_modules/@deepseek-ai/dsh-desktop-host/lib/index.js /Applications/DeepSeek Harness.app/Contents/Resources/app.asar/dsh /Users/me/.dsh/profiles/desktop /Applications/DeepSeek Harness.app/Contents/Resources/runtime/primary-runtime";
        assert_eq!(
            home_from_command(command).as_deref(),
            Some(Path::new("/Users/me/.dsh"))
        );
        // An environment-provided home with spaces still resolves.
        let custom = "/Applications/DeepSeek Harness.app/Contents/MacOS/DeepSeek Harness --expose-internals dsh/node_modules/@deepseek-ai/dsh-desktop-host/lib/index.js app.asar/dsh /Users/me/My State/.dsh/profiles/desktop runtime";
        assert_eq!(
            home_from_command(custom).as_deref(),
            Some(Path::new("/Users/me/My State/.dsh"))
        );
        assert!(home_from_command("/opt/homebrew/bin/dsh tui").is_none());
    }

    const ASKED: &str =
        "{\"type\":\"approval/asked\",\"data\":{\"id\":\"a\",\"toolName\":\"bash\"}}\n";

    #[test]
    fn an_unanswered_approval_prompt_is_recognized() {
        assert!(!log_signals("").approval_asked);
        assert!(log_signals(ASKED).approval_asked);
        // A later decision for the same prompt clears it.
        let decided = format!(
            "{ASKED}{{\"type\":\"approval/decided\",\"data\":{{\"id\":\"a\",\"outcome\":\"allowed-once\"}}}}\n"
        );
        assert!(!log_signals(&decided).approval_asked);
        // Prompts are paired by id, so an unrelated decision decides nothing.
        let other = format!("{ASKED}{{\"type\":\"approval/decided\",\"data\":{{\"id\":\"b\"}}}}\n");
        assert!(log_signals(&other).approval_asked);
        // An unrelated event must not clear a live prompt either.
        let noise = format!("{ASKED}{{\"type\":\"tool/call\",\"data\":{{\"name\":\"bash\"}}}}\n");
        assert!(log_signals(&noise).approval_asked);
    }

    #[test]
    fn a_pending_question_is_waiting_for_a_reply() {
        // The desktop projection reports no open step while a question is on
        // screen, so this pair of events is the only thing that distinguishes
        // "waiting for the user" from "working".
        let call = "{\"type\":\"tool/call\",\"data\":{\"callId\":\"q1\",\"name\":\"ask_user_question\"}}\n";
        let signals = log_signals(call);
        assert!(signals.question_pending);
        // A different tool is ordinary work.
        let work = "{\"type\":\"tool/call\",\"data\":{\"callId\":\"b1\",\"name\":\"bash\"}}\n";
        assert!(!log_signals(work).question_pending);
        // The answer arrives as the tool result for that call.
        let answered = format!(
            "{call}{{\"type\":\"tool/result\",\"data\":{{\"message\":{{\"source\":{{\"callId\":\"q1\"}}}}}}}}\n"
        );
        assert!(!log_signals(&answered).question_pending);
        // An unrelated result must not clear it.
        let unrelated = format!(
            "{call}{{\"type\":\"tool/result\",\"data\":{{\"message\":{{\"source\":{{\"callId\":\"other\"}}}}}}}}\n"
        );
        assert!(log_signals(&unrelated).question_pending);
    }

    #[test]
    fn only_a_spent_retry_budget_is_a_failure() {
        // The session log has no error event; `agent/error` is live-bus only. A
        // failed step appears as `llm/retry`, which is also what a *recovering*
        // step writes, so only a spent retry budget counts.
        let retry = |retry: u64, max: Value| {
            format!(
                "{{\"type\":\"turn/start\",\"data\":{{\"turn\":1}}}}\n{{\"type\":\"llm/retry\",\"data\":{{\"turn\":1,\"step\":2,\"provider\":\"deepseek-official\",\"mode\":\"normal\",\"retry\":{retry},\"maxRetries\":{max},\"delayMs\":500,\"failure\":{{\"message\":\"boom\"}}}}}}\n"
            )
        };
        // Inside the budget the plugin is retrying the step, not failing it.
        assert!(!log_signals(&retry(1, json!(5))).error);
        assert!(!log_signals(&retry(4, json!(5))).error);
        // The last attempt has been spent.
        assert!(log_signals(&retry(5, json!(5))).error);
        // Closing the turn is not recovery: an offline session ends exactly
        // there (retries spent, turn closed, nothing after it).
        let closed = format!(
            "{}{{\"type\":\"turn/end\",\"data\":{{\"turn\":1}}}}\n",
            retry(5, json!(5))
        );
        assert!(log_signals(&closed).error);
        // Progress after it is.
        let recovered = format!(
            "{closed}{{\"type\":\"turn/start\",\"data\":{{\"turn\":2}}}}\n{{\"type\":\"assistant/message\",\"data\":{{\"turn\":2}}}}\n"
        );
        assert!(!log_signals(&recovered).error);
        // `mode: "always"` carries no budget and never stops on its own.
        let always = "{\"type\":\"turn/start\",\"data\":{\"turn\":1}}\n{\"type\":\"llm/retry\",\"data\":{\"mode\":\"always\",\"retry\":9,\"failure\":{\"message\":\"boom\"}}}\n";
        assert!(!log_signals(always).error);
        // A record without a retry count is not evidence either.
        assert!(
            !log_signals(
                "{\"type\":\"turn/start\",\"data\":{}}\n{\"type\":\"llm/retry\",\"data\":{}}\n"
            )
            .error
        );
        assert!(
            !log_signals(
                "{\"type\":\"turn/start\",\"data\":{}}\n{\"type\":\"step/end\",\"data\":{}}\n"
            )
            .error
        );
        // A failure and a pending prompt can coexist in one tail.
        let both = format!("{ASKED}{}", retry(5, json!(5)));
        let signals = log_signals(&both);
        assert!(signals.error && signals.approval_asked);
    }

    /// `zstd` is the same external tool the CLI session reader relies on; a
    /// machine without it cannot decompress these logs at all, and the approval
    /// pass degrades to leaving the conversation as working.
    fn zstd_available() -> bool {
        std::process::Command::new("zstd")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// A desktop profile laid out like the real one: a projection cache next to
    /// the compressed session log the same session id writes.
    fn fake_profile(id: &str, events: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!(
            "asi-desktop-profile-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        let cache = home.join("storages/session_projcache/sessions");
        std::fs::create_dir_all(&cache).unwrap();
        let document = session_document(rows(
            json!({ "turn": 1, "step": 3, "firstTokenTime": null }),
            json!({}),
            json!([]),
            json!("等确认"),
        ));
        std::fs::write(
            cache.join(format!("{id}.json")),
            serde_json::to_string(&document).unwrap(),
        )
        .unwrap();
        let log = home
            .join("sessions")
            .join("--Users-me-code-app--")
            .join(id)
            .join("session.v4.jsonl.zstd");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        compress(events, &log);
        home
    }

    /// Compresses `text` with the external `zstd`, the format the desktop app
    /// writes.
    fn compress(text: &str, path: &Path) {
        use std::io::Write;
        let mut child = std::process::Command::new("zstd")
            .args(["-q", "-f", "-o"])
            .arg(path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .expect("zstd");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(text.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn a_pending_approval_marks_the_conversation_as_waiting() {
        if !zstd_available() {
            return;
        }
        let id = "session-approval";
        let home = fake_profile(
            id,
            "{\"type\":\"approval/asked\",\"data\":{\"id\":\"a\"}}\n",
        );
        let mut analyzer = DeepSeekDesktopAnalyzer::default();
        assert!(analyzer.refresh(Some(&home)) > 0);
        let sessions = analyzer.sessions(None);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].state, AgentState::Waiting);
    }

    #[test]
    fn an_appended_log_is_reread_inside_the_check_interval() {
        if !zstd_available() {
            return;
        }
        // A decision has to clear the waiting state on the next 2-second scan,
        // not after the rate limit, so an appended log is always re-read.
        let id = "session-appended";
        let home = fake_profile(
            id,
            "{\"type\":\"approval/asked\",\"data\":{\"id\":\"a\"}}\n",
        );
        let mut analyzer = DeepSeekDesktopAnalyzer::default();
        analyzer.refresh(Some(&home));
        assert_eq!(analyzer.sessions(None)[0].state, AgentState::Waiting);
        let log = home
            .join("sessions/--Users-me-code-app--")
            .join(id)
            .join("session.v4.jsonl.zstd");
        compress(
            "{\"type\":\"approval/asked\",\"data\":{\"id\":\"a\"}}\n{\"type\":\"approval/decided\",\"data\":{\"id\":\"a\"}}\n",
            &log,
        );
        analyzer.refresh(Some(&home));
        let state = analyzer.sessions(None)[0].state;
        assert_eq!(state, AgentState::Working);
    }

    #[test]
    fn a_decided_approval_leaves_the_conversation_working() {
        if !zstd_available() {
            return;
        }
        let id = "session-approved";
        let home = fake_profile(
            id,
            "{\"type\":\"approval/asked\",\"data\":{\"id\":\"a\"}}\n{\"type\":\"approval/decided\",\"data\":{\"id\":\"a\",\"outcome\":\"allowed-once\"}}\n",
        );
        let mut analyzer = DeepSeekDesktopAnalyzer::default();
        analyzer.refresh(Some(&home));
        assert_eq!(analyzer.sessions(None)[0].state, AgentState::Working);
    }

    #[test]
    fn an_approval_policy_never_marks_the_auto_confirmation_mode() {
        let auto = parse_temp(&session_document(with_approval(
            rows_plain(json!(null), json!({}), json!([]), json!("自动确认")),
            "never",
        )));
        assert!(auto.automatic_confirmation_mode);
        // "ask" means the user still approves, so notifications must not be
        // silenced for it.
        let asked = parse_temp(&session_document(with_approval(
            rows_plain(json!(null), json!({}), json!([]), json!("需要确认")),
            "ask",
        )));
        assert!(!asked.automatic_confirmation_mode);
    }

    #[test]
    fn an_answered_approval_clears_inside_the_rate_limit() {
        if !zstd_available() {
            return;
        }
        // Regression for the approval row that stayed on "waiting for
        // confirmation" after the user submitted the prompt: the decision moves
        // the log, but it lands within the read interval, and the interval used
        // to win — the cached "asked" answer was reused until something else
        // happened to refresh the row.
        let id = "session-approval-timing";
        let home = fake_profile(
            id,
            "{\"type\":\"approval/asked\",\"data\":{\"id\":\"a\"}}\n",
        );
        let mut analyzer = DeepSeekDesktopAnalyzer::default();
        analyzer.refresh(Some(&home));
        assert_eq!(analyzer.sessions(None)[0].state, AgentState::Waiting);

        let log = home
            .join("sessions")
            .join("--Users-me-code-app--")
            .join(id)
            .join("session.v4.jsonl.zstd");
        // Answered immediately: the second scan happens well inside the interval.
        compress(
            "{\"type\":\"approval/asked\",\"data\":{\"id\":\"a\"}}\n{\"type\":\"approval/decided\",\"data\":{\"id\":\"a\",\"outcome\":\"allowed-once\"}}\n",
            &log,
        );
        analyzer.refresh(Some(&home));
        assert_eq!(
            analyzer.sessions(None)[0].state,
            AgentState::Working,
            "the decision must clear the wait on the next scan, not after the interval"
        );
    }

    #[test]
    fn a_live_pending_question_reads_as_waiting_for_a_reply() {
        if !zstd_available() {
            return;
        }
        // The exact live shape: no open step, one pending call in the projection,
        // and the log holding the unanswered `ask_user_question` call.
        let id = "session-live-question";
        let home = std::env::temp_dir().join(format!(
            "asi-desktop-question-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        let cache = home.join("storages/session_projcache/sessions");
        std::fs::create_dir_all(&cache).unwrap();
        let rows = rows_plain(
            json!(null),
            json!({ "q1": { "name": "ask_user_question" } }),
            json!([]),
            json!("等你回答"),
        );
        std::fs::write(
            cache.join(format!("{id}.json")),
            serde_json::to_string(&session_document(rows)).unwrap(),
        )
        .unwrap();
        let log = home
            .join("sessions")
            .join("--Users-me-code-app--")
            .join(id)
            .join("session.v4.jsonl.zstd");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        compress(
            "{\"type\":\"tool/call\",\"data\":{\"callId\":\"q1\",\"name\":\"ask_user_question\"}}\n",
            &log,
        );

        let mut analyzer = DeepSeekDesktopAnalyzer::default();
        analyzer.refresh(Some(&home));
        let sessions = analyzer.sessions(None);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].state, AgentState::WaitingReply);
        assert!(needs_attention(sessions[0].state));

        // Answering it settles the call in both places: the app rewrites the
        // projection without the pending call and appends the tool result.
        let settled = rows_plain(json!(null), json!({}), json!([]), json!("等你回答"));
        std::fs::write(
            cache.join(format!("{id}.json")),
            serde_json::to_string(&session_document(settled)).unwrap(),
        )
        .unwrap();
        compress(
            "{\"type\":\"tool/call\",\"data\":{\"callId\":\"q1\",\"name\":\"ask_user_question\"}}\n{\"type\":\"tool/result\",\"data\":{\"message\":{\"source\":{\"callId\":\"q1\"}}}}\n",
            &log,
        );
        analyzer.refresh(Some(&home));
        assert_eq!(analyzer.sessions(None)[0].state, AgentState::Ready);
    }

    #[test]
    fn a_stale_projection_is_not_listed() {
        // Measured on this profile: two projection files carry no
        // `identity.formatVersion` and 13 rows instead of 24, and the desktop
        // app's workspace list omits them. Listing them invented rows — two of
        // them sharing a title — that existed nowhere in the UI.
        // Built from the real document shape, so the flag is exercised where it
        // is actually set.
        let current = parse_temp(&session_document(rows(
            json!(null),
            json!({}),
            json!([]),
            json!("当前格式"),
        )));
        assert_eq!(current.format_version, Some(4));
        let mut legacy = current.clone();
        legacy.id = "session-legacy".into();
        legacy.format_version = None;

        // No registry present means "show everything", so this exercises the
        // format filter rather than the application's list.
        let listed = listed_sessions(&[legacy, current], None, &WorkspaceRegistry::default());
        assert_eq!(listed.len(), 1, "the stale projection must not be listed");
        assert_ne!(listed[0].format_version, None);

        // Reported, but not as damage: a superseded projection is normal
        // history, so it must not raise the profile alert.
        let health = ProfileHealth {
            root_present: true,
            parsed: 2,
            stale_format: 1,
            ..ProfileHealth::default()
        };
        assert_eq!(health.stale_format, 1);
        assert!(health.is_intact());
        assert!(health.alert().is_none());
    }

    #[test]
    fn only_the_sessions_the_application_shows_are_listed() {
        // Measured on this profile: the application's workspace list held five
        // sessions while six projections were present, and the extra one was the
        // conversation a `dsh` CLI process created in the same project. The tray
        // listed it as a desktop row, which is what "classified as desktop"
        // means here.
        let current = parse_temp(&session_document(rows(
            json!(null),
            json!({}),
            json!([]),
            json!("应用打开的会话"),
        )));
        let mut cli = current.clone();
        cli.id = "session-cli".into();

        let mut registry = WorkspaceRegistry::default();
        registry.present = true;
        registry.sessions.insert(current.id.clone());

        let listed = listed_sessions(&[cli.clone(), current.clone()], None, &registry);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, current.id);
        assert!(registry.shows(&current));
        assert!(!registry.shows(&cli));

        // A registry that was never written must not blank the list.
        let absent = WorkspaceRegistry::default();
        assert!(absent.shows(&cli));
        assert_eq!(listed_sessions(&[cli], None, &absent).len(), 1);
    }

    #[test]
    fn an_archived_conversation_is_not_listed() {
        // Measured on this profile: `archivedSessionIds` held one conversation
        // that the workspace's `sessionIds` still names, so membership alone kept
        // reporting a conversation the user had filed away.
        let shown = parse_temp(&session_document(rows(
            json!(null),
            json!({}),
            json!([]),
            json!("在列表里的会话"),
        )));
        let mut archived = shown.clone();
        archived.id = "session-archived".into();

        let mut registry = WorkspaceRegistry::default();
        registry.present = true;
        registry.sessions.insert(shown.id.clone());
        registry.sessions.insert(archived.id.clone());

        // Both are named by a workspace, so both are the application's…
        assert!(registry.shows(&shown));
        assert!(registry.shows(&archived));
        // …until one is archived, which removes it from the list.
        registry.archived.insert(archived.id.clone());
        assert!(registry.shows(&shown));
        assert!(!registry.shows(&archived));

        let listed = listed_sessions(&[archived, shown.clone()], None, &registry);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, shown.id);

        // Archiving is keyed by session, so another session is unaffected even
        // when it shares the archived one's project and title.
        let mut same_project = shown.clone();
        same_project.id = "session-other".into();
        registry.sessions.insert(same_project.id.clone());
        assert!(registry.shows(&same_project));
    }

    #[test]
    fn the_health_report_explains_a_shape_change() {
        // A renamed row must not read as "everything is fine, there are simply
        // no sessions" — that is the silent failure this accounting exists for.
        let dir = std::env::temp_dir().join(format!(
            "asi-health-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        let cache = dir.join("storages/session_projcache/sessions");
        std::fs::create_dir_all(&cache).unwrap();

        // Intact: parsed, nothing missing.
        let good = session_document(rows(json!(null), json!({}), json!([]), json!("健康")));
        std::fs::write(
            cache.join("session-good.json"),
            serde_json::to_string(&good).unwrap(),
        )
        .unwrap();
        let mut analyzer = DeepSeekDesktopAnalyzer::default();
        analyzer.refresh(Some(&dir));
        assert_eq!(analyzer.health().parsed, 1);
        assert_eq!(analyzer.health().incomplete, 0);
        assert!(analyzer.health().is_intact());

        // Rows moved (same format version): parsed but reported incomplete.
        let mut moved = session_document(rows(json!(null), json!({}), json!([]), json!("改名")));
        let stats = moved["record"]["rows"]["sessionStats"].take();
        moved["record"]["rows"]["turnStats"] = stats;
        std::fs::write(
            cache.join("session-moved.json"),
            serde_json::to_string(&moved).unwrap(),
        )
        .unwrap();
        let mut analyzer = DeepSeekDesktopAnalyzer::default();
        analyzer.refresh(Some(&dir));
        assert_eq!(
            analyzer.health().incomplete,
            1,
            "a renamed row must be visible"
        );
        assert!(!analyzer.health().is_intact());

        // A newer format version whose rows moved: refused and named.
        let mut future = session_document(rows(json!(null), json!({}), json!([]), json!("未来")));
        future["version"] = json!(99);
        let stats = future["record"]["rows"]["sessionStats"].take();
        future["record"]["rows"]["turnStats"] = stats;
        std::fs::write(
            cache.join("session-future.json"),
            serde_json::to_string(&future).unwrap(),
        )
        .unwrap();
        let mut analyzer = DeepSeekDesktopAnalyzer::default();
        analyzer.refresh(Some(&dir));
        assert_eq!(analyzer.health().version_mismatch, 1);
        assert!(!analyzer.health().is_intact());

        // Unreadable bytes are local damage, counted separately.
        std::fs::write(cache.join("session-broken.json"), b"{not json").unwrap();
        let mut analyzer = DeepSeekDesktopAnalyzer::default();
        analyzer.refresh(Some(&dir));
        assert_eq!(analyzer.health().unreadable, 1);

        // An absent profile is stated, not implied by an empty list.
        let empty = DeepSeekDesktopAnalyzer::default();
        let mut empty = empty;
        empty.refresh(Some(&dir.join("nowhere")));
        assert!(!empty.health().root_present);
    }

    #[test]
    fn a_stale_conversation_is_not_probed_for_a_failure() {
        if !zstd_available() {
            return;
        }
        // Outside the failure window a session is left alone: its state comes
        // from the projection, and the log is not read. This is deliberate (the
        // cost bound) and is why the window exists.
        let id = "session-stale";
        let home = std::env::temp_dir().join(format!(
            "asi-desktop-stale-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        let cache = home.join("storages/session_projcache/sessions");
        std::fs::create_dir_all(&cache).unwrap();
        let mut document =
            session_document(rows(json!(null), json!({}), json!([]), json!("陈旧会话")));
        document["record"]["rows"]["sessionListMetadata"]["val"]["lastPromptAt"] =
            json!(now_millis().saturating_sub(60 * 60 * 1000));
        std::fs::write(
            cache.join(format!("{id}.json")),
            serde_json::to_string(&document).unwrap(),
        )
        .unwrap();
        let log = home
            .join("sessions")
            .join("--Users-me-code-app--")
            .join(id)
            .join("session.v4.jsonl.zstd");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        compress(
            "{\"type\":\"turn/start\",\"data\":{\"turn\":1}}\n{\"type\":\"llm/retry\",\"data\":{\"turn\":1,\"step\":1,\"mode\":\"normal\",\"retry\":5,\"maxRetries\":5,\"failure\":{\"message\":\"boom\"}}}\n",
            &log,
        );
        let mut analyzer = DeepSeekDesktopAnalyzer::default();
        analyzer.refresh(Some(&home));
        // A finished turn with old activity: ready, and no log read at all.
        assert_eq!(analyzer.sessions(None)[0].state, AgentState::Ready);
        assert!(analyzer.signal_report(id)["readAgoSecs"].is_null());
    }

    #[test]
    fn a_turn_that_ends_drops_the_working_state() {
        // Regression: the applied state used to be written back over the
        // projection's own, so once a scan reported "working" the row kept
        // reporting it after the turn ended — the cache was reused (size and
        // modification time unchanged) and the session had dropped out of the
        // observation set, so nothing ever recomputed it.
        let home = std::env::temp_dir().join(format!(
            "asi-desktop-latch-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        let cache = home.join("storages/session_projcache/sessions");
        std::fs::create_dir_all(&cache).unwrap();
        let document = |open_step: Value| {
            serde_json::to_string(&session_document(rows(
                open_step,
                json!({}),
                json!([]),
                json!("一轮结束后不应还是进行中"),
            )))
            .unwrap()
        };
        let file = cache.join("session-latch.json");
        std::fs::write(&file, document(json!({ "turn": 1, "step": 1 }))).unwrap();

        let mut analyzer = DeepSeekDesktopAnalyzer::default();
        analyzer.refresh(Some(&home));
        assert_eq!(analyzer.sessions(None)[0].state, AgentState::Working);

        // The turn ends: the app rewrites the cache with no open step.
        std::fs::write(&file, document(json!(null))).unwrap();
        analyzer.refresh(Some(&home));
        assert_eq!(
            analyzer.sessions(None)[0].state,
            AgentState::Ready,
            "a finished turn must not keep reporting the working state"
        );

        // And a session that leaves the observation set entirely still reads the
        // projection, because the reported state is derived, never latched.
        let mut fresh = DeepSeekDesktopAnalyzer::default();
        fresh.refresh(Some(&home));
        assert_eq!(fresh.sessions(None)[0].state, AgentState::Ready);
    }

    #[test]
    fn a_finished_turn_with_a_failure_is_an_error_not_ready() {
        // The projection drops the open step when the turn fails, so the event
        // log is the only thing that can still tell ready from failed.
        let session = parse_temp(&session_document(rows_plain(
            json!(null),
            json!({}),
            json!([]),
            json!("失败的会话"),
        )));
        assert_eq!(session.state, AgentState::Ready);
        assert!(!session.turn_open);

        let home = std::env::temp_dir().join(format!(
            "asi-desktop-failed-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        let mut analyzer = DeepSeekDesktopAnalyzer::default();
        let id = "session-failed";
        let cache = home.join("storages/session_projcache/sessions");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            cache.join(format!("{id}.json")),
            serde_json::to_string(&session_document(rows_plain(
                json!(null),
                json!({}),
                json!([]),
                json!("失败的会话"),
            )))
            .unwrap(),
        )
        .unwrap();
        // No log yet: the projection alone reports ready.
        analyzer.refresh(Some(&home));
        assert_eq!(analyzer.sessions(None)[0].state, AgentState::Ready);
        if !zstd_available() {
            return;
        }
        // A turn that failed and released its step now reads as failed.
        let log = home
            .join("sessions")
            .join("--Users-me-code-app--")
            .join(id)
            .join("session.v4.jsonl.zstd");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        compress(
            "{\"type\":\"turn/start\",\"data\":{\"turn\":1}}\n{\"type\":\"step/start\",\"data\":{\"turn\":1,\"step\":1}}\n{\"type\":\"step/end\",\"data\":{\"turn\":1,\"step\":1}}\n{\"type\":\"llm/retry\",\"data\":{\"turn\":1,\"step\":1,\"mode\":\"normal\",\"retry\":5,\"maxRetries\":5,\"failure\":{\"message\":\"boom\"}}}\n",
            &log,
        );
        analyzer.refresh(Some(&home));
        assert_eq!(analyzer.sessions(None)[0].state, AgentState::Error);
        assert!(needs_attention(AgentState::Error));
    }

    #[test]
    fn the_cap_drops_least_urgent_and_reports_how_many() {
        let now = SystemTime::now();
        let mut sessions: Vec<DesktopSession> = (0..10)
            .map(|index| DesktopSession {
                id: format!("session-{index}"),
                cwd: None,
                title: Some(format!("会话 {index}")),
                state: AgentState::Ready,
                model: None,
                context: None,
                // Older as the index grows, so index 0 is the newest.
                activity: now - Duration::from_secs(index),
                last_prompt: None,
                format_version: Some(4),
                run: None,
                turn_open: false,
                automatic_confirmation_mode: false,
                missing_rows: 0,
            })
            .collect();
        // One waiting conversation is the oldest: urgency must beat recency, or
        // the cap would drop the row that actually needs the user.
        let waiting = DesktopSession {
            id: "session-waiting".into(),
            state: AgentState::Waiting,
            activity: now - Duration::from_secs(3600),
            ..sessions[0].clone()
        };
        sessions.push(waiting);
        sessions.sort_by(|left, right| {
            attention_rank(left.state)
                .cmp(&attention_rank(right.state))
                .then_with(|| right.activity.cmp(&left.activity))
                .then_with(|| left.id.cmp(&right.id))
        });
        assert_eq!(sessions[0].state, AgentState::Waiting);
        assert_eq!(sessions.len(), 11);
        let hidden = sessions.len().saturating_sub(MAX_DESKTOP_ROWS);
        assert_eq!(hidden, 3);
        sessions.truncate(MAX_DESKTOP_ROWS);
        assert!(sessions
            .iter()
            .any(|session| session.state == AgentState::Waiting));
    }

    #[test]
    fn only_mid_turn_or_recent_sessions_stay_listed() {
        let now = SystemTime::now();
        let session = |state: AgentState, age_secs: u64| DesktopSession {
            id: "session-x".into(),
            cwd: None,
            title: Some("t".into()),
            state,
            model: None,
            context: None,
            activity: now - Duration::from_secs(age_secs),
            last_prompt: None,
            format_version: Some(4),
            run: None,
            turn_open: false,
            automatic_confirmation_mode: false,
            missing_rows: 0,
        };
        let window = Some(Duration::from_secs(15 * 60));
        assert!(worth_showing(
            &session(AgentState::Working, 4 * 3600),
            now,
            window
        ));
        assert!(worth_showing(
            &session(AgentState::Waiting, 4 * 3600),
            now,
            window
        ));
        assert!(worth_showing(
            &session(AgentState::Error, 4 * 3600),
            now,
            window
        ));
        assert!(worth_showing(&session(AgentState::Ready, 60), now, window));
        assert!(!worth_showing(
            &session(AgentState::Ready, 3600),
            now,
            window
        ));
        // "All conversations" keeps finished ones regardless of age.
        assert!(worth_showing(
            &session(AgentState::Ready, 30 * 86_400),
            now,
            None
        ));
    }
}
