use std::{
    collections::HashMap,
    process::Command,
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct WebUrlDetector {
    cache: HashMap<Vec<u32>, (Instant, Option<String>)>,
}

impl WebUrlDetector {
    pub fn discover(&mut self, root_pid: u32, pids: &[u32]) -> Option<String> {
        let mut key = pids.to_vec();
        key.sort_unstable();
        if let Some((checked, value)) = self.cache.get(&key) {
            if checked.elapsed() < Duration::from_secs(10) {
                return value.clone();
            }
        }
        let value = listening_url(root_pid, &key);
        self.cache.insert(key, (Instant::now(), value.clone()));
        value
    }
}

fn listening_url(root_pid: u32, pids: &[u32]) -> Option<String> {
    if pids.is_empty() {
        return None;
    }
    let list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let output = Command::new("/usr/sbin/lsof")
        .args(["-nP", "-a", "-p", &list, "-iTCP", "-sTCP:LISTEN", "-Fn"])
        .output()
        .ok()?;
    let mut current_pid: Option<u32> = None;
    let mut urls = vec![];
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(pid) = line.strip_prefix('p').and_then(|value| value.parse().ok()) {
            current_pid = Some(pid);
        } else if let (Some(pid), Some(address)) = (current_pid, line.strip_prefix('n')) {
            if let Some(port) = local_port(address) {
                urls.push((pid, format!("http://127.0.0.1:{port}/")));
            }
        }
    }
    urls.sort_by_key(|(pid, url)| (*pid != root_pid, *pid, url.clone()));
    urls.into_iter().next().map(|(_, url)| url)
}

fn local_port(address: &str) -> Option<&str> {
    let (_, port) = address.rsplit_once(':')?;
    let host = address.strip_suffix(&format!(":{port}"))?;
    matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1").then_some(port)
}

pub fn focus_url(url: &str, reuse_tabs: bool) -> bool {
    if !is_local_http_url(url) {
        return false;
    }
    #[cfg(target_os = "macos")]
    if reuse_tabs && focus_existing_tab(url) {
        return true;
    }
    #[cfg(target_os = "macos")]
    if reuse_tabs && automation_denied(url) && activate_browser() {
        return true;
    }
    Command::new("open")
        .arg(url)
        .status()
        .is_ok_and(|status| status.success())
}

fn is_local_http_url(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("http://") else {
        return false;
    };
    let host = rest
        .split('/')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default();
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

#[cfg(target_os = "macos")]
fn focus_existing_tab(url: &str) -> bool {
    let script = include_str!("web_focus.applescript").replace("{url}", &url.replace('"', "\\\""));
    Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e", &script])
        .output()
        .is_ok_and(|output| output.status.success() && !output.stdout.is_empty())
}

#[cfg(target_os = "macos")]
fn automation_denied(url: &str) -> bool {
    let script = include_str!("web_focus.applescript").replace("{url}", &url.replace('"', "\\\""));
    Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e", &script])
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stderr).contains("-1743"))
}

#[cfg(target_os = "macos")]
fn activate_browser() -> bool {
    ["Google Chrome", "Microsoft Edge", "Brave Browser", "Safari"]
        .iter()
        .any(|app| {
            Command::new("/usr/bin/open")
                .args(["-a", app])
                .status()
                .is_ok_and(|status| status.success())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_only_loopback_listener_addresses() {
        assert_eq!(local_port("127.0.0.1:3080"), Some("3080"));
        assert_eq!(local_port("[::1]:8080"), Some("8080"));
        assert_eq!(local_port("10.0.0.1:8080"), None);
    }
}
