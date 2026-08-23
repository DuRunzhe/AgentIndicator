use crate::model::{AgentState, ContextUsage};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Clone, Debug, Default)]
pub struct OpenCodeFacts {
    pub state: Option<AgentState>,
    pub model: Option<String>,
    pub context: Option<ContextUsage>,
    session_id: Option<String>,
}

#[derive(Default)]
pub struct OpenCodeAnalyzer {
    cache: HashMap<PathBuf, CacheEntry>,
}

struct CacheEntry {
    signature: Signature,
    facts: Option<OpenCodeFacts>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Signature {
    database: Option<SystemTime>,
    wal: Option<SystemTime>,
    models: Option<SystemTime>,
    log: Option<SystemTime>,
}

impl OpenCodeAnalyzer {
    pub fn analyze(&mut self, cwd: &Path) -> Option<OpenCodeFacts> {
        let home = dirs::home_dir()?;
        let database = home.join(".local/share/opencode/opencode.db");
        let models = home.join(".cache/opencode/models.json");
        let log = home.join(".local/share/opencode/log/opencode.log");
        let signature = Signature {
            database: modified(&database),
            wal: modified(&database.with_extension("db-wal")),
            models: modified(&models),
            log: modified(&log),
        };
        if let Some(entry) = self.cache.get(cwd) {
            if entry.signature == signature {
                return entry.facts.clone();
            }
        }
        let mut facts = query(&database, &models, cwd);
        if let Some(value) = facts.as_mut() {
            if let Some(realtime) = runtime_state_from_log(&log, value.session_id.as_deref()) {
                value.state = Some(merge_realtime_state(value.state, realtime));
            }
        }
        self.cache.insert(
            cwd.to_owned(),
            CacheEntry {
                signature,
                facts: facts.clone(),
            },
        );
        facts
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    path.metadata().ok()?.modified().ok()
}

fn query(database: &Path, models: &Path, cwd: &Path) -> Option<OpenCodeFacts> {
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    connection
        .busy_timeout(std::time::Duration::from_millis(250))
        .ok()?;
    let mut statement = connection.prepare(SQL).ok()?;
    let row = statement
        .query_row([cwd.to_string_lossy().as_ref()], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .ok()?;
    let catalog = fs::read(models)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let mut facts = facts_from_values(
        row.1.as_deref(),
        row.2.as_deref(),
        row.3.as_deref(),
        row.4.as_deref(),
        catalog.as_ref(),
    );
    facts.session_id = row.0;
    Some(facts)
}

const SQL: &str = r#"
WITH latest_session AS (
  SELECT id, model FROM session
  WHERE directory = ?1 AND parent_id IS NULL AND time_archived IS NULL
  ORDER BY time_updated DESC LIMIT 1
), latest_message AS (
  SELECT message.data FROM message JOIN latest_session ON latest_session.id = message.session_id
  ORDER BY message.time_created DESC, message.id DESC LIMIT 1
), latest_assistant AS (
  SELECT message.id, message.data FROM message JOIN latest_session ON latest_session.id = message.session_id
  WHERE json_extract(message.data, '$.role') = 'assistant'
  ORDER BY message.time_created DESC, message.id DESC LIMIT 1
), latest_text AS (
  SELECT json_extract(part.data, '$.text') FROM part JOIN latest_assistant ON latest_assistant.id = part.message_id
  WHERE json_extract(part.data, '$.type') = 'text'
  ORDER BY part.time_created DESC, part.id DESC LIMIT 1
)
SELECT latest_session.id, latest_session.model, latest_message.data, latest_assistant.data,
       (SELECT * FROM latest_text)
FROM latest_session LEFT JOIN latest_message LEFT JOIN latest_assistant
"#;

fn facts_from_values(
    session_model: Option<&str>,
    latest_message: Option<&str>,
    latest_assistant: Option<&str>,
    assistant_text: Option<&str>,
    catalog: Option<&Value>,
) -> OpenCodeFacts {
    let session = parse(session_model);
    let message = parse(latest_message);
    let assistant = parse(latest_assistant);
    let (provider, model_id) = model_identity(session.as_ref(), assistant.as_ref());
    let model = model_id.as_ref().map(|id| match provider.as_ref() {
        Some(provider) => format!("{provider}/{id}"),
        None => id.clone(),
    });
    let context = assistant
        .as_ref()
        .and_then(|value| value.pointer("/tokens/total")?.as_u64())
        .zip(model_id.as_ref())
        .and_then(|(used_tokens, id)| {
            let provider = provider.as_ref()?;
            let window_tokens = catalog?
                .pointer(&format!(
                    "/{}/models/{}/limit/context",
                    escape_pointer(provider),
                    escape_pointer(id)
                ))?
                .as_u64()?;
            Some(ContextUsage {
                used_tokens,
                window_tokens,
            })
        });
    OpenCodeFacts {
        state: Some(message.as_ref().map_or(AgentState::Ready, |value| {
            state_from_message(value, assistant_text)
        })),
        model,
        context,
        session_id: None,
    }
}

fn runtime_state_from_log(path: &Path, session_id: Option<&str>) -> Option<AgentState> {
    let session_id = session_id?;
    let bytes = fs::read(path).ok()?;
    let start = bytes.len().saturating_sub(256 * 1024);
    let tail = String::from_utf8_lossy(&bytes[start..]);
    tail.lines()
        .rev()
        .filter(|line| line.contains(&format!("session.id={session_id}")))
        .find_map(|line| {
            if line.contains("message=\"exiting loop\"") {
                Some(AgentState::Ready)
            } else if line.contains("message=loop")
                || line.contains("message=process")
                || line.contains("message=stream")
            {
                Some(AgentState::Working)
            } else {
                None
            }
        })
}

fn merge_realtime_state(database: Option<AgentState>, realtime: AgentState) -> AgentState {
    match (database, realtime) {
        // A new generation loop must clear a reply request from the previous turn,
        // even during the short interval before SQLite materializes the user message.
        (_, AgentState::Working) => AgentState::Working,
        // Finishing generation does not mean the turn is ready: the final answer may
        // explicitly ask the user a question. This matches AgentStatusBar priority.
        (Some(AgentState::Waiting | AgentState::WaitingReply), AgentState::Ready) => {
            database.unwrap()
        }
        (_, state) => state,
    }
}

fn parse(value: Option<&str>) -> Option<Value> {
    serde_json::from_str(value?).ok()
}

fn model_identity(
    session: Option<&Value>,
    assistant: Option<&Value>,
) -> (Option<String>, Option<String>) {
    let source = assistant
        .filter(|value| value.get("modelID").is_some() || value.get("id").is_some())
        .or(session);
    let clean = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.chars().take(80).collect())
    };
    (
        clean(source.and_then(|v| v["providerID"].as_str())),
        clean(source.and_then(|v| v["modelID"].as_str().or_else(|| v["id"].as_str()))),
    )
}

fn state_from_message(message: &Value, assistant_text: Option<&str>) -> AgentState {
    match message["role"].as_str() {
        Some("user") => AgentState::Working,
        Some("assistant") if message.pointer("/time/completed").is_none() => AgentState::Working,
        Some("assistant") if message["finish"] == "tool-calls" => AgentState::Working,
        Some("assistant") if assistant_text.is_some_and(ends_with_question) => {
            AgentState::WaitingReply
        }
        _ => AgentState::Ready,
    }
}

fn ends_with_question(text: &str) -> bool {
    text.chars()
        .rev()
        .find(|&c| !is_question_suffix(c))
        .is_some_and(|c| matches!(c, '?' | '？'))
}

fn is_question_suffix(c: char) -> bool {
    c.is_whitespace()
        || "\"'”’）)]】}。.!！*_`~～".contains(c)
        || matches!(
            c as u32,
            0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0xFE0F | 0x200D
        )
}

fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_message_states() {
        assert_eq!(
            state_from_message(&serde_json::json!({"role":"user"}), None),
            AgentState::Working
        );
        assert_eq!(
            state_from_message(
                &serde_json::json!({"role":"assistant","time":{"completed":2}}),
                Some("继续吗？")
            ),
            AgentState::WaitingReply
        );
        assert_eq!(
            state_from_message(
                &serde_json::json!({"role":"assistant","time":{"completed":2}}),
                Some("完成。")
            ),
            AgentState::Ready
        );
        assert_eq!(
            state_from_message(
                &serde_json::json!({"role":"assistant","time":{"completed":2}}),
                Some("要继续吗？ 🤔")
            ),
            AgentState::WaitingReply
        );
        assert_eq!(
            state_from_message(
                &serde_json::json!({"role":"assistant","finish":"tool-calls","time":{"completed":2}}),
                Some("继续吗？")
            ),
            AgentState::Working
        );
    }

    #[test]
    fn extracts_model_and_context() {
        let facts = facts_from_values(
            Some(r#"{"id":"fallback","providerID":"old"}"#),
            Some(r#"{"role":"assistant","time":{"completed":2}}"#),
            Some(
                r#"{"role":"assistant","providerID":"openai","modelID":"gpt-5","tokens":{"total":1200}}"#,
            ),
            None,
            Some(&serde_json::json!({"openai":{"models":{"gpt-5":{"limit":{"context":10000}}}}})),
        );
        assert_eq!(facts.model.as_deref(), Some("openai/gpt-5"));
        assert_eq!(facts.context.unwrap().used_tokens, 1200);
    }

    #[test]
    fn realtime_log_tracks_generation_loop() {
        let directory = std::env::temp_dir().join(format!("opencode-log-{}", std::process::id()));
        let _ = fs::create_dir_all(&directory);
        let path = directory.join("opencode.log");
        fs::write(
            &path,
            "message=\"exiting loop\" session.id=old\nmessage=loop session.id=current step=0\n",
        )
        .unwrap();
        assert_eq!(
            runtime_state_from_log(&path, Some("current")),
            Some(AgentState::Working)
        );
        fs::write(
            &path,
            "message=loop session.id=current step=0\nmessage=\"exiting loop\" session.id=current\n",
        )
        .unwrap();
        assert_eq!(
            runtime_state_from_log(&path, Some("current")),
            Some(AgentState::Ready)
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn exiting_loop_preserves_waiting_reply_from_final_message() {
        assert_eq!(
            merge_realtime_state(Some(AgentState::WaitingReply), AgentState::Ready),
            AgentState::WaitingReply
        );
        assert_eq!(
            merge_realtime_state(Some(AgentState::WaitingReply), AgentState::Working),
            AgentState::Working
        );
    }
}
