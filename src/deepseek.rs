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
    checked: Instant,
    facts: DeepSeekFacts,
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
        if let Some(cached) = self.cache.get(&session) {
            if (cached.modified == modified && cached.size == metadata.len())
                || cached.checked.elapsed() < Duration::from_secs(2)
            {
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
                checked: Instant::now(),
                facts: facts.clone(),
            },
        );
        Some(facts)
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

fn read_session_tail(path: &Path) -> Option<String> {
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

fn parse_signals(text: &str) -> DeepSeekFacts {
    let mut facts = DeepSeekFacts::default();
    let mut approvals = HashSet::new();
    let mut questions = HashSet::new();
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
            }
            Some("assistant/message") => {
                last_text = last_text_block(&data["message"]["content"]);
                if let Some(model) = data["message"]["source"]["model"].as_str() {
                    facts.model = Some(model.trim().into());
                }
            }
            Some("turn/end") => {
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
            Some("tool/call") if data["name"] == "ask_user_question" => {
                if let Some(id) = data["callId"].as_str() {
                    questions.insert(id.to_owned());
                }
            }
            Some("tool/result") => {
                if let Some(id) = data["message"]["source"]["callId"].as_str() {
                    questions.remove(id);
                }
            }
            _ => {}
        }
    }
    facts.state = if !questions.is_empty() || reply_requested {
        Some(AgentState::WaitingReply)
    } else if !approvals.is_empty() {
        Some(AgentState::Waiting)
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
    #[test]
    fn detects_question_at_turn_end() {
        let text = "{\"type\":\"assistant/message\",\"data\":{\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Continue?\"}],\"source\":{\"model\":\"deepseek-v3\"}}}}\n{\"type\":\"turn/end\",\"data\":{}}\n";
        let facts = parse_signals(text);
        assert_eq!(facts.state, Some(AgentState::WaitingReply));
        assert_eq!(facts.model.as_deref(), Some("deepseek-v3"));
    }
}
