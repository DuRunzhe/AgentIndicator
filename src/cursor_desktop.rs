//! Cursor desktop (the IDE's Agent panel) sessions.
//!
//! Cursor keeps every Composer session in one global SQLite database,
//! `<Cursor User>/globalStorage/state.vscdb`. Each session has a
//! `composerData:<id>` state document in `cursorDiskKV` and one
//! `bubbleId:<id>:<bubble>` row per message segment. The document carries the
//! live turn state (`status`, `generatingBubbleIds`), the model, the context
//! counters and the list that orders the bubbles; the bubbles carry the text,
//! tool status and token counts.
//!
//! The workspaceStorage databases map a composer to the folder it was opened
//! in, and their header copy also carries `hasBlockingPendingActions`. The
//! format is closed source and has drifted between releases, so every read is
//! tolerant: a missing field or an unreadable row drops one fact, never the
//! whole scan.
//!
//! This is separate from [`crate::cursor`]: the CLI's `~/.cursor/chats` store
//! has a different schema and no token counts.

use crate::agent_sessions::{clean_text, percent_decode, text_ends_with_question};
use crate::cursor::json_blob;
use crate::model::{AgentState, ContextUsage};
use crate::timestamps::parse_rfc3339_millis;
use rusqlite::{types::Value as SqlValue, Connection, OpenFlags};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// Upper bound on the composer rows one scan adds to the menu. A composer that
/// needs the user (waiting, error, working) sorts ahead of the cap.
const MAX_ROWS: usize = 8;

/// One composer ready for the menu.
#[derive(Clone, Debug)]
pub struct DesktopSession {
    pub id: String,
    pub title: Option<String>,
    pub cwd: Option<PathBuf>,
    pub state: AgentState,
    pub model: Option<String>,
    pub context: Option<ContextUsage>,
    pub created: Option<SystemTime>,
    pub updated: Option<SystemTime>,
}

#[derive(Default)]
pub struct CursorDesktopAnalyzer {
    cache: Option<Cached>,
}

struct Cached {
    signature: Signature,
    sessions: Vec<DesktopSession>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Signature {
    db: (Option<SystemTime>, u64),
    wal: (Option<SystemTime>, u64),
    headers: u64,
    window: Option<Duration>,
}

impl CursorDesktopAnalyzer {
    /// The live composer rows, most actionable first, and how many the row cap
    /// left out. `window` decides how long a finished composer keeps its row;
    /// a composer that needs the user is always listed.
    pub fn overview(&mut self, window: Option<Duration>) -> (Vec<DesktopSession>, usize) {
        let Some(root) = user_dir() else {
            return (Vec::new(), 0);
        };
        self.overview_at(&root, window)
    }

    fn overview_at(
        &mut self,
        root: &Path,
        window: Option<Duration>,
    ) -> (Vec<DesktopSession>, usize) {
        let signature = signature(root, window);
        let sessions = if self
            .cache
            .as_ref()
            .is_some_and(|cached| cached.signature == signature)
        {
            self.cache
                .as_ref()
                .map(|cached| cached.sessions.clone())
                .unwrap_or_default()
        } else {
            match read_sessions(root, window) {
                Some(sessions) => {
                    self.cache = Some(Cached {
                        signature,
                        sessions: sessions.clone(),
                    });
                    sessions
                }
                // A live SQLite database can briefly refuse a read. Keep the
                // last good list instead of emptying the menu for one scan.
                None => self
                    .cache
                    .as_ref()
                    .map(|cached| cached.sessions.clone())
                    .unwrap_or_default(),
            }
        };
        listed(&sessions, window, SystemTime::now())
    }

    #[cfg(test)]
    fn overview_in(
        &mut self,
        root: &Path,
        window: Option<Duration>,
    ) -> (Vec<DesktopSession>, usize) {
        self.overview_at(root, window)
    }
}

fn user_dir() -> Option<PathBuf> {
    if let Some(override_dir) = std::env::var_os("CURSOR_DESKTOP_USER_DIR") {
        return Some(PathBuf::from(override_dir));
    }
    Some(dirs::config_dir()?.join("Cursor").join("User"))
}

fn listed(
    sessions: &[DesktopSession],
    window: Option<Duration>,
    now: SystemTime,
) -> (Vec<DesktopSession>, usize) {
    let mut sessions: Vec<DesktopSession> = sessions
        .iter()
        .filter(|session| needs_attention(session.state) || within(session.updated, window, now))
        .cloned()
        .collect();
    sessions.sort_by(|left, right| {
        attention_rank(left.state)
            .cmp(&attention_rank(right.state))
            .then_with(|| right.updated.cmp(&left.updated))
            .then_with(|| left.id.cmp(&right.id))
    });
    let hidden = sessions.len().saturating_sub(MAX_ROWS);
    sessions.truncate(MAX_ROWS);
    (sessions, hidden)
}

fn needs_attention(state: AgentState) -> bool {
    matches!(
        state,
        AgentState::Waiting | AgentState::WaitingReply | AgentState::Working | AgentState::Error
    )
}

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

fn within(updated: Option<SystemTime>, window: Option<Duration>, now: SystemTime) -> bool {
    let Some(window) = window else {
        return true;
    };
    match updated {
        Some(updated) => now
            .duration_since(updated)
            .map(|age| age < window)
            .unwrap_or(true),
        // A session without a timestamp cannot be aged out; keep it rather
        // than silently hiding a live composer.
        None => true,
    }
}

fn signature(root: &Path, window: Option<Duration>) -> Signature {
    let db = root.join("globalStorage").join("state.vscdb");
    let wal = root.join("globalStorage").join("state.vscdb-wal");
    Signature {
        db: file_state(&db),
        wal: file_state(&wal),
        headers: headers_signature(root),
        window,
    }
}

fn file_state(path: &Path) -> (Option<SystemTime>, u64) {
    match path.metadata() {
        Ok(metadata) => (metadata.modified().ok(), metadata.len()),
        Err(_) => (None, 0),
    }
}

fn headers_signature(root: &Path) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let storage = root.join("workspaceStorage");
    let Ok(entries) = fs::read_dir(&storage) else {
        return 0;
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    paths.sort();
    for path in paths {
        path.hash(&mut hasher);
        file_state(&path.join("state.vscdb")).hash(&mut hasher);
        file_state(&path.join("state.vscdb-wal")).hash(&mut hasher);
        file_state(&path.join("workspace.json")).hash(&mut hasher);
    }
    hasher.finish()
}

fn read_sessions(root: &Path, window: Option<Duration>) -> Option<Vec<DesktopSession>> {
    let db = root.join("globalStorage").join("state.vscdb");
    let connection = open_db(&db)?;
    let table = kv_table(&connection)?;
    let headers = read_headers(root);
    let composers = read_composers(&connection, table)?;
    let subagents = collect_subagents(&composers);
    let now = SystemTime::now();
    let mut sessions = Vec::new();
    for (id, data) in composers {
        if subagents.contains(&id) {
            continue;
        }
        let header = headers.get(&id);
        // A composer that finished long ago cannot produce a row, and reading
        // its bubbles (which can carry whole tool outputs) is the expensive
        // part of the scan. Only a composer inside the range, or one that is
        // still running or needs the user, is read.
        let updated = [
            ms_time(data["lastUpdatedAt"].as_i64()),
            ms_time(data["createdAt"].as_i64()),
            header.and_then(|header| header.updated),
        ]
        .into_iter()
        .flatten()
        .max();
        let status = data["status"].as_str().unwrap_or_default();
        let active = data["generatingBubbleIds"]
            .as_array()
            .is_some_and(|ids| !ids.is_empty())
            || is_generating(status)
            || header.is_some_and(|header| header.blocking)
            || data["hasBlockingPendingActions"].as_bool().unwrap_or(false)
            || data["hasUnreadMessages"]
                .as_bool()
                .or_else(|| header.and_then(|header| header.unread))
                .unwrap_or(false);
        if !active && (updated.is_none() || !within(updated, window, now)) {
            continue;
        }
        if let Some(session) = summarize(&connection, table, &id, &data, header) {
            sessions.push(session);
        }
    }
    Some(sessions)
}

fn open_db(path: &Path) -> Option<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let _ = connection.busy_timeout(Duration::from_millis(250));
    Some(connection)
}

fn table_named(connection: &Connection, name: &'static str) -> Option<&'static str> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |_| Ok(()),
        )
        .ok()
        .map(|_| name)
}

fn kv_table(connection: &Connection) -> Option<&'static str> {
    table_named(connection, "cursorDiskKV").or_else(|| table_named(connection, "ItemTable"))
}

fn read_composers(connection: &Connection, table: &str) -> Option<Vec<(String, Value)>> {
    let sql = format!("SELECT key, value FROM {table} WHERE key LIKE 'composerData:%'");
    let mut statement = connection.prepare(&sql).ok()?;
    let rows = statement
        .query_map([], |row| {
            let key: SqlValue = row.get(0)?;
            let value: SqlValue = row.get(1)?;
            Ok((key, value))
        })
        .ok()?;
    let mut composers = Vec::new();
    for row in rows.flatten() {
        let Some(value) = json_blob(&row.1) else {
            continue;
        };
        let Some(id) = value["composerId"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| key_suffix(&row.0))
        else {
            continue;
        };
        composers.push((id, value));
    }
    Some(composers)
}

fn key_suffix(value: &SqlValue) -> Option<String> {
    let text = match value {
        SqlValue::Text(text) => text.clone(),
        SqlValue::Blob(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        _ => return None,
    };
    let (_, id) = text.split_once(':')?;
    (!id.is_empty()).then(|| id.to_owned())
}

fn collect_subagents(composers: &[(String, Value)]) -> HashSet<String> {
    let mut ids = HashSet::new();
    for (_, data) in composers {
        for key in ["subComposerIds", "subagentIds", "childComposerIds"] {
            if let Some(children) = data[key].as_array() {
                for child in children.iter().filter_map(Value::as_str) {
                    ids.insert(child.to_owned());
                }
            }
        }
    }
    ids
}

/// The header copy in workspaceStorage. It carries the project folder and the
/// live blocking flag that the global document does not.
#[derive(Clone, Debug, Default)]
struct Header {
    folder: Option<PathBuf>,
    blocking: bool,
    unread: Option<bool>,
    name: Option<String>,
    subtitle: Option<String>,
    mode: Option<String>,
    draft: Option<bool>,
    archived: Option<bool>,
    updated: Option<SystemTime>,
}

fn read_headers(root: &Path) -> HashMap<String, Header> {
    let mut headers = HashMap::new();
    let storage = root.join("workspaceStorage");
    let Ok(entries) = fs::read_dir(&storage) else {
        return headers;
    };
    for entry in entries.flatten() {
        let workspace = entry.path();
        if !workspace.is_dir() {
            continue;
        }
        let folder = read_json(&workspace.join("workspace.json"))
            .and_then(|value| value["folder"].as_str().and_then(folder_from_uri));
        let db = workspace.join("state.vscdb");
        let Some(connection) = open_db(&db) else {
            continue;
        };
        // Workspace databases also carry a `cursorDiskKV` table. The header is
        // in `ItemTable` there; picking `cursorDiskKV` first (the global
        // database's layout) silently lost every project and the blocking flag.
        let Some(table) = table_named(&connection, "ItemTable")
            .or_else(|| table_named(&connection, "cursorDiskKV"))
        else {
            continue;
        };
        let sql = format!("SELECT value FROM {table} WHERE key = 'composer.composerData'");
        let Ok(value) = connection.query_row(&sql, [], |row| row.get::<_, SqlValue>(0)) else {
            continue;
        };
        let Some(data) = json_blob(&value) else {
            continue;
        };
        let Some(composers) = data["allComposers"].as_array() else {
            continue;
        };
        for composer in composers {
            let Some(id) = composer["composerId"].as_str() else {
                continue;
            };
            let header = Header {
                folder: folder.clone(),
                blocking: composer["hasBlockingPendingActions"]
                    .as_bool()
                    .unwrap_or(false),
                unread: composer["hasUnreadMessages"].as_bool(),
                name: composer["name"].as_str().and_then(clean_text),
                subtitle: composer["subtitle"].as_str().and_then(clean_text),
                mode: composer["unifiedMode"].as_str().map(str::to_owned),
                draft: composer["isDraft"].as_bool(),
                archived: composer["isArchived"].as_bool(),
                updated: ms_time(composer["lastUpdatedAt"].as_i64()),
            };
            headers.entry(id.to_owned()).or_insert(header);
        }
    }
    headers
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn folder_from_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = percent_decode(rest);
    let path = if cfg!(windows)
        && decoded.starts_with('/')
        && decoded
            .as_bytes()
            .get(1)
            .is_some_and(u8::is_ascii_alphabetic)
        && decoded.as_bytes().get(2) == Some(&b':')
    {
        &decoded[1..]
    } else {
        &decoded
    };
    Some(PathBuf::from(path))
}

fn summarize(
    connection: &Connection,
    table: &str,
    id: &str,
    data: &Value,
    header: Option<&Header>,
) -> Option<DesktopSession> {
    let mode = data["unifiedMode"]
        .as_str()
        .or_else(|| header.and_then(|header| header.mode.as_deref()));
    if mode == Some("chat") {
        return None;
    }
    if data["isSubagent"].as_bool() == Some(true) {
        return None;
    }
    if data["isDraft"]
        .as_bool()
        .or_else(|| header.and_then(|header| header.draft))
        == Some(true)
    {
        return None;
    }
    if data["isArchived"]
        .as_bool()
        .or_else(|| header.and_then(|header| header.archived))
        == Some(true)
    {
        return None;
    }
    if data["isBestOfNSubcomposer"].as_bool() == Some(true) {
        return None;
    }

    let bubbles = read_bubbles(connection, table, id, data);
    let has_bubbles = !bubbles.is_empty();
    let status = data["status"].as_str().unwrap_or_default();
    let generating = data["generatingBubbleIds"]
        .as_array()
        .is_some_and(|ids| !ids.is_empty())
        || is_generating(status);
    let blocking = data["hasBlockingPendingActions"].as_bool().unwrap_or(false)
        || header.is_some_and(|header| header.blocking);
    if !has_bubbles && !generating && !blocking {
        return None;
    }

    let mut last_role = 0i64;
    let mut last_text = String::new();
    let mut first_user = None;
    let mut model = None;
    let mut tools: HashMap<String, bool> = HashMap::new();
    let mut last_bubble_time = None;

    for bubble in &bubbles {
        if let Some(time) = bubble["createdAt"]
            .as_str()
            .and_then(parse_rfc3339_millis)
            .and_then(|millis| ms_time(Some(millis)))
        {
            last_bubble_time = Some(time);
        }
        match bubble["type"].as_i64() {
            Some(1) => {
                last_role = 1;
                last_text.clear();
                // A new prompt interrupts an unfinished tool call.
                tools.clear();
                if first_user.is_none() {
                    first_user = bubble["text"].as_str().and_then(clean_text);
                }
            }
            Some(2) => {
                last_role = 2;
                if let Some(text) = bubble["text"].as_str().filter(|text| !text.is_empty()) {
                    last_text.push_str(text);
                }
                if let Some(name) = bubble["modelInfo"]["modelName"].as_str() {
                    if name != "default" && !name.is_empty() {
                        model = Some(name.to_owned());
                    }
                }
                if let Some(tool) = bubble.get("toolFormerData") {
                    if let Some(id) = tool["toolCallId"]
                        .as_str()
                        .or_else(|| bubble["bubbleId"].as_str())
                    {
                        tools.insert(id.to_owned(), tool_in_flight(tool));
                    }
                }
            }
            _ => {}
        }
    }

    let in_flight = tools.values().any(|in_flight| *in_flight);
    // A terminal composer cannot have a tool in flight. The per-tool status is
    // written in place and is sometimes left at `started`/null when a turn is
    // aborted, which used to keep an idle composer reporting "working" for
    // months. The composer status is the turn-level truth; only an unknown
    // status falls back to the tool state.
    let terminal = is_terminal_status(status);
    let in_flight = in_flight && !terminal;
    let unread = data["hasUnreadMessages"]
        .as_bool()
        .or_else(|| header.and_then(|header| header.unread));
    let state = if blocking {
        AgentState::Waiting
    } else if is_error(status) {
        AgentState::Error
    } else if generating || in_flight || (!terminal && last_role == 1) {
        AgentState::Working
    } else if text_ends_with_question(&last_text) && unread != Some(false) {
        AgentState::WaitingReply
    } else {
        AgentState::Ready
    };

    let title = data["name"]
        .as_str()
        .and_then(clean_text)
        .or_else(|| header.and_then(|header| header.name.clone()))
        .or_else(|| data["subtitle"].as_str().and_then(clean_text))
        .or_else(|| header.and_then(|header| header.subtitle.clone()))
        .or(first_user);
    if model.is_none() {
        model = data["modelConfig"]["modelName"]
            .as_str()
            .filter(|name| *name != "default" && !name.is_empty())
            .map(str::to_owned);
    }
    let context = context_usage(data);
    let cwd = composer_cwd(data).or_else(|| header.and_then(|header| header.folder.clone()));
    let created = ms_time(data["createdAt"].as_i64());
    let updated = [
        ms_time(data["lastUpdatedAt"].as_i64()),
        header.and_then(|header| header.updated),
        last_bubble_time,
    ]
    .into_iter()
    .flatten()
    .max();

    Some(DesktopSession {
        id: id.to_owned(),
        title,
        cwd,
        state,
        model,
        context,
        created,
        updated,
    })
}

fn read_bubbles(connection: &Connection, table: &str, id: &str, data: &Value) -> Vec<Value> {
    let sql = format!("SELECT key, value FROM {table} WHERE key LIKE ?1 ORDER BY rowid");
    let Ok(mut statement) = connection.prepare(&sql) else {
        return Vec::new();
    };
    let pattern = format!("bubbleId:{id}:%");
    let Ok(rows) = statement.query_map([pattern], |row| {
        let key: SqlValue = row.get(0)?;
        let value: SqlValue = row.get(1)?;
        Ok((key, value))
    }) else {
        return Vec::new();
    };
    let mut by_id: HashMap<String, Value> = HashMap::new();
    for row in rows.flatten() {
        let Some(bubble) = json_blob(&row.1) else {
            continue;
        };
        let Some(key) = key_text(&row.0) else {
            continue;
        };
        let Some((_, bubble_id)) = key.rsplit_once(':') else {
            continue;
        };
        by_id.insert(bubble_id.to_owned(), bubble);
    }
    let mut ordered = Vec::new();
    if let Some(headers) = data["fullConversationHeadersOnly"].as_array() {
        for header in headers {
            let Some(bubble_id) = header["bubbleId"].as_str() else {
                continue;
            };
            if let Some(bubble) = by_id.remove(bubble_id) {
                ordered.push(bubble);
            }
        }
    }
    // Anything the document's order did not name (an older document, a bubble
    // written after the list) still belongs to the session.
    let mut leftovers: Vec<Value> = by_id.into_values().collect();
    leftovers.sort_by_key(|bubble| bubble["createdAt"].as_str().map(str::to_owned));
    ordered.extend(leftovers);
    ordered
}

fn key_text(value: &SqlValue) -> Option<String> {
    match value {
        SqlValue::Text(text) => Some(text.clone()),
        SqlValue::Blob(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    }
}

fn tool_in_flight(tool: &Value) -> bool {
    let status = tool["status"].as_str().unwrap_or_default();
    if status.is_empty() {
        return tool.get("result").is_none_or(Value::is_null);
    }
    !matches!(
        status,
        "completed" | "error" | "failed" | "cancelled" | "canceled" | "rejected"
    )
}

fn is_generating(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "generating" | "working" | "running" | "in_progress" | "started" | "streaming"
    )
}

fn is_error(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "error" | "failed" | "failure"
    )
}

/// A status that says the turn is over. Once the composer is over, a tool call
/// whose own status was never updated (a long terminal command that was
/// interrupted, for example) cannot still be running; its `started`/missing
/// status is stale, not a working signal.
fn is_terminal_status(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "completed"
            | "complete"
            | "done"
            | "aborted"
            | "cancelled"
            | "canceled"
            | "error"
            | "failed"
            | "failure"
    )
}

fn composer_cwd(data: &Value) -> Option<PathBuf> {
    for pointer in [
        "/workspaceIdentifier/uri/fsPath",
        "/workspaceIdentifier/fsPath",
        "/agentLocation/environment/uri/fsPath",
    ] {
        if let Some(path) = data.pointer(pointer).and_then(Value::as_str) {
            if let Some(path) = path_from_field(path) {
                return Some(path);
            }
        }
    }
    data["trackedGitRepos"]
        .as_array()?
        .first()?
        .get("repoPath")?
        .as_str()
        .and_then(path_from_field)
}

fn path_from_field(value: &str) -> Option<PathBuf> {
    if value.starts_with("file://") {
        folder_from_uri(value)
    } else if value.is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

fn context_usage(data: &Value) -> Option<ContextUsage> {
    let limit = data["contextTokenLimit"]
        .as_u64()
        .or_else(|| data["contextWindowTokens"].as_u64())?;
    if limit == 0 {
        return None;
    }
    let used = data["contextTokensUsed"]
        .as_u64()
        .or_else(|| {
            data["contextUsagePercent"]
                .as_f64()
                .map(|percent| (percent / 100.0 * limit as f64).round() as u64)
        })
        .filter(|used| *used > 0)?;
    Some(ContextUsage {
        used_tokens: used,
        window_tokens: limit,
    })
}

fn ms_time(millis: Option<i64>) -> Option<SystemTime> {
    let millis = millis.filter(|millis| *millis > 0)?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(millis as u64))
}

/// One bubble's token counts, for the usage table.
pub(crate) struct UsageRecord {
    pub at: i64,
    pub input: u64,
    pub input_cached: u64,
    pub output: u64,
}

/// Every billed bubble in the global database, keyed by its own timestamp.
/// Cursor records input and output only; there is no cache split and no cost.
pub(crate) fn usage_records(db: &Path) -> Vec<UsageRecord> {
    let Some(connection) = open_db(db) else {
        return Vec::new();
    };
    let Some(table) = kv_table(&connection) else {
        return Vec::new();
    };
    let sql = format!("SELECT value FROM {table} WHERE key LIKE 'bubbleId:%'");
    let Ok(mut statement) = connection.prepare(&sql) else {
        return Vec::new();
    };
    let Ok(rows) = statement.query_map([], |row| row.get::<_, SqlValue>(0)) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    for row in rows.flatten() {
        let Some(bubble) = json_blob(&row) else {
            continue;
        };
        let Some(at) = bubble["createdAt"]
            .as_str()
            .and_then(parse_rfc3339_millis)
            .or_else(|| bubble["createdAt"].as_i64())
        else {
            continue;
        };
        let input = bubble["tokenCount"]["inputTokens"].as_u64().unwrap_or(0);
        let output = bubble["tokenCount"]["outputTokens"].as_u64().unwrap_or(0);
        if input == 0 && output == 0 {
            continue;
        }
        records.push(UsageRecord {
            at,
            input,
            input_cached: 0,
            output,
        });
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn scratch(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "asi-cursor-desktop-{tag}-{}-{}",
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

    fn open(path: &Path, schema: &str) -> Connection {
        let connection = Connection::open(path).expect("db");
        connection.execute_batch(schema).expect("schema");
        connection
    }

    fn insert(connection: &Connection, key: &str, value: &Value) {
        connection
            .execute(
                "INSERT OR REPLACE INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
                rusqlite::params![key, value.to_string()],
            )
            .expect("insert");
    }

    fn composer(id: &str, status: &str, name: &str) -> Value {
        json!({
            "_v": 17,
            "composerId": id,
            "name": name,
            "status": status,
            "generatingBubbleIds": [],
            "unifiedMode": "agent",
            "isDraft": false,
            "modelConfig": {"modelName": "claude-4.5-sonnet-thinking"},
            "contextTokensUsed": 26221,
            "contextTokenLimit": 176000,
            "createdAt": 1_768_319_444_313i64,
            "lastUpdatedAt": 1_768_319_521_554i64,
            "hasUnreadMessages": false,
            "fullConversationHeadersOnly": []
        })
    }

    fn bubble(id: &str, kind: i64, text: &str, minutes: i64) -> Value {
        let stamp = format!("2026-01-13T15:{:02}:00.000Z", minutes);
        json!({
            "bubbleId": id,
            "type": kind,
            "text": text,
            "createdAt": stamp,
            "modelInfo": {"modelName": "claude-4.5-sonnet-thinking"},
            "tokenCount": {"inputTokens": 0, "outputTokens": 0}
        })
    }

    fn add_bubble(connection: &Connection, cid: &str, value: &Value) {
        let id = value["bubbleId"].as_str().expect("bubble id");
        insert(connection, &format!("bubbleId:{cid}:{id}"), value);
    }

    fn fixture(tag: &str) -> (PathBuf, Connection) {
        let root = scratch(tag);
        let global = root.join("globalStorage");
        fs::create_dir_all(&global).expect("global");
        let connection = open(
            &global.join("state.vscdb"),
            "CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        );
        (root, connection)
    }

    fn workspace(root: &Path, folder: &str, headers: Value) {
        let dir = root.join("workspaceStorage").join("w1");
        fs::create_dir_all(&dir).expect("workspace");
        write(
            &dir.join("workspace.json"),
            &json!({ "folder": format!("file://{folder}") }).to_string(),
        );
        let connection = open(
            &dir.join("state.vscdb"),
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT NOT NULL);\
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        );
        connection
            .execute(
                "INSERT INTO ItemTable (key, value) VALUES ('composer.composerData', ?1)",
                rusqlite::params![headers.to_string()],
            )
            .expect("header");
    }

    fn header(id: &str, name: &str, blocking: bool) -> Value {
        json!({
            "composerId": id,
            "name": name,
            "hasBlockingPendingActions": blocking,
            "hasUnreadMessages": false,
            "unifiedMode": "agent",
            "isDraft": false,
            "isArchived": false,
            "lastUpdatedAt": 1_768_319_521_554i64
        })
    }

    #[test]
    fn a_completed_composer_is_ready_with_title_model_context_and_project() {
        let (root, connection) = fixture("ready");
        let mut data = composer("c1", "completed", "Fix the parser");
        data["fullConversationHeadersOnly"] = json!([
            {"bubbleId": "u1", "type": 1},
            {"bubbleId": "a1", "type": 2}
        ]);
        insert(&connection, "composerData:c1", &data);
        add_bubble(&connection, "c1", &bubble("u1", 1, "fix the parser", 1));
        add_bubble(&connection, "c1", &bubble("a1", 2, "All done.", 2));
        workspace(
            &root,
            "/Users/me/project",
            json!({"allComposers": [header("c1", "Fix the parser", false)]}),
        );
        let (sessions, hidden) = CursorDesktopAnalyzer::default().overview_in(&root, None);
        assert_eq!(hidden, 0);
        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.state, AgentState::Ready);
        assert_eq!(session.title.as_deref(), Some("Fix the parser"));
        assert_eq!(session.cwd.as_deref(), Some(Path::new("/Users/me/project")));
        assert_eq!(session.model.as_deref(), Some("claude-4.5-sonnet-thinking"));
        assert_eq!(
            session.context,
            Some(ContextUsage {
                used_tokens: 26221,
                window_tokens: 176000
            })
        );
    }

    #[test]
    fn started_tools_work_only_while_the_composer_is_running() {
        let (root, connection) = fixture("working");
        let mut data = composer("c1", "generating", "Fix the parser");
        data["generatingBubbleIds"] = json!(["a1"]);
        data["fullConversationHeadersOnly"] = json!([{"bubbleId": "u1", "type": 1}]);
        insert(&connection, "composerData:c1", &data);
        add_bubble(&connection, "c1", &bubble("u1", 1, "fix the parser", 1));
        workspace(
            &root,
            "/p",
            json!({"allComposers": [header("c1", "Fix", false)]}),
        );
        let (sessions, _) = CursorDesktopAnalyzer::default().overview_in(&root, None);
        assert_eq!(sessions[0].state, AgentState::Working);

        let (root, connection) = fixture("tool");
        let mut data = composer("c1", "none", "Run the tests");
        data["fullConversationHeadersOnly"] = json!([
            {"bubbleId": "u1", "type": 1},
            {"bubbleId": "a1", "type": 2}
        ]);
        insert(&connection, "composerData:c1", &data);
        add_bubble(&connection, "c1", &bubble("u1", 1, "run tests", 1));
        let mut tool = bubble("a1", 2, "", 2);
        tool["toolFormerData"] =
            json!({"toolCallId": "t1", "status": "started", "name": "run_terminal_command_v2"});
        add_bubble(&connection, "c1", &tool);
        workspace(
            &root,
            "/p",
            json!({"allComposers": [header("c1", "Run", false)]}),
        );
        let (sessions, _) = CursorDesktopAnalyzer::default().overview_in(&root, None);
        assert_eq!(sessions[0].state, AgentState::Working);

        // The same started tool under an aborted turn is stale: Cursor writes
        // the tool status in place and can leave it unset when the user stops
        // the turn. The terminal composer status must win.
        let (root, connection) = fixture("aborted-tool");
        let mut data = composer("c1", "aborted", "Stopped");
        data["fullConversationHeadersOnly"] = json!([
            {"bubbleId": "u1", "type": 1},
            {"bubbleId": "a1", "type": 2}
        ]);
        insert(&connection, "composerData:c1", &data);
        add_bubble(&connection, "c1", &bubble("u1", 1, "run tests", 1));
        let mut tool = bubble("a1", 2, "", 2);
        tool["toolFormerData"] = json!({
            "toolCallId": "t1",
            "name": "run_terminal_command_v2"
        });
        add_bubble(&connection, "c1", &tool);
        workspace(
            &root,
            "/p",
            json!({"allComposers": [header("c1", "Stopped", false)]}),
        );
        let (sessions, _) = CursorDesktopAnalyzer::default().overview_in(&root, None);
        assert_eq!(sessions[0].state, AgentState::Ready);
    }

    #[test]
    fn blocking_pending_actions_wait_for_the_user() {
        let (root, connection) = fixture("blocking");
        insert(
            &connection,
            "composerData:c1",
            &composer("c1", "completed", "Deploy"),
        );
        add_bubble(&connection, "c1", &bubble("u1", 1, "deploy", 1));
        workspace(
            &root,
            "/p",
            json!({"allComposers": [header("c1", "Deploy", true)]}),
        );
        let (sessions, _) = CursorDesktopAnalyzer::default().overview_in(&root, None);
        assert_eq!(sessions[0].state, AgentState::Waiting);
    }

    #[test]
    fn an_aborted_composer_is_ready_and_a_failed_one_is_an_error() {
        let (root, connection) = fixture("aborted");
        insert(
            &connection,
            "composerData:c1",
            &composer("c1", "aborted", "Stop"),
        );
        add_bubble(&connection, "c1", &bubble("u1", 1, "go", 1));
        add_bubble(&connection, "c1", &bubble("a1", 2, "I was doing", 2));
        workspace(
            &root,
            "/p",
            json!({"allComposers": [header("c1", "Stop", false)]}),
        );
        let (sessions, _) = CursorDesktopAnalyzer::default().overview_in(&root, None);
        assert_eq!(sessions[0].state, AgentState::Ready);

        let (root, connection) = fixture("failed");
        insert(
            &connection,
            "composerData:c1",
            &composer("c1", "error", "Fail"),
        );
        add_bubble(&connection, "c1", &bubble("u1", 1, "go", 1));
        add_bubble(&connection, "c1", &bubble("a1", 2, "I failed", 2));
        workspace(
            &root,
            "/p",
            json!({"allComposers": [header("c1", "Fail", false)]}),
        );
        let (sessions, _) = CursorDesktopAnalyzer::default().overview_in(&root, None);
        assert_eq!(sessions[0].state, AgentState::Error);
    }

    #[test]
    fn an_unread_question_waits_for_a_reply_but_a_read_one_does_not() {
        let (root, connection) = fixture("question");
        let mut data = composer("c1", "completed", "Fix the parser");
        data["hasUnreadMessages"] = json!(true);
        data["fullConversationHeadersOnly"] = json!([
            {"bubbleId": "u1", "type": 1},
            {"bubbleId": "a1", "type": 2}
        ]);
        insert(&connection, "composerData:c1", &data);
        add_bubble(&connection, "c1", &bubble("u1", 1, "fix it", 1));
        add_bubble(
            &connection,
            "c1",
            &bubble("a1", 2, "Should I delete it?", 2),
        );
        workspace(
            &root,
            "/p",
            json!({"allComposers": [header("c1", "Fix", false)]}),
        );
        let (sessions, _) = CursorDesktopAnalyzer::default().overview_in(&root, None);
        assert_eq!(sessions[0].state, AgentState::WaitingReply);

        let (root, connection) = fixture("read-question");
        let mut data = composer("c1", "completed", "Fix the parser");
        data["hasUnreadMessages"] = json!(false);
        data["fullConversationHeadersOnly"] = json!([
            {"bubbleId": "u1", "type": 1},
            {"bubbleId": "a1", "type": 2}
        ]);
        insert(&connection, "composerData:c1", &data);
        add_bubble(&connection, "c1", &bubble("u1", 1, "fix it", 1));
        add_bubble(
            &connection,
            "c1",
            &bubble("a1", 2, "Should I delete it?", 2),
        );
        workspace(
            &root,
            "/p",
            json!({"allComposers": [header("c1", "Fix", false)]}),
        );
        let (sessions, _) = CursorDesktopAnalyzer::default().overview_in(&root, None);
        assert_eq!(sessions[0].state, AgentState::Ready);
    }

    #[test]
    fn chat_drafts_and_subagents_are_not_rows() {
        let (root, connection) = fixture("skip");
        let mut chat = composer("chat", "completed", "Chat");
        chat["unifiedMode"] = json!("chat");
        insert(&connection, "composerData:chat", &chat);
        let mut draft = composer("draft", "generating", "Draft");
        draft["isDraft"] = json!(true);
        insert(&connection, "composerData:draft", &draft);
        let mut parent = composer("parent", "completed", "Parent");
        parent["subComposerIds"] = json!(["child"]);
        insert(&connection, "composerData:parent", &parent);
        add_bubble(&connection, "parent", &bubble("u1", 1, "parent", 1));
        insert(
            &connection,
            "composerData:child",
            &composer("child", "completed", "Child"),
        );
        let (sessions, _) = CursorDesktopAnalyzer::default().overview_in(&root, None);
        assert_eq!(sessions.len(), 1, "{sessions:?}");
        assert_eq!(sessions[0].id, "parent");
    }

    #[test]
    fn the_window_hides_old_finished_rows_but_keeps_working_ones() {
        let (root, connection) = fixture("window");
        let mut old = composer("old", "completed", "Old");
        old["lastUpdatedAt"] = json!(1_000_000i64);
        insert(&connection, "composerData:old", &old);
        add_bubble(&connection, "old", &bubble("u1", 1, "old", 1));
        add_bubble(&connection, "old", &bubble("a1", 2, "done", 2));
        let mut working = composer("working", "generating", "Working");
        working["lastUpdatedAt"] = json!(1_000_000i64);
        insert(&connection, "composerData:working", &working);
        add_bubble(&connection, "working", &bubble("u1", 1, "work", 1));
        workspace(
            &root,
            "/p",
            json!({"allComposers": [header("old", "Old", false), header("working", "Working", false)]}),
        );
        let (sessions, _) =
            CursorDesktopAnalyzer::default().overview_in(&root, Some(Duration::from_secs(3600)));
        assert_eq!(sessions.len(), 1, "{sessions:?}");
        assert_eq!(sessions[0].id, "working");
    }

    #[test]
    fn bubbles_without_a_header_order_still_work() {
        let (root, connection) = fixture("order");
        insert(
            &connection,
            "composerData:c1",
            &composer("c1", "completed", "No order"),
        );
        add_bubble(&connection, "c1", &bubble("u1", 1, "hello", 1));
        add_bubble(&connection, "c1", &bubble("a1", 2, "hi", 2));
        workspace(
            &root,
            "/p",
            json!({"allComposers": [header("c1", "No order", false)]}),
        );
        let (sessions, _) = CursorDesktopAnalyzer::default().overview_in(&root, None);
        assert_eq!(sessions[0].state, AgentState::Ready);
        assert_eq!(sessions[0].title.as_deref(), Some("No order"));
    }

    #[test]
    fn usage_reads_each_bubble_once() {
        let (root, connection) = fixture("usage");
        let mut first = bubble("a1", 2, "hi", 1);
        first["tokenCount"] = json!({"inputTokens": 100, "outputTokens": 10});
        add_bubble(&connection, "c1", &first);
        let mut second = bubble("a2", 2, "there", 2);
        second["tokenCount"] = json!({"inputTokens": 50, "outputTokens": 5});
        add_bubble(&connection, "c1", &second);
        let db = root.join("globalStorage").join("state.vscdb");
        let records = usage_records(&db);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].input, 100);
        assert_eq!(records[0].output, 10);
        assert_eq!(records[1].input, 50);
    }

    #[test]
    fn a_file_uri_decodes_to_a_project_path() {
        assert_eq!(
            folder_from_uri("file:///Users/me/my%20project").as_deref(),
            Some(Path::new("/Users/me/my project"))
        );
    }
}
