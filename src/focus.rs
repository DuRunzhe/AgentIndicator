use std::process::Command;

pub fn focus(pid: u32) -> bool {
    #[cfg(target_os = "macos")]
    {
        return focus_macos(pid);
    }
    #[cfg(target_os = "windows")]
    {
        if activate_windows_process_window(pid) {
            return true;
        }
        return Command::new("cmd")
            .args(["/C", "start", "wt"])
            .status()
            .is_ok_and(|status| status.success());
    }
    #[cfg(target_os = "linux")]
    {
        // xdotool can activate an existing terminal window on X11. Wayland and
        // minimal desktops safely fall back to the configured terminal.
        let script = "if command -v xdotool >/dev/null 2>&1; then w=$(xdotool search --pid \"$1\" 2>/dev/null | head -n1); [ -n \"$w\" ] && xdotool windowactivate \"$w\" && exit 0; fi; if command -v x-terminal-emulator >/dev/null 2>&1; then x-terminal-emulator; elif command -v gnome-terminal >/dev/null 2>&1; then gnome-terminal; else xdg-open terminal://; fi";
        return Command::new("sh")
            .args(["-c", script, "agent-status-indicator", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success());
    }
}

#[cfg(target_os = "windows")]
fn activate_windows_process_window(pid: u32) -> bool {
    // Codex itself is normally a console child with no window handle. Walk its
    // parent chain and ask the Windows shell to foreground the first GUI host.
    // This works for Windows Terminal and classic conhost without adding an
    // unsafe Win32 dependency to the portable detector core.
    let script = format!(
        "$id={pid}; for($n=0;$n -lt 16 -and $id -gt 0;$n++){{ $p=Get-Process -Id $id -ErrorAction SilentlyContinue; if($p -and $p.MainWindowHandle -ne 0){{ $null=(New-Object -ComObject WScript.Shell).AppActivate($p.Id); exit 0 }}; $id=(Get-CimInstance Win32_Process -Filter \"ProcessId=$id\" -ErrorAction SilentlyContinue).ParentProcessId }}; exit 1"
    );
    Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "macos")]
fn focus_macos(pid: u32) -> bool {
    let tty = tty_for_pid(pid);
    if tty.as_deref().is_some_and(focus_exact_terminal_session) {
        return true;
    }
    let ancestry = process_ancestry(pid);
    // A GUI application that hosts an agent (ChatGPT drives its own Codex)
    // owns the session, so activating the application is the focus action.
    detect_host_application(&ancestry)
        .or_else(|| detect_terminal_app(&ancestry))
        .is_some_and(activate_application)
}

#[cfg(target_os = "macos")]
fn activate_application(app: &str) -> bool {
    Command::new("/usr/bin/open")
        .args(["-a", app])
        .status()
        .is_ok_and(|status| status.success())
}

/// GUI applications that embed and drive an agent themselves, mapped to their
/// application name for `open -a`. Mirrors the detector's host applications.
#[cfg(target_os = "macos")]
fn detect_host_application(ancestry: &str) -> Option<&'static str> {
    const HOST_APPLICATIONS: [(&str, &str); 1] = [("ChatGPT.app/", "ChatGPT")];
    HOST_APPLICATIONS
        .iter()
        .find_map(|(needle, app)| ancestry.contains(needle).then_some(*app))
}

#[cfg(target_os = "macos")]
fn tty_for_pid(pid: u32) -> Option<String> {
    let output = Command::new("/usr/sbin/lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "0,1,2", "-Fn"])
        .output()
        .ok()?;
    output.status.success().then_some(())?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            line.strip_prefix("n/dev/tty")
                .map(|suffix| format!("/dev/tty{suffix}"))
        })
}

#[cfg(target_os = "macos")]
fn focus_exact_terminal_session(tty: &str) -> bool {
    Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(include_str!("focus.applescript"))
        .arg("--")
        .arg(tty)
        .output()
        .is_ok_and(|result| result.status.success() && !result.stdout.is_empty())
}

#[cfg(target_os = "macos")]
fn process_ancestry(pid: u32) -> String {
    let mut commands = Vec::new();
    let mut current = pid;
    for _ in 0..16 {
        let Ok(output) = Command::new("/bin/ps")
            .args(["-o", "ppid=", "-o", "command=", "-p", &current.to_string()])
            .output()
        else {
            break;
        };
        let line = String::from_utf8_lossy(&output.stdout);
        let mut fields = line.split_whitespace();
        let Some(parent) = fields.next().and_then(|value| value.parse::<u32>().ok()) else {
            break;
        };
        commands.push(fields.collect::<Vec<_>>().join(" "));
        if parent <= 1 || parent == current {
            break;
        }
        current = parent;
    }
    commands.join("\n")
}

#[cfg(target_os = "macos")]
fn detect_terminal_app(ancestry: &str) -> Option<&'static str> {
    let patterns = [
        ("iTerm2.app/", "iTerm"),
        ("iTermServer", "iTerm"),
        ("Terminal.app/", "Terminal"),
        ("Warp.app/", "Warp"),
        ("Warp Helper", "Warp"),
        ("Visual Studio Code.app/", "Visual Studio Code"),
        ("Code Helper", "Visual Studio Code"),
        ("Cursor.app/", "Cursor"),
        ("Cursor Helper", "Cursor"),
        ("Windsurf.app/", "Windsurf"),
        ("Windsurf Helper", "Windsurf"),
        ("kitty.app/", "kitty"),
        ("Alacritty.app/", "Alacritty"),
    ];
    patterns
        .iter()
        .find_map(|(needle, app)| ancestry.contains(needle).then_some(*app))
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn detects_supported_terminal_ancestors() {
        assert_eq!(
            detect_terminal_app("/Applications/Cursor.app/Contents/MacOS/Cursor"),
            Some("Cursor")
        );
        assert_eq!(detect_terminal_app("iTermServer"), Some("iTerm"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn detects_host_application_ancestors() {
        assert_eq!(
            detect_host_application(
                "/Applications/ChatGPT.app/Contents/Resources/codex -c features.code_mode_host=true app-server"
            ),
            Some("ChatGPT")
        );
        assert_eq!(detect_host_application("/opt/homebrew/bin/codex"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bundled_script_mentions_exact_terminal_support() {
        let script = include_str!("focus.applescript");
        assert!(script.contains("Terminal"));
        assert!(script.contains("iTerm2"));
    }
}
