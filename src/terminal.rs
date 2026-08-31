use crate::model::AgentState;
use std::{
    collections::HashMap,
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant, SystemTime},
};

const STATUS_SEPARATOR: char = '\u{1d}';
const RECORD_SEPARATOR: char = '\u{1e}';
const FIELD_SEPARATOR: char = '\u{1f}';

pub struct TerminalProbe {
    results: HashMap<u32, (Instant, Option<SystemTime>, Option<AgentState>)>,
    pending: HashMap<u32, Option<SystemTime>>,
    tx: Sender<(u32, Option<SystemTime>, Option<AgentState>)>,
    rx: Receiver<(u32, Option<SystemTime>, Option<AgentState>)>,
}

impl Default for TerminalProbe {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            results: HashMap::new(),
            pending: HashMap::new(),
            tx,
            rx,
        }
    }
}

impl TerminalProbe {
    pub fn request(&mut self, pid: u32, activity: Option<SystemTime>) -> Option<AgentState> {
        while let Ok((pid, requested_activity, state)) = self.rx.try_recv() {
            // Do not let an AppleScript result captured for an older rollout
            // overwrite the state after Codex resumes or starts working again.
            if self.pending.get(&pid) == Some(&requested_activity) {
                self.results
                    .insert(pid, (Instant::now(), requested_activity, state));
                self.pending.remove(&pid);
            }
        }
        if let Some((at, result_activity, state)) = self.results.get(&pid) {
            if *result_activity == activity && at.elapsed() < Duration::from_secs(5) {
                return *state;
            }
        }
        if self.pending.get(&pid) != Some(&activity) {
            self.pending.insert(pid, activity);
            let tx = self.tx.clone();
            thread::spawn(move || {
                let _ = tx.send((pid, activity, probe_codex_sync(pid)));
            });
        }
        None
    }
}

fn probe_codex_sync(pid: u32) -> Option<AgentState> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        None
    }
    #[cfg(target_os = "macos")]
    {
        let tty = tty_for_pid(pid)?;
        let result = Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(include_str!("terminal.applescript"))
            .arg("--")
            .arg(&tty)
            .output()
            .ok()?;
        if !result.status.success() {
            return None;
        }
        parse_terminal_output(&String::from_utf8_lossy(&result.stdout), &tty)
    }
}

#[cfg(target_os = "macos")]
fn tty_for_pid(pid: u32) -> Option<String> {
    let output = Command::new("/usr/sbin/lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "0,1,2", "-Fn"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            line.strip_prefix("n/dev/tty")
                .map(|suffix| format!("/dev/tty{suffix}"))
        })
}

fn parse_terminal_output(output: &str, target_tty: &str) -> Option<AgentState> {
    let (_, records) = output.split_once(STATUS_SEPARATOR)?;
    for record in records.split(RECORD_SEPARATOR) {
        let Some((tty, contents)) = record.split_once(FIELD_SEPARATOR) else {
            continue;
        };
        if tty.trim() == target_tty {
            return detect_codex_terminal_state(contents);
        }
    }
    None
}

fn detect_codex_terminal_state(contents: &str) -> Option<AgentState> {
    let visible = contents.trim_end();
    let prompt = tail_chars(visible, 2_000);
    let lower = prompt.to_ascii_lowercase();
    let working = lower.contains("esc to interrupt")
        || lower.contains("background terminal running")
        || lower.contains("background terminals running");
    if working {
        return Some(AgentState::Working);
    }
    let footer = visible
        .to_ascii_lowercase()
        .ends_with("press enter to confirm or esc to cancel");
    let question = lower.contains("would you like to") || lower.contains("do you want to");
    let yes = has_numbered_choice(prompt, "yes");
    let no = has_numbered_choice(prompt, "no");
    (footer && question && yes && no).then_some(AgentState::Waiting)
}

fn tail_chars(value: &str, limit: usize) -> &str {
    if value.chars().count() <= limit {
        return value;
    }
    let start = value
        .char_indices()
        .rev()
        .nth(limit - 1)
        .map(|(index, _)| index)
        .unwrap_or(0);
    &value[start..]
}

fn has_numbered_choice(value: &str, choice: &str) -> bool {
    value.lines().any(|line| {
        let line = line.trim_start_matches(|c: char| c.is_whitespace() || c == '›' || c == '>');
        let Some((number, label)) = line.split_once('.') else {
            return false;
        };
        !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit())
            && label.trim_start().to_ascii_lowercase().starts_with(choice)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const APPROVAL: &str = "Do you want to run this command?\n› 1. Yes\n  2. No\nPress enter to confirm or esc to cancel";
    #[test]
    fn detects_complete_approval() {
        assert_eq!(
            detect_codex_terminal_state(APPROVAL),
            Some(AgentState::Waiting)
        );
    }
    #[test]
    fn active_background_work_wins_over_old_approval() {
        let value =
            format!("{APPROVAL}\nPlanning (4m • esc to interrupt) · 1 background terminal running");
        assert_eq!(
            detect_codex_terminal_state(&value),
            Some(AgentState::Working)
        );
    }
    #[test]
    fn incomplete_prompt_is_ignored() {
        assert_eq!(
            detect_codex_terminal_state("Do you want to run?\n1. Yes"),
            None
        );
    }
}
