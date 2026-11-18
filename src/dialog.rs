#[cfg(target_os = "macos")]
pub fn choose(
    message: &str,
    title: &str,
    buttons: &[&str],
    default: &str,
    cancel: Option<&str>,
) -> Option<String> {
    let buttons = buttons
        .iter()
        .map(|button| format!("\"{}\"", escape(button)))
        .collect::<Vec<_>>()
        .join(", ");
    let cancel = cancel
        .map(|button| format!(" cancel button \"{}\"", escape(button)))
        .unwrap_or_default();
    let script = format!("display dialog \"{}\" with title \"{}\" buttons {{{buttons}}} default button \"{}\"{cancel}", escape(message), escape(title), escape(default));
    let output = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .split("button returned:")
        .nth(1)?
        .split([',', '\n'])
        .next()
        .map(|value| value.trim().to_owned())
}

#[cfg(target_os = "linux")]
pub fn choose(
    message: &str,
    title: &str,
    buttons: &[&str],
    default: &str,
    _cancel: Option<&str>,
) -> Option<String> {
    linux_choose(message, title, buttons, default)
}

#[cfg(target_os = "windows")]
pub fn choose(
    _message: &str,
    _title: &str,
    _buttons: &[&str],
    default: &str,
    _cancel: Option<&str>,
) -> Option<String> {
    // Windows 暂无原生对话框后端；调用方（如 startup::toggle）都是用户主动点击
    // 菜单触发的，按默认按钮继续，避免功能静默失效。需要真正询问时应补 MessageBox 实现。
    Some(default.to_owned())
}

#[cfg(target_os = "linux")]
fn linux_choose(message: &str, title: &str, buttons: &[&str], default: &str) -> Option<String> {
    // 单按钮调用（notice）走信息框；两按钮及以上走确认框。
    if buttons.len() <= 1 {
        let ok = buttons.first().copied().unwrap_or(default);
        if which("zenity").is_some() {
            let status = std::process::Command::new("zenity")
                .args([
                    "--info",
                    "--no-markup",
                    "--title",
                    title,
                    "--text",
                    message,
                    "--ok-label",
                    ok,
                ])
                .status();
            if status.is_ok_and(|s| s.success()) {
                return Some(ok.to_owned());
            }
        }
        if which("kdialog").is_some() {
            let status = std::process::Command::new("kdialog")
                .args(["--title", title, "--msgbox", message])
                .status();
            if status.is_ok_and(|s| s.success()) {
                return Some(ok.to_owned());
            }
        }
        // 没有可用的对话框后端：视为已确认（否则调用方会把用户操作当成取消）
        return Some(ok.to_owned());
    }

    let ok_label = if buttons.contains(&default) {
        default
    } else {
        buttons[buttons.len() - 1]
    };
    let cancel_label = buttons
        .iter()
        .copied()
        .find(|button| *button != ok_label)
        .unwrap_or(buttons[0]);

    if which("zenity").is_some() {
        let status = std::process::Command::new("zenity")
            .args([
                "--question",
                "--no-markup",
                "--title",
                title,
                "--text",
                message,
                "--ok-label",
                ok_label,
                "--cancel-label",
                cancel_label,
            ])
            .status();
        if let Ok(status) = status {
            return Some(if status.success() {
                ok_label.to_owned()
            } else {
                cancel_label.to_owned()
            });
        }
    }
    if which("kdialog").is_some() {
        let status = std::process::Command::new("kdialog")
            .args([
                "--title",
                title,
                "--yesno",
                message,
                "--yes-label",
                ok_label,
                "--no-label",
                cancel_label,
            ])
            .status();
        if let Ok(status) = status {
            return Some(if status.success() {
                ok_label.to_owned()
            } else {
                cancel_label.to_owned()
            });
        }
    }
    // 没有可用的对话框后端：用户是主动点击菜单触发的，按默认按钮继续
    Some(default.to_owned())
}

#[cfg(target_os = "linux")]
fn which(program: &str) -> Option<std::path::PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

pub fn notice(message: &str, title: &str) {
    let _ = choose(message, title, &["确定"], "确定", None);
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escapes_applescript_strings() {
        assert_eq!(escape("a\\b\"c"), "a\\\\b\\\"c");
    }
}
