//! Cursor CLI (`cursor-agent`) sessions.
//!
//! Cursor keeps a resumable chat store per session under
//! `~/.cursor/chats/<md5(workspace)>/<session-id>/`. The directory holds
//! `meta.json` (title and timestamps), `prompt_history.json`, and `store.db`, a
//! small SQLite database with content-addressed `blobs` and a `meta` record.
//!
//! Conversation messages are the JSON blobs in `rowid` order. Cursor also stores
//! protobuf turn-graph blobs in the same table; those never parse as JSON and are
//! skipped. A tool call is unfinished when no `tool-result` blob with its id has
//! been written yet, which is the store's "working" signal. The format is closed
//! source, so every parse is tolerant: an unknown role or block is ignored
//! rather than failing the row.

use crate::agent_sessions::{
    bind_sessions, clean_text, content_text, text_ends_with_question, AgentFacts, Candidate,
    LiveSession,
};
use crate::model::AgentState;
use rusqlite::{types::Value as SqlValue, Connection, OpenFlags};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

const CACHE_LIFETIME: Duration = Duration::from_secs(86_400);

#[derive(Default)]
pub struct CursorAnalyzer {
    cache: HashMap<PathBuf, Cached>,
}

struct Cached {
    signature: Signature,
    facts: AgentFacts,
    last_access: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Signature {
    meta: (Option<SystemTime>, u64),
    store: (Option<SystemTime>, u64),
    /// Cursor keeps SQLite in WAL mode, so new turns may land in the sidecar
    /// while `store.db` itself is untouched. Without this a live agent would
    /// keep reading its first parse.
    wal: (Option<SystemTime>, u64),
}

impl CursorAnalyzer {
    /// Resolve one session per live `cursor-agent` process in `cwd`.
    pub fn analyze(&mut self, cwd: &Path, live: &[LiveSession]) -> Vec<Option<AgentFacts>> {
        if live.is_empty() {
            return Vec::new();
        }
        let Some(root) = chats_root() else {
            return live.iter().map(|_| None).collect();
        };
        self.analyze_in(&root, cwd, live)
    }

    fn analyze_in(
        &mut self,
        root: &Path,
        cwd: &Path,
        live: &[LiveSession],
    ) -> Vec<Option<AgentFacts>> {
        let Some(bucket) = workspace_bucket(root, cwd) else {
            return live.iter().map(|_| None).collect();
        };
        let candidates = session_candidates(&bucket);
        if candidates.is_empty() {
            return live.iter().map(|_| None).collect();
        }
        bind_sessions(live, &candidates)
            .into_iter()
            .map(|claim| claim.and_then(|index| self.read_facts(&candidates[index].path)))
            .collect()
    }

    fn read_facts(&mut self, directory: &Path) -> Option<AgentFacts> {
        let store = directory.join("store.db");
        let meta_path = directory.join("meta.json");
        let signature = Signature {
            meta: file_state(&meta_path),
            store: file_state(&store),
            wal: file_state(&directory.join("store.db-wal")),
        };
        if let Some(cached) = self.cache.get_mut(directory) {
            cached.last_access = Instant::now();
            if cached.signature == signature {
                return Some(cached.facts.clone());
            }
        }
        let meta = read_json(&meta_path).unwrap_or(Value::Null);
        let facts = facts_from_store(&store, &meta)?;
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

fn file_state(path: &Path) -> (Option<SystemTime>, u64) {
    match path.metadata() {
        Ok(metadata) => (metadata.modified().ok(), metadata.len()),
        Err(_) => (None, 0),
    }
}

fn chats_root() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".cursor/chats"))
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// The workspace bucket Cursor named after the MD5 of the absolute path. The
/// CLI is the only writer of that name, so the variants only cover the path
/// spellings the process table may hand back (case folding on case-insensitive
/// platforms and backslashes on Windows).
fn workspace_bucket(root: &Path, cwd: &Path) -> Option<PathBuf> {
    let raw = cwd.to_string_lossy().into_owned();
    let slashed = raw.replace('\\', "/");
    let mut variants = vec![raw, slashed.clone()];
    if let Ok(canonical) = fs::canonicalize(cwd) {
        let canonical = canonical.to_string_lossy().replace('\\', "/");
        variants.push(canonical.clone());
        variants.push(canonical.to_ascii_lowercase());
    }
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        variants.push(slashed.to_ascii_lowercase());
    }
    variants.dedup();
    variants.into_iter().find_map(|variant| {
        let bucket = root.join(md5_hex(variant.as_bytes()));
        bucket.is_dir().then_some(bucket)
    })
}

fn session_candidates(bucket: &Path) -> Vec<Candidate> {
    let Ok(entries) = fs::read_dir(bucket) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let directory = entry.path();
            if !directory.join("store.db").is_file() {
                return None;
            }
            let meta = read_json(&directory.join("meta.json")).unwrap_or(Value::Null);
            let file_modified = [
                directory
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok()),
                file_state(&directory.join("store.db")).0,
                file_state(&directory.join("store.db-wal")).0,
            ]
            .into_iter()
            .flatten()
            .max();
            let parsed_updated = meta["updatedAtMs"].as_i64().map(millis_time);
            // A resumed chat's `updatedAtMs` can predate the process while the
            // store is already being touched; the file's mtime decides binding.
            let modified = match (parsed_updated, file_modified) {
                (Some(parsed), Some(file)) => parsed.max(file),
                (Some(parsed), None) => parsed,
                (None, Some(file)) => file,
                (None, None) => SystemTime::UNIX_EPOCH,
            };
            let created = meta["createdAtMs"]
                .as_i64()
                .map(millis_time)
                .or_else(|| {
                    directory
                        .metadata()
                        .ok()
                        .and_then(|metadata| metadata.created().ok())
                })
                .unwrap_or(modified);
            Some(Candidate {
                path: directory,
                created,
                modified,
            })
        })
        .collect()
}

fn millis_time(millis: i64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_millis(millis.max(0) as u64)
}

fn facts_from_store(store: &Path, meta: &Value) -> Option<AgentFacts> {
    let connection = Connection::open_with_flags(
        store,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let _ = connection.busy_timeout(Duration::from_millis(250));
    let mut statement = connection
        .prepare("SELECT CAST(data AS BLOB) FROM blobs ORDER BY rowid")
        .ok()?;
    let rows = statement
        .query_map([], |row| row.get::<_, SqlValue>(0))
        .ok()?;

    let mut pending: HashSet<String> = HashSet::new();
    let mut first_user = None;
    let mut model = None;
    let mut last_role: Option<String> = None;
    let mut last_text = String::new();
    let mut any_message = false;

    for row in rows.flatten() {
        let Some(value) = json_blob(&row) else {
            continue;
        };
        let Some(role) = value["role"].as_str() else {
            continue;
        };
        any_message = true;
        match role {
            "user" => {
                // The editor injects context as a `user` blob too; it is not a
                // turn and must not restart the state machine.
                let injected = content_text(&value["content"]).is_some_and(|text| {
                    text.contains("<user_info>") && !text.contains("<user_query>")
                });
                if injected {
                    continue;
                }
                // A prompt sent while a tool call is in flight interrupts it;
                // Cursor writes no result for that call.
                pending.clear();
                last_role = Some("user".to_owned());
                last_text.clear();
                if first_user.is_none() {
                    first_user = user_text(&value["content"]);
                }
            }
            "assistant" => {
                last_role = Some("assistant".to_owned());
                last_text = assistant_text(&value["content"]);
                if let Some(name) = model_name(&value) {
                    model = Some(name);
                }
                for id in tool_calls(&value["content"]) {
                    pending.insert(id);
                }
                // A few builds also keep the calls beside the content blocks.
                if let Some(calls) = value["toolCalls"].as_array() {
                    for call in calls {
                        if let Some(id) =
                            call["toolCallId"].as_str().or_else(|| call["id"].as_str())
                        {
                            pending.insert(id.to_owned());
                        }
                    }
                }
            }
            "tool" => {
                for id in tool_results(&value["content"]) {
                    pending.remove(&id);
                }
                if let Some(id) = value["toolCallId"]
                    .as_str()
                    .or_else(|| value["tool_use_id"].as_str())
                {
                    pending.remove(id);
                }
                last_role = Some("tool".to_owned());
                last_text.clear();
            }
            _ => continue,
        }
    }

    let state = if !pending.is_empty() {
        Some(AgentState::Working)
    } else {
        match last_role.as_deref() {
            Some("assistant") => Some(if text_ends_with_question(&last_text) {
                AgentState::WaitingReply
            } else {
                AgentState::Ready
            }),
            Some("user" | "tool") => Some(AgentState::Working),
            _ => None,
        }
    };
    if !any_message {
        return None;
    }

    let mut facts = AgentFacts {
        state,
        model,
        context: None,
        title: meta["title"].as_str().and_then(clean_text).or(first_user),
    };
    if facts.model.is_none() {
        facts.model = agent_record_model(store);
    }
    Some(facts)
}

pub(crate) fn json_blob(value: &SqlValue) -> Option<Value> {
    let bytes = match value {
        SqlValue::Blob(bytes) => bytes.as_slice(),
        SqlValue::Text(text) => text.as_bytes(),
        _ => return None,
    };
    serde_json::from_slice(bytes).ok()
}

fn model_name(value: &Value) -> Option<String> {
    value["providerOptions"]["cursor"]["modelName"]
        .as_str()
        .or_else(|| value["providerOptions"]["cursor"]["model"].as_str())
        .and_then(clean_text)
}

fn tool_calls(content: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(blocks) = content.as_array() {
        for block in blocks {
            if !matches!(
                block["type"].as_str(),
                Some("tool-call" | "tool_use" | "toolCall")
            ) {
                continue;
            }
            if let Some(id) = block["toolCallId"]
                .as_str()
                .or_else(|| block["tool_use_id"].as_str())
                .or_else(|| block["id"].as_str())
            {
                ids.push(id.to_owned());
            }
        }
    }
    ids
}

fn tool_results(content: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(blocks) = content.as_array() {
        for block in blocks {
            if !matches!(
                block["type"].as_str(),
                Some("tool-result" | "tool_result" | "toolResult")
            ) {
                continue;
            }
            if let Some(id) = block["toolCallId"]
                .as_str()
                .or_else(|| block["tool_use_id"].as_str())
                .or_else(|| block["id"].as_str())
            {
                ids.push(id.to_owned());
            }
        }
    }
    ids
}

fn assistant_text(content: &Value) -> String {
    let Some(blocks) = content.as_array() else {
        return content_text(content).unwrap_or_default();
    };
    blocks
        .iter()
        .filter(|block| matches!(block["type"].as_str(), Some("text")))
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}

/// The first real user prompt. Editor-injected `<user_info>` context is not a
/// prompt, and the CLI wraps the actual text in `<user_query>`.
fn user_text(content: &Value) -> Option<String> {
    let text = content_text(content)?;
    if let Some(query) = between(&text, "<user_query>", "</user_query>") {
        return clean_text(query);
    }
    if text.contains("<user_info>") {
        return None;
    }
    clean_text(&text)
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let start = text.find(start)? + start.len();
    let rest = &text[start..];
    let end = rest.find(end)?;
    Some(&rest[..end])
}

/// The model from the session's agent record, used when no assistant blob
/// carried one. The record is stored as JSON in the `meta` table under key `0`,
/// sometimes hex-encoded.
fn agent_record_model(store: &Path) -> Option<String> {
    let connection = Connection::open_with_flags(
        store,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let _ = connection.busy_timeout(Duration::from_millis(250));
    let value = connection
        .query_row(
            "SELECT CAST(value AS BLOB) FROM meta WHERE key = '0'",
            [],
            |row| row.get::<_, SqlValue>(0),
        )
        .ok()?;
    let record = json_blob(&value).or_else(|| {
        let text = match &value {
            SqlValue::Text(text) => text.as_bytes(),
            SqlValue::Blob(bytes) => bytes.as_slice(),
            _ => return None,
        };
        let text = std::str::from_utf8(text).ok()?;
        let decoded = hex_decode(text.trim())?;
        serde_json::from_slice(&decoded).ok()
    })?;
    record["lastUsedModel"]
        .as_str()
        .or_else(|| record["model"].as_str())
        .and_then(clean_text)
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if text.len() & 1 == 1 || !text.is_ascii() {
        return None;
    }
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        decoded.push(high * 16 + low);
    }
    Some(decoded)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// MD5, written out because Cursor's bucket name is the only use for it and a
/// one-function implementation avoids a dependency. RFC 1321.
pub(crate) fn md5_hex(input: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    let mut bytes = input.to_vec();
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    bytes.push(0x80);
    while bytes.len() % 64 != 56 {
        bytes.push(0);
    }
    bytes.extend_from_slice(&bit_len.to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    for chunk in bytes.chunks(64) {
        let mut words = [0u32; 16];
        for (index, word) in words.iter_mut().enumerate() {
            let offset = index * 4;
            *word = u32::from_le_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for index in 0..64 {
            let (f, g) = match index {
                0..=15 => ((b & c) | (!b & d), index),
                16..=31 => ((d & b) | (!d & c), (5 * index + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * index + 5) % 16),
                _ => (c ^ (b | !d), (7 * index) % 16),
            };
            let next = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(K[index])
                    .wrapping_add(words[g])
                    .rotate_left(S[index]),
            );
            a = d;
            d = c;
            c = b;
            b = next;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut digest = String::with_capacity(32);
    for word in [a0, b0, c0, d0] {
        for byte in word.to_le_bytes() {
            digest.push_str(&format!("{byte:02x}"));
        }
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scratch(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "asi-cursor-{tag}-{}-{}",
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

    fn session_dir(root: &Path, cwd: &Path, id: &str) -> PathBuf {
        let directory = root
            .join(md5_hex(cwd.to_string_lossy().as_bytes()))
            .join(id);
        fs::create_dir_all(&directory).expect("session dir");
        let store = Connection::open(directory.join("store.db")).expect("db");
        store
            .execute_batch(
                "CREATE TABLE blobs (id text PRIMARY KEY, data blob);\
                 CREATE TABLE meta (key text PRIMARY KEY, value blob);",
            )
            .expect("schema");
        directory
    }

    fn insert(connection: &Connection, order: usize, value: &Value) {
        connection
            .execute(
                "INSERT INTO blobs (id, data) VALUES (?1, ?2)",
                rusqlite::params![format!("blob-{order}"), value.to_string()],
            )
            .expect("insert blob");
    }

    fn analyze(root: &Path, cwd: &str) -> Option<AgentFacts> {
        let directory = root
            .join(md5_hex(cwd.as_bytes()))
            .join("019edf9c-0000-7000-8000-000000000001");
        let started = directory
            .join("meta.json")
            .metadata()
            .unwrap()
            .modified()
            .unwrap();
        let mut analyzer = CursorAnalyzer::default();
        analyzer
            .analyze_in(root, Path::new(cwd), &[LiveSession { started }])
            .into_iter()
            .next()
            .flatten()
    }

    fn fixture(tag: &str, cwd: &str, blobs: &[Value]) -> PathBuf {
        let root = scratch(tag);
        let directory = session_dir(
            &root,
            Path::new(cwd),
            "019edf9c-0000-7000-8000-000000000001",
        );
        write(
            &directory.join("meta.json"),
            &json!({
                "schemaVersion": 1,
                "title": "Fix the parser",
                "createdAtMs": 1_789_000_000_000i64,
                "updatedAtMs": 1_789_000_100_000i64,
                "hasConversation": true
            })
            .to_string(),
        );
        let connection = Connection::open(directory.join("store.db")).expect("db");
        for (order, blob) in blobs.iter().enumerate() {
            insert(&connection, order, blob);
        }
        root
    }

    #[test]
    fn md5_matches_the_rfc_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn a_prompt_with_no_answer_is_working() {
        let root = fixture(
            "prompt",
            "/Users/me/project",
            &[json!({"role": "user", "content": "<user_query>fix it</user_query>"})],
        );
        let facts = analyze(&root, "/Users/me/project").expect("facts");
        assert_eq!(facts.state, Some(AgentState::Working));
        assert_eq!(facts.title.as_deref(), Some("Fix the parser"));
    }

    #[test]
    fn a_finished_answer_is_ready_or_waits_for_a_reply() {
        let root = fixture(
            "answer",
            "/Users/me/project",
            &[
                json!({"role": "user", "content": "<user_query>fix it</user_query>"}),
                json!({
                    "role": "assistant",
                    "content": [{"type": "text", "text": "All done."}],
                    "providerOptions": {"cursor": {"modelName": "composer-2.5"}}
                }),
            ],
        );
        let facts = analyze(&root, "/Users/me/project").expect("facts");
        assert_eq!(facts.state, Some(AgentState::Ready));
        assert_eq!(facts.model.as_deref(), Some("composer-2.5"));

        let root = fixture(
            "question",
            "/Users/me/project",
            &[
                json!({"role": "user", "content": "<user_query>fix it</user_query>"}),
                json!({
                    "role": "assistant",
                    "content": [{"type": "text", "text": "Should I delete the old file?"}]
                }),
            ],
        );
        assert_eq!(
            analyze(&root, "/Users/me/project").unwrap().state,
            Some(AgentState::WaitingReply)
        );
    }

    #[test]
    fn a_tool_call_without_a_result_is_working() {
        let root = fixture(
            "tool",
            "/Users/me/project",
            &[
                json!({"role": "user", "content": "<user_query>list</user_query>"}),
                json!({
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Reading the file."},
                        {"type": "tool-call", "toolCallId": "c1", "toolName": "Read", "args": {"path": "a"}}
                    ]
                }),
            ],
        );
        assert_eq!(
            analyze(&root, "/Users/me/project").unwrap().state,
            Some(AgentState::Working)
        );
    }

    #[test]
    fn a_tool_result_closes_the_call() {
        let root = fixture(
            "tool-result",
            "/Users/me/project",
            &[
                json!({"role": "user", "content": "<user_query>list</user_query>"}),
                json!({
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Reading the file."},
                        {"type": "tool-call", "toolCallId": "c1", "toolName": "Read", "args": {"path": "a"}}
                    ]
                }),
                json!({
                    "role": "tool",
                    "content": [{"type": "tool-result", "toolCallId": "c1", "isError": false, "result": "contents"}]
                }),
                json!({"role": "assistant", "content": [{"type": "text", "text": "The file is empty."}]}),
            ],
        );
        assert_eq!(
            analyze(&root, "/Users/me/project").unwrap().state,
            Some(AgentState::Ready)
        );
    }

    #[test]
    fn the_agent_record_supplies_the_model_when_no_message_does() {
        let root = fixture(
            "meta-model",
            "/Users/me/project",
            &[json!({"role": "user", "content": "hi"})],
        );
        let store = root
            .join(md5_hex(b"/Users/me/project"))
            .join("019edf9c-0000-7000-8000-000000000001");
        let connection = Connection::open(store.join("store.db")).expect("db");
        let record = json!({
            "agentId": "a1",
            "workspacePath": "/Users/me/project",
            "lastUsedModel": "composer-2.5",
            "approvalMode": "ask"
        });
        connection
            .execute(
                "INSERT INTO meta (key, value) VALUES ('0', ?1)",
                rusqlite::params![record.to_string()],
            )
            .expect("meta");
        assert_eq!(
            analyze(&root, "/Users/me/project")
                .unwrap()
                .model
                .as_deref(),
            Some("composer-2.5")
        );
    }

    #[test]
    fn a_hex_encoded_agent_record_is_decoded() {
        assert_eq!(hex_decode("7b7d").as_deref(), Some(&b"{}"[..]));
        assert_eq!(hex_decode("zz"), None);
    }

    #[test]
    fn protobuf_blobs_are_ignored() {
        let root = scratch("protobuf");
        let directory = session_dir(
            &root,
            Path::new("/Users/me/project"),
            "019edf9c-0000-7000-8000-000000000001",
        );
        write(
            &directory.join("meta.json"),
            &json!({"title": "proto"}).to_string(),
        );
        let connection = Connection::open(directory.join("store.db")).expect("db");
        connection
            .execute(
                "INSERT INTO blobs (id, data) VALUES ('p', ?1)",
                rusqlite::params![vec![0x0au8, 0x08, 0x01]],
            )
            .expect("insert");
        assert!(analyze(&root, "/Users/me/project").is_none());
    }

    #[test]
    fn the_bucket_name_is_the_md5_of_the_workspace() {
        let root = scratch("bucket");
        let cwd = Path::new("/Users/me/project");
        let bucket = root.join(md5_hex(cwd.to_string_lossy().as_bytes()));
        fs::create_dir_all(&bucket).expect("bucket");
        assert_eq!(workspace_bucket(&root, cwd), Some(bucket));
    }
}
