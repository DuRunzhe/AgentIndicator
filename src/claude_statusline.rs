use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
};

const CONTEXT_DIR: &str = "/tmp/agent-statusbar-claude-context";

pub fn install() -> Result<(), String> {
    let config_dir = claude_config_dir()?;
    let settings_path = config_dir.join("settings.json");
    let integration_path = config_dir.join("agent-status-indicator-statusline.json");
    let mut settings = read_json(&settings_path).unwrap_or_else(|| json!({}));
    if !settings.is_object() {
        return Err("Claude settings.json 不是 JSON 对象".into());
    }
    let collector_command = collector_command()?;
    let existing = read_json(&integration_path);
    let current = settings.get("statusLine").cloned();
    let previous = if existing
        .as_ref()
        .and_then(|value| value["collector_command"].as_str())
        == Some(&collector_command)
    {
        existing
            .and_then(|value| value.get("previous_status_line").cloned())
            .unwrap_or(Value::Null)
    } else {
        match current {
            None => Value::Null,
            Some(value)
                if value.as_object().is_some_and(|object| {
                    object.get("type").and_then(Value::as_str) == Some("command")
                }) =>
            {
                value
            }
            Some(_) => return Err("现有 Claude statusLine 不是 command 类型，未作修改".into()),
        }
    };
    settings["statusLine"] = json!({"type": "command", "command": collector_command});
    atomic_json(
        &integration_path,
        &json!({
            "version": 1,
            "collector_command": collector_command,
            "previous_status_line": previous,
        }),
    )?;
    atomic_json(&settings_path, &settings)?;
    Ok(())
}

/// True when settings.json routes the statusline to a collector command that
/// is ours (carries `--claude-statusline`) but points at a different
/// executable than this one. Foreign or absent statusLines are never stale, so
/// explicit user configuration and deliberate removals survive.
fn collector_is_stale(status_line: &Value, expected_command: &str) -> bool {
    if status_line.get("type").and_then(Value::as_str) != Some("command") {
        return false;
    }
    let Some(command) = status_line.get("command").and_then(Value::as_str) else {
        return false;
    };
    command.contains("--claude-statusline") && command != expected_command
}

/// Re-point the Claude statusline at this executable when it still references a
/// collector installed by an earlier channel (npm / Homebrew / curl). The tray
/// calls this once at startup, so launching the freshly installed binary
/// automatically updates `~/.claude/settings.json`; `install()` preserves the
/// old command as `previous_status_line` for a clean uninstall.
pub fn auto_repoint_if_stale() {
    let Ok(config_dir) = claude_config_dir() else {
        return;
    };
    let settings_path = config_dir.join("settings.json");
    let Some(settings) = read_json(&settings_path) else {
        return;
    };
    let Some(status_line) = settings.get("statusLine") else {
        return;
    };
    let Ok(expected_command) = collector_command() else {
        return;
    };
    if !collector_is_stale(status_line, &expected_command) {
        return;
    }
    match install() {
        Ok(()) => eprintln!("repointed Claude statusline to {expected_command}"),
        Err(error) => eprintln!("could not repoint Claude statusline: {error}"),
    }
}

pub fn collect_from_stdin() {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    if let Ok(payload) = serde_json::from_str::<Value>(&input) {
        let _ = write_snapshot(&payload);
    }
    forward_original_statusline(&input);
}

fn write_snapshot(payload: &Value) -> Result<(), String> {
    let Some(id) = payload
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|id| safe_session_id(id))
    else {
        return Ok(());
    };
    let model = match payload.get("model") {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Object(value)) => value
            .get("display_name")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    };
    let context = payload.get("context_window").and_then(|value| {
        Some(json!({
            "used_tokens": value.get("total_input_tokens")?.as_u64()?,
            "window_tokens": value.get("context_window_size")?.as_u64()?,
        }))
    });
    let snapshot = json!({
        "session_id": id,
        "transcript_path": payload.get("transcript_path").and_then(Value::as_str),
        "cwd": payload.get("cwd").and_then(Value::as_str),
        "model": model,
        "context_usage": context,
    });
    let destination = PathBuf::from(CONTEXT_DIR).join(format!("{id}.json"));
    atomic_json(&destination, &snapshot)
}

fn forward_original_statusline(input: &str) {
    let Ok(config_dir) = claude_config_dir() else {
        return;
    };
    let Some(command) = read_json(&config_dir.join("agent-status-indicator-statusline.json"))
        .and_then(|value| {
            value["previous_status_line"]["command"]
                .as_str()
                .map(str::to_owned)
        })
    else {
        return;
    };
    let Ok(mut child) = Command::new("/bin/sh")
        .args(["-lc", &command])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let Ok(output) = child.wait_with_output() else {
        return;
    };
    let _ = std::io::stdout().write_all(&output.stdout);
    let _ = std::io::stderr().write_all(&output.stderr);
}

fn collector_command() -> Result<String, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    Ok(format!(
        "{} --claude-statusline",
        shell_quote(&executable.to_string_lossy())
    ))
}

fn claude_config_dir() -> Result<PathBuf, String> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
        .ok_or_else(|| "无法确定 Claude 配置目录".into())
}

fn safe_session_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|char| char.is_ascii_alphanumeric() || char == '_' || char == '-')
}
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\\"'\\\"'"))
}
fn read_json(path: &PathBuf) -> Option<Value> {
    serde_json::from_reader(fs::File::open(path).ok()?).ok()
}
fn atomic_json(path: &PathBuf, value: &Value) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| "无效配置路径".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let file = fs::File::create(&temporary).map_err(|error| error.to_string())?;
    serde_json::to_writer(file, value).map_err(|error| error.to_string())?;
    fs::rename(temporary, path).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_only_safe_session_ids() {
        assert!(safe_session_id("abc-123_A"));
        assert!(!safe_session_id("../x"));
    }
    #[test]
    fn quotes_shell_paths() {
        assert_eq!(shell_quote("a'b"), "'a'\\\"'\\\"'b'");
    }
    #[test]
    fn auto_repoint_rewrites_a_stale_collector_in_place() {
        let config = std::env::temp_dir().join(format!(
            "agent-status-indicator-repoint-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&config);
        fs::create_dir_all(&config).expect("create temp config dir");
        let settings_path = config.join("settings.json");
        let stale = serde_json::json!({
            "statusLine": {
                "type": "command",
                "command": "'/old/install/agent-status-indicator' --claude-statusline"
            }
        });
        fs::write(&settings_path, serde_json::to_string(&stale).unwrap()).unwrap();
        let previous = std::env::var_os("CLAUDE_CONFIG_DIR");
        std::env::set_var("CLAUDE_CONFIG_DIR", &config);
        auto_repoint_if_stale();
        if let Some(value) = previous {
            std::env::set_var("CLAUDE_CONFIG_DIR", value);
        } else {
            std::env::remove_var("CLAUDE_CONFIG_DIR");
        }
        let expected = std::env::current_exe().expect("current exe");
        let updated: Value =
            serde_json::from_reader(fs::File::open(&settings_path).unwrap()).unwrap();
        let command = updated["statusLine"]["command"]
            .as_str()
            .unwrap_or_default();
        assert!(command.contains(&expected.to_string_lossy().into_owned()));
        assert!(command.ends_with("--claude-statusline"));
        // The stale path is preserved as previous so uninstall can restore it.
        let metadata = read_json(&config.join("agent-status-indicator-statusline.json"))
            .expect("integration metadata written");
        assert_eq!(
            metadata["previous_status_line"]["command"],
            serde_json::json!("'/old/install/agent-status-indicator' --claude-statusline")
        );
        let _ = fs::remove_dir_all(&config);
    }
}
