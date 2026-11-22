//! What the CLI session readers have in common.
//!
//! Gemini CLI, Grok CLI and Cursor CLI all write one session record per
//! conversation, keyed by the project directory, and several sessions can share
//! one directory. A row therefore has to be bound to the session its own process
//! is writing; reading "the newest session in the directory" would make two
//! agents in the same project mirror whichever one happens to be active.
//!
//! The agents differ in how they name and timestamp those files, so each reader
//! projects its candidates into [`Candidate`] and hands the live processes to
//! [`bind_sessions`], which is the one place the pairing rules live.

use crate::model::{AgentState, ContextUsage};
use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

/// The facts a session reader contributes to its row.
#[derive(Clone, Debug, Default)]
pub struct AgentFacts {
    pub state: Option<AgentState>,
    pub model: Option<String>,
    pub context: Option<ContextUsage>,
    /// The conversation title. Every reader falls back to the first user
    /// message when the agent records no generated title.
    pub title: Option<String>,
}

/// One live agent process that the detector found in a directory.
pub struct LiveSession {
    pub started: SystemTime,
}

/// One session record on disk, in the form the binding rules need.
pub struct Candidate {
    pub path: PathBuf,
    pub created: SystemTime,
    pub modified: SystemTime,
}

/// How far a session file may be created from the matching process start and
/// still be treated as that process's fresh session. Some agents create the
/// record when the first message is sent rather than at launch, so this is a
/// generous window; the recency pass below covers anything later.
const ANCHOR_WINDOW_SECONDS: u64 = 90;

/// Slack allowed between a process's start and a session's last write. Uptime
/// comes from `ps` whole-second etime and filesystem mtimes can be coarse, so a
/// fresh session written right at launch can lag the start by a few seconds.
const WRITE_TOLERANCE_SECONDS: u64 = 5;

/// Assign each live process one session record. Returns an index into
/// `candidates` per entry of `live` (or `None` when there is no fit).
///
/// 1. **Anchor**: a new session is created near its process's start. Claim the
///    closest pair first so a slow-starting sibling cannot steal another
///    process's file.
/// 2. **Recency**: a resumed session was created long before the process
///    started but is being written again now, so pair the remaining processes
///    (newest first) with the remaining files (newest first) whose last write is
///    at or after that process's start.
/// 3. **`/new`**: a process that moves to a newer session file keeps its old
///    anchor; follow the newer file once it is the one being written.
/// 4. **Resume alone**: with one process, an older file that is now the most
///    recently written one is the live conversation.
///
/// A file written entirely before a process started belongs to an earlier
/// conversation; such a process stays unbound (and shows its process-activity
/// state) rather than inheriting a dead session's tail.
pub fn bind_sessions(live: &[LiveSession], candidates: &[Candidate]) -> Vec<Option<usize>> {
    let mut claims: Vec<Option<usize>> = vec![None; live.len()];
    let mut claimed = vec![false; candidates.len()];

    loop {
        let mut best: Option<(usize, usize)> = None;
        let mut best_delta = u64::MAX;
        for (index, process) in live.iter().enumerate() {
            if claims[index].is_some() {
                continue;
            }
            for (candidate, file) in candidates.iter().enumerate() {
                if claimed[candidate] || !written_while_alive(file, process) {
                    continue;
                }
                if let Some(delta) = seconds_between(file.created, process.started) {
                    if delta <= ANCHOR_WINDOW_SECONDS && delta < best_delta {
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
            let candidate = free.remove(position);
            claimed[candidate] = true;
            claims[index] = Some(candidate);
        }
    }

    // 3. `/new` keeps the process alive while it moves to a brand-new session
    //    file, so the file anchored at startup is no longer being written and
    //    the later file the process created stays unclaimed. Rebind to the
    //    newest later session; without this the row keeps the pre-`/new` title,
    //    model and state.
    let mut newest_first: Vec<usize> = (0..live.len()).collect();
    newest_first.sort_by_key(|index| std::cmp::Reverse(live[*index].started));
    for index in newest_first {
        let Some(previous) = claims[index] else {
            continue;
        };
        let mut best: Option<usize> = None;
        for (candidate, file) in candidates.iter().enumerate() {
            if claimed[candidate]
                || file.created <= candidates[previous].created
                || file.modified <= candidates[previous].modified
            {
                continue;
            }
            // Only a session that appeared at or after this process started can
            // be one it created; a file that clearly predates it belongs to an
            // earlier run.
            if unix_seconds(live[index].started).saturating_sub(unix_seconds(file.created))
                > WRITE_TOLERANCE_SECONDS
            {
                continue;
            }
            if !written_while_alive(file, &live[index]) {
                continue;
            }
            if best.is_none_or(|current: usize| file.created > candidates[current].created) {
                best = Some(candidate);
            }
        }
        if let Some(candidate) = best {
            claimed[previous] = false;
            claims[index] = Some(candidate);
            claimed[candidate] = true;
        }
    }

    // 4. `/new` moves forward in time, but resuming moves the other way: the
    //    process creates its startup file and then opens an older conversation,
    //    so the anchored file freezes while the older one keeps growing. With no
    //    sibling to confuse, the file written most recently is the one in use.
    if live.len() == 1 {
        if let Some(previous) = claims[0] {
            let newer = candidates
                .iter()
                .enumerate()
                .filter(|(index, file)| {
                    *index != previous
                        && !claimed[*index]
                        && file.modified > candidates[previous].modified
                        && written_while_alive(file, &live[0])
                })
                .max_by_key(|(_, file)| file.modified)
                .map(|(index, _)| index);
            if let Some(candidate) = newer {
                claimed[previous] = false;
                claimed[candidate] = true;
                claims[0] = Some(candidate);
            }
        }
    }
    claims
}

fn written_while_alive(file: &Candidate, process: &LiveSession) -> bool {
    let started = unix_seconds(process.started);
    let modified = unix_seconds(file.modified);
    started.saturating_sub(modified) <= WRITE_TOLERANCE_SECONDS
}

pub fn seconds_between(a: SystemTime, b: SystemTime) -> Option<u64> {
    if a >= b {
        a.duration_since(b).ok().map(|delta| delta.as_secs())
    } else {
        b.duration_since(a).ok().map(|delta| delta.as_secs())
    }
}

pub fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map(|delta| delta.as_secs())
        .unwrap_or(0)
}

/// A path in the one shape every comparison in the session readers can use:
/// forward slashes, no trailing separator, and lower case on the case-insensitive
/// platforms. The rows themselves keep the original path; this is only a key.
pub fn path_key(path: &Path) -> String {
    let mut normalized = path.to_string_lossy().replace('\\', "/");
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

/// [`path_key`] after resolving a symlink. The process table reports a
/// canonical path (`/private/tmp/...` on macOS) while a project registry may
/// store the spelling the user opened (`/tmp/...`), and both name the same
/// directory. A path that no longer exists falls back to its literal key.
pub fn canonical_key(path: &Path) -> String {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    path_key(&resolved)
}

/// Decode `%XX` escapes in a directory name. Grok names its session groups by
/// the URL-encoded working directory. Invalid escapes are left alone rather
/// than dropped, so a name that merely contains a `%` still compares as itself.
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_digit(bytes[index + 1]), hex_digit(bytes[index + 2]))
            {
                output.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// The text of a message `content` field: either a string or an array of parts
/// carrying `text`.
pub fn content_text(content: &serde_json::Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }
    let text = content
        .as_array()?
        .iter()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    (!text.is_empty()).then_some(text)
}

/// Whether a finished assistant message asked the user something, which is the
/// one shape that reads as "waiting for a reply" without an explicit signal.
pub fn text_ends_with_question(text: &str) -> bool {
    text.trim_end_matches(|ch: char| {
        matches!(ch, '"' | '\'' | ')' | ']' | '}' | '。' | '！' | '!' | '.')
    })
    .ends_with(['?', '？'])
}

/// A display string with control characters collapsed, so a title cannot break
/// a menu row.
pub fn clean_text(value: &str) -> Option<String> {
    let value = value.replace(['\r', '\n', '\t', '|'], " ");
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!value.is_empty()).then(|| value.chars().take(80).collect())
}

/// The first line of a file, used only for the `.cwd` marker Grok writes when a
/// group name would exceed the filesystem's name limit.
pub fn read_small_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// The mtime of a path, or the epoch when it cannot be read.
pub fn modified_or_epoch(path: &Path) -> SystemTime {
    path.metadata()
        .and_then(|metadata| metadata.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// The creation time of a path, falling back to its mtime (Linux filesystems do
/// not all report a birth time).
pub fn created_or_modified(path: &Path) -> SystemTime {
    path.metadata()
        .and_then(|metadata| metadata.created().or_else(|_| metadata.modified()))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn live(started: u64) -> LiveSession {
        LiveSession {
            started: at(started),
        }
    }

    fn candidate(created: u64, modified: u64) -> Candidate {
        Candidate {
            path: PathBuf::from(format!("session-{created}-{modified}")),
            created: at(created),
            modified: at(modified),
        }
    }

    #[test]
    fn a_fresh_session_binds_to_the_process_that_created_it() {
        // Two processes and two sessions: the anchor pass must not let the
        // slower sibling take the other process's file.
        let processes = [live(1_000), live(1_100)];
        let files = [candidate(1_001, 1_200), candidate(1_101, 1_300)];
        let claims = bind_sessions(&processes, &files);
        assert_eq!(claims, [Some(0), Some(1)]);
    }

    #[test]
    fn a_resumed_session_binds_by_recency() {
        // The file predates the process but is being written again, so the old
        // session is the one the process drives.
        let processes = [live(1_000)];
        let files = [candidate(100, 1_050), candidate(200, 300)];
        assert_eq!(bind_sessions(&processes, &files), [Some(0)]);
    }

    #[test]
    fn a_file_written_before_the_process_started_is_not_adopted() {
        // A brand-new process with no record of its own must not inherit the
        // directory's stale session and report that conversation's state.
        let processes = [live(10_000)];
        let files = [candidate(100, 200)];
        assert_eq!(bind_sessions(&processes, &files), [None]);
    }

    #[test]
    fn two_processes_never_share_one_file() {
        let processes = [live(1_000), live(1_010)];
        let files = [candidate(1_005, 1_020)];
        let claims = bind_sessions(&processes, &files);
        let assigned: Vec<_> = claims.iter().flatten().collect();
        assert_eq!(assigned.len(), 1, "the second process stays unbound");
    }

    #[test]
    fn a_new_session_in_the_same_process_wins() {
        // `/new` keeps the process alive; the later file is the live one even
        // though the startup file is the closer creation-time anchor.
        let processes = [live(1_000)];
        let files = [candidate(1_001, 1_100), candidate(1_200, 1_300)];
        assert_eq!(bind_sessions(&processes, &files), [Some(1)]);
    }

    #[test]
    fn a_resumed_session_in_the_same_process_wins() {
        // Resuming opens an older file (created long before the process) but it
        // is the one being written now, while the startup file is frozen.
        let processes = [live(1_000)];
        let files = [candidate(1_001, 1_050), candidate(100, 1_200)];
        assert_eq!(bind_sessions(&processes, &files), [Some(1)]);
    }

    #[test]
    fn no_candidates_leaves_every_process_unbound() {
        let claims = bind_sessions(&[live(1_000)], &[]);
        assert_eq!(claims, [None]);
    }

    #[test]
    fn decodes_the_encoded_directory_grok_records() {
        assert_eq!(
            percent_decode("%2FUsers%2Ftest%2Fmy%20project"),
            "/Users/test/my project"
        );
        assert_eq!(percent_decode("100%"), "100%", "a bare % is kept");
        assert_eq!(percent_decode("a%zzb"), "a%zzb", "a bad escape is kept");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn path_keys_ignore_separators_and_trailing_slashes() {
        assert_eq!(
            path_key(Path::new("/Users/me/project/")),
            path_key(Path::new("/Users/me/project"))
        );
        assert_eq!(
            path_key(Path::new("C:\\work\\project")),
            path_key(Path::new("C:/work/project"))
        );
    }

    #[test]
    fn question_detection_ignores_trailing_punctuation() {
        assert!(text_ends_with_question("Should I continue?"));
        assert!(text_ends_with_question("这样可以吗？"));
        assert!(text_ends_with_question("Ready?"));
        assert!(text_ends_with_question("Done?)"));
        assert!(!text_ends_with_question("Done.)"));
        assert!(!text_ends_with_question("Finished."));
        assert!(!text_ends_with_question("No question here"));
    }

    #[test]
    fn content_text_reads_strings_and_text_parts() {
        let string = serde_json::json!("hello");
        assert_eq!(content_text(&string).as_deref(), Some("hello"));
        let parts = serde_json::json!([{"text": "a"}, {"text": "b"}]);
        assert_eq!(content_text(&parts).as_deref(), Some("ab"));
        let empty = serde_json::json!([]);
        assert_eq!(content_text(&empty), None);
    }
}
