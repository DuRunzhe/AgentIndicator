use crate::model::{AgentState, ContextUsage};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};

const TAIL_BYTES: usize = 2 * 1024 * 1024;
const TAIL_LINES: usize = 1000;

#[derive(Clone, Debug, Default)]
pub struct PiFacts {
    pub state: Option<AgentState>,
    pub model: Option<String>,
    pub context: Option<ContextUsage>,
}

struct Cached {
    session_modified: SystemTime,
    session_size: u64,
    models_modified: Option<SystemTime>,
    facts: PiFacts,
    last_access: Instant,
}

#[derive(Default)]
pub struct PiAnalyzer {
    cache: HashMap<PathBuf, Cached>,
}

impl PiAnalyzer {
    pub fn analyze(&mut self, cwd: &Path) -> Option<PiFacts> {
        let home = dirs::home_dir()?.join(".pi/agent");
        let session = latest_session(cwd, &home.join("sessions"))?;
        let metadata = session.metadata().ok()?;
        let session_modified = metadata.modified().ok()?;
        let models = home.join("models.json");
        let models_modified = models
            .metadata()
            .ok()
            .and_then(|entry| entry.modified().ok());
        if let Some(cached) = self.cache.get_mut(&session) {
            cached.last_access = Instant::now();
            if cached.session_modified == session_modified
                && cached.session_size == metadata.len()
                && cached.models_modified == models_modified
            {
                return Some(cached.facts.clone());
            }
        }
        let facts = parse_signals(&read_tail(&session)?, &models);
        self.cache.insert(
            session,
            Cached {
                session_modified,
                session_size: metadata.len(),
                models_modified,
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

fn latest_session(cwd: &Path, root: &Path) -> Option<PathBuf> {
    root.join(encode_project_key(cwd))
        .read_dir()
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|path| Some((path.metadata().ok()?.modified().ok()?, path)))
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
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

fn parse_signals(text: &str, models: &Path) -> PiFacts {
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
                    Some("stop" | "length" | "error" | "aborted") => {
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
            models,
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

fn context_window(models: &Path, provider: Option<&str>, model: Option<&str>) -> Option<u64> {
    let catalog: Value = serde_json::from_slice(&fs::read(models).ok()?).ok()?;
    let provider = provider?;
    let model = model?;
    catalog
        .pointer(&format!("/providers/{}/models", escape(provider)))?
        .as_array()?
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
    fn stopped_question_waits_for_reply() {
        let facts = parse_signals(
            r#"{"type":"message","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"Continue?"}]}}"#,
            Path::new("/missing"),
        );
        assert_eq!(facts.state, Some(AgentState::WaitingReply));
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
}
