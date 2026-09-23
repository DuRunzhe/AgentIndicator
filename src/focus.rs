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
        return focus_linux(pid);
    }
}

#[cfg(target_os = "windows")]
fn activate_windows_process_window(pid: u32) -> bool {
    // The agent itself is normally a console child with no window handle.
    // Windows Terminal and conhost own the visible window on an ancestor
    // process, so walk the parent chain and foreground the first visible
    // top-level window any ancestor owns.
    let ancestors = ancestor_pids(pid);
    if ancestors.is_empty() {
        return false;
    }
    find_visible_window(&ancestors).is_some_and(foreground_window)
}

#[cfg(target_os = "windows")]
fn ancestor_pids(pid: u32) -> Vec<u32> {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    let mut chain = Vec::new();
    let mut current = Pid::from_u32(pid);
    for _ in 0..16 {
        let Some(process) = system.process(current) else {
            break;
        };
        chain.push(current.as_u32());
        match process.parent() {
            Some(parent) if parent.as_u32() > 0 && parent != current => current = parent,
            _ => break,
        }
    }
    chain
}

#[cfg(target_os = "windows")]
fn find_visible_window(pids: &[u32]) -> Option<isize> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible,
    };

    struct EnumContext<'a> {
        targets: &'a [u32],
        found: Option<isize>,
    }

    // SAFETY: `lparam` carries the EnumContext pointer we handed to
    // EnumWindows; the callback only reads the target pid list and records
    // the first matching visible window. Returning FALSE stops enumeration.
    unsafe extern "system" fn enum_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let context = unsafe { &mut *(lparam.0 as *mut EnumContext<'_>) };
        // SAFETY: `hwnd` is a live top-level window supplied by EnumWindows.
        unsafe {
            if IsWindowVisible(hwnd).as_bool() {
                let mut window_pid = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut window_pid));
                if context.targets.contains(&window_pid) {
                    context.found = Some(hwnd.0 as isize);
                    return BOOL(0);
                }
            }
        }
        BOOL(1)
    }

    let mut context = EnumContext {
        targets: pids,
        found: None,
    };
    // SAFETY: the callback and the context pointer stay valid for the
    // duration of the call; EnumWindows enumerates on this thread before
    // returning.
    let _ = unsafe {
        EnumWindows(
            Some(enum_callback),
            LPARAM(&mut context as *mut EnumContext as isize),
        )
    };
    context.found
}

#[cfg(target_os = "windows")]
fn foreground_window(hwnd_address: isize) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId, IsIconic, SetForegroundWindow, ShowWindow,
        SW_RESTORE,
    };

    // SAFETY: `hwnd_address` was produced by EnumWindows in this process.
    // Every call below restores or foregrounds that window, or temporarily
    // attaches this thread's input queue, which requires no other state.
    unsafe {
        let hwnd = HWND(hwnd_address as *mut core::ffi::c_void);

        // A minimized window cannot receive the foreground, so restore first.
        if IsIconic(hwnd).as_bool() {
            ShowWindow(hwnd, SW_RESTORE);
        }

        // Windows rejects SetForegroundWindow from a process that does not
        // already own the foreground. Attaching our input queue to the
        // current foreground thread makes the OS treat this process as part
        // of the foreground for the duration of the call, lifting the
        // restriction without injecting synthetic keystrokes.
        let foreground = GetForegroundWindow();
        let foreground_thread = if foreground.0.is_null() {
            0
        } else {
            GetWindowThreadProcessId(foreground, None)
        };
        let current_thread = GetCurrentThreadId();
        let attached = foreground_thread != 0
            && foreground_thread != current_thread
            && AttachThreadInput(current_thread, foreground_thread, true).as_bool();
        let focused = SetForegroundWindow(hwnd).as_bool();
        if attached {
            AttachThreadInput(current_thread, foreground_thread, false);
        }
        focused
    }
}

#[cfg(target_os = "linux")]
fn focus_linux(pid: u32) -> bool {
    if session_is_wayland(
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
    ) {
        // xdotool only speaks X11 and Wayland has no universal toplevel
        // activation interface, so exact focus is intentionally skipped there.
        return launch_fallback_terminal();
    }
    // Top-level terminal windows belong to the terminal emulator process, not
    // to the agent shell, so try every pid on the ancestor chain from the
    // agent upward before giving up.
    for candidate in ancestor_pids(pid) {
        let Ok(output) = Command::new("xdotool")
            .args(["search", "--onlyvisible", "--pid", &candidate.to_string()])
            .output()
        else {
            // xdotool is not installed at all; skip straight to the fallback.
            break;
        };
        if !output.status.success() {
            continue;
        }
        let Some(window) = String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .and_then(|line| line.trim().parse::<u32>().ok())
        else {
            continue;
        };
        if Command::new("xdotool")
            .args(["windowactivate", "--sync", &window.to_string()])
            .status()
            .is_ok_and(|status| status.success())
        {
            return true;
        }
    }
    launch_fallback_terminal()
}

/// Pure helper so Wayland detection stays unit-testable on every OS.
#[cfg(any(target_os = "linux", test))]
fn session_is_wayland(wayland_display: Option<&str>, session_type: Option<&str>) -> bool {
    wayland_display.is_some_and(|value| !value.trim().is_empty())
        || session_type.is_some_and(|value| value.eq_ignore_ascii_case("wayland"))
}

#[cfg(target_os = "linux")]
fn ancestor_pids(pid: u32) -> Vec<u32> {
    // /proc/<pid>/stat is `pid (comm) state ppid ...` where comm may contain
    // spaces, so split on the last ')' and take the second field after it as
    // the parent pid.
    let mut chain = vec![pid];
    let mut current = pid;
    for _ in 0..16 {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{current}/stat")) else {
            break;
        };
        let Some((_, fields)) = stat.rsplit_once(')') else {
            break;
        };
        let Some(parent) = fields
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<u32>().ok())
        else {
            break;
        };
        if parent <= 1 || chain.contains(&parent) {
            break;
        }
        chain.push(parent);
        current = parent;
    }
    chain
}

#[cfg(target_os = "linux")]
fn launch_fallback_terminal() -> bool {
    let script = "if command -v x-terminal-emulator >/dev/null 2>&1; then x-terminal-emulator; elif command -v gnome-terminal >/dev/null 2>&1; then gnome-terminal; else xdg-open terminal://; fi";
    Command::new("sh")
        .args(["-c", script, "agent-status-indicator"])
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
mod wayland_tests {
    use super::session_is_wayland;

    #[test]
    fn wayland_display_marks_a_wayland_session() {
        assert!(session_is_wayland(Some("wayland-0"), None));
        assert!(session_is_wayland(Some(" wayland-1 "), Some("x11")));
        assert!(!session_is_wayland(Some(""), Some("x11")));
    }

    #[test]
    fn session_type_marks_a_wayland_session() {
        assert!(session_is_wayland(None, Some("wayland")));
        assert!(session_is_wayland(None, Some("Wayland")));
        assert!(!session_is_wayland(None, Some("tty")));
    }
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
