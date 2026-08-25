use std::process::Command;

pub fn focus(pid: u32) -> bool {
    #[cfg(target_os = "macos")]
    {
        return focus_macos(pid);
    }
    #[cfg(target_os = "windows")]
    {
        let _ = pid;
        return Command::new("cmd")
            .args(["/C", "start", "wt"])
            .status()
            .is_ok();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = pid;
        return Command::new("sh")
            .args(["-c", "xdg-open terminal://"])
            .status()
            .is_ok();
    }
}

#[cfg(target_os = "macos")]
fn focus_macos(pid: u32) -> bool {
    let tty = tty_for_pid(pid);
    if tty.as_deref().is_some_and(focus_exact_terminal_session) {
        return true;
    }
    terminal_app_for_pid(pid).is_some_and(|app| {
        Command::new("/usr/bin/open")
            .args(["-a", app])
            .status()
            .is_ok_and(|status| status.success())
    })
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
fn terminal_app_for_pid(pid: u32) -> Option<&'static str> {
    let mut commands = Vec::new();
    let mut current = pid;
    for _ in 0..16 {
        let output = Command::new("/bin/ps")
            .args(["-o", "ppid=", "-o", "command=", "-p", &current.to_string()])
            .output()
            .ok()?;
        let line = String::from_utf8_lossy(&output.stdout);
        let mut fields = line.split_whitespace();
        let parent = fields.next()?.parse::<u32>().ok()?;
        commands.push(fields.collect::<Vec<_>>().join(" "));
        if parent <= 1 || parent == current {
            break;
        }
        current = parent;
    }
    detect_terminal_app(&commands.join("\n"))
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
    fn bundled_script_mentions_exact_terminal_support() {
        let script = include_str!("focus.applescript");
        assert!(script.contains("Terminal"));
        assert!(script.contains("iTerm2"));
    }
}
