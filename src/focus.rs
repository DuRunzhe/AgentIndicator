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
    let output = Command::new("/usr/sbin/lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "0,1,2", "-Fn"])
        .output();
    let Some(tty) = output.ok().filter(|o| o.status.success()).and_then(|o| {
        String::from_utf8_lossy(&o.stdout).lines().find_map(|line| {
            line.strip_prefix("n/dev/tty")
                .map(|suffix| format!("/dev/tty{suffix}"))
        })
    }) else {
        return false;
    };
    Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(include_str!("focus.applescript"))
        .arg("--")
        .arg(&tty)
        .output()
        .is_ok_and(|result| result.status.success() && !result.stdout.is_empty())
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    #[test]
    fn bundled_script_mentions_supported_terminals() {
        let script = include_str!("focus.applescript");
        assert!(script.contains("Terminal"));
        assert!(script.contains("iTerm2"));
    }
}
