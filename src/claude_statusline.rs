use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const CONTEXT_DIR: &str = "/tmp/agent-statusbar-claude-context";

/// True when Claude Code routes its statusline through this collector. The
/// tray queries this on every refresh so the menu row always shows the current
/// state instead of a stale one cached at startup.
pub fn is_installed() -> bool {
    match claude_config_dir() {
        Ok(config_dir) => is_installed_in(&config_dir),
        Err(_) => false,
    }
}

fn is_installed_in(config_dir: &Path) -> bool {
    read_json(&config_dir.join("settings.json"))
        .as_ref()
        .and_then(|settings| settings.get("statusLine"))
        .is_some_and(is_collector_status_line)
}

pub fn install() -> Result<(), String> {
    install_in(&claude_config_dir()?)
}

fn install_in(config_dir: &Path) -> Result<(), String> {
    let settings_path = config_dir.join("settings.json");
    let integration_path = config_dir.join("agent-status-indicator-statusline.json");
    let mut settings = read_json(&settings_path).unwrap_or_else(|| json!({}));
    if !settings.is_object() {
        return Err(crate::i18n::text("claude_error_settings_not_object").into());
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
            Some(_) => return Err(crate::i18n::text("claude_error_foreign_statusline").into()),
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

/// True for a statusline that belongs to us: a command carrying
/// `--claude-statusline`, whichever executable it points at.
fn is_collector_status_line(status_line: &Value) -> bool {
    status_line.get("type").and_then(Value::as_str) == Some("command")
        && status_line
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| command.contains("--claude-statusline"))
}

/// True when settings.json routes the statusline to a collector command that
/// is ours (carries `--claude-statusline`) but points at a different
/// executable than this one. Foreign or absent statusLines are never stale, so
/// explicit user configuration and deliberate removals survive.
fn collector_is_stale(status_line: &Value, expected_command: &str) -> bool {
    is_collector_status_line(status_line)
        && status_line.get("command").and_then(Value::as_str) != Some(expected_command)
}

/// Undo [`install`]: restore the statusLine the collector replaced and drop the
/// integration metadata. A statusLine that is not ours is never touched, so a
/// re-configured or deliberately removed collector reports an error instead of
/// overwriting user settings.
pub fn uninstall() -> Result<(), String> {
    uninstall_in(&claude_config_dir()?)
}

fn uninstall_in(config_dir: &Path) -> Result<(), String> {
    let settings_path = config_dir.join("settings.json");
    let integration_path = config_dir.join("agent-status-indicator-statusline.json");
    let mut settings = read_json(&settings_path)
        .ok_or_else(|| crate::i18n::text("claude_error_settings_missing").to_owned())?;
    if !settings.is_object() {
        return Err(crate::i18n::text("claude_error_settings_not_object").into());
    }
    if !settings
        .get("statusLine")
        .is_some_and(is_collector_status_line)
    {
        return Err(crate::i18n::text("claude_error_not_installed").into());
    }
    let previous = read_json(&integration_path)
        .and_then(|value| value.get("previous_status_line").cloned())
        .unwrap_or(Value::Null);
    match previous {
        // No statusLine existed before the collector was installed.
        Value::Null => {
            if let Some(object) = settings.as_object_mut() {
                object.remove("statusLine");
            }
        }
        previous => settings["statusLine"] = previous,
    }
    atomic_json(&settings_path, &settings)?;
    let _ = fs::remove_file(&integration_path);
    Ok(())
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
    match install_in(&config_dir) {
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
    let executable = std::env::current_exe()
        .map_err(|error| format!("{}: {error}", crate::i18n::text("claude_error_executable")))?;
    Ok(format!(
        "{} --claude-statusline",
        shell_quote(&executable.to_string_lossy())
    ))
}

fn claude_config_dir() -> Result<PathBuf, String> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
        .ok_or_else(|| crate::i18n::text("claude_error_config_dir").to_owned())
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
fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_reader(fs::File::open(path).ok()?).ok()
}
fn atomic_json(path: &Path, value: &Value) -> Result<(), String> {
    let write_error = |error: std::io::Error| {
        format!(
            "{}: {error}",
            crate::i18n::text("claude_error_write_failed")
        )
    };
    let parent = path
        .parent()
        .ok_or_else(|| crate::i18n::text("claude_error_invalid_path").to_owned())?;
    fs::create_dir_all(parent).map_err(write_error)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let file = fs::File::create(&temporary).map_err(write_error)?;
    serde_json::to_writer(file, value).map_err(|error| write_error(error.into()))?;
    fs::rename(temporary, path).map_err(write_error)
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
    fn install_then_uninstall_restores_the_previous_statusline() {
        let config = temp_config("round-trip");
        let settings_path = config.join("settings.json");
        let original = json!({
            "statusLine": {"type": "command", "command": "my-statusline"},
            "model": "opus",
        });
        fs::write(&settings_path, serde_json::to_string(&original).unwrap()).unwrap();

        assert!(!is_installed_in(&config));
        install_in(&config).expect("install");
        assert!(is_installed_in(&config));
        // A second install is idempotent and still remembers the original.
        install_in(&config).expect("reinstall");
        assert!(is_installed_in(&config));

        uninstall_in(&config).expect("uninstall");
        assert!(!is_installed_in(&config));
        let restored: Value =
            serde_json::from_reader(fs::File::open(&settings_path).unwrap()).unwrap();
        assert_eq!(restored["statusLine"], original["statusLine"]);
        assert_eq!(restored["model"], json!("opus"));

        // Uninstalling when nothing is installed must not rewrite settings.
        assert!(uninstall_in(&config).is_err());
        let _ = fs::remove_dir_all(&config);
    }
    #[test]
    fn uninstall_without_a_previous_statusline_removes_the_key() {
        let config = temp_config("fresh");
        let settings_path = config.join("settings.json");
        fs::write(
            &settings_path,
            serde_json::to_string(&json!({"theme": "dark"})).unwrap(),
        )
        .unwrap();

        install_in(&config).expect("install");
        assert!(is_installed_in(&config));
        uninstall_in(&config).expect("uninstall");

        let restored: Value =
            serde_json::from_reader(fs::File::open(&settings_path).unwrap()).unwrap();
        assert!(restored.get("statusLine").is_none());
        assert_eq!(restored["theme"], json!("dark"));
        let _ = fs::remove_dir_all(&config);
    }
    fn temp_config(name: &str) -> PathBuf {
        let config = std::env::temp_dir().join(format!(
            "agent-status-indicator-{name}-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&config);
        fs::create_dir_all(&config).expect("create temp config dir");
        config
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
