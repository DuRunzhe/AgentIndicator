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
        // Refresh asynchronously without briefly reverting to the rollout's
        // Working state. Only reuse evidence for the same rollout revision;
        // new activity must invalidate an old confirmation immediately.
        self.results
            .get(&pid)
            .and_then(|(_, result_activity, state)| {
                (*result_activity == activity).then_some(*state).flatten()
            })
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
    // Prefer the shape of the active chooser over a particular English
    // question/footer. Codex changes wording independently of its version.
    if has_confirmation_chooser(prompt) {
        return Some(AgentState::Waiting);
    }
    let working = lower.contains("esc to interrupt")
        || lower.contains("background terminal running")
        || lower.contains("background terminals running");
    working.then_some(AgentState::Working)
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

/// Require a bottom-of-screen keyboard hint and a selected, sequential choice
/// list. Key-bearing labels support localized/reworded prompts; older Codex
/// builds without label shortcuts use a narrowly scoped Yes/No fallback.
/// Unrecognized layouts yield no signal, never a timeout-based approval guess.
fn has_confirmation_chooser(prompt: &str) -> bool {
    let mut lines = prompt
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let Some(footer) = lines.pop() else {
        return false;
    };
    let footer_keys = footer
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let has_key = |keys: &[&str]| footer_keys.iter().any(|word| keys.contains(&word.as_str()));
    if !has_key(&["enter", "return"]) || !has_key(&["esc", "escape"]) {
        return false;
    }
    let mut choices = Vec::new();
    for line in lines.into_iter().rev() {
        let selected = line.starts_with(['›', '>', '❯']);
        let line = line.trim_start_matches(['›', '>', '❯']).trim_start();
        let Some((number, label)) = line.split_once(['.', ')']) else {
            break;
        };
        let Ok(number) = number.parse::<usize>() else {
            break;
        };
        choices.push((number, label.trim().to_ascii_lowercase(), selected));
    }
    choices.reverse();
    if choices.len() < 2
        || !choices.iter().any(|(_, _, selected)| *selected)
        || !choices
            .iter()
            .enumerate()
            .all(|(i, (number, _, _))| *number == i + 1)
    {
        return false;
    }
    let shortcut = |label: &str, key: &str| {
        label.contains(&format!("({key})")) || label.contains(&format!("[{key}]"))
    };
    let keyed_yes = choices.iter().any(|(_, label, _)| shortcut(label, "y"));
    let keyed_no = choices.iter().any(|(_, label, _)| {
        shortcut(label, "esc") || shortcut(label, "escape") || shortcut(label, "n")
    });
    let starts_word = |label: &str, word: &str| {
        label.split(|c: char| !c.is_ascii_alphanumeric()).next() == Some(word)
    };
    let legacy_yes = choices
        .iter()
        .any(|(_, label, _)| starts_word(label, "yes"));
    let legacy_no = choices.iter().any(|(_, label, _)| starts_word(label, "no"));
    (keyed_yes && keyed_no) || (legacy_yes && legacy_no)
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
    fn active_background_work_wins_after_an_old_approval() {
        let value =
            format!("{APPROVAL}\nPlanning (4m • esc to interrupt) · 1 background terminal running");
        assert_eq!(
            detect_codex_terminal_state(&value),
            Some(AgentState::Working)
        );
    }
    #[test]
    fn complete_approval_wins_over_an_earlier_activity_marker() {
        let value = format!("Planning (4m • esc to interrupt)\n{APPROVAL}");
        assert_eq!(
            detect_codex_terminal_state(&value),
            Some(AgentState::Waiting)
        );
    }
    #[test]
    fn incomplete_prompt_is_ignored() {
        assert_eq!(
            detect_codex_terminal_state("Do you want to run?\n1. Yes"),
            None
        );
    }
    #[test]
    fn recognizes_reworded_and_localized_keyed_prompts() {
        for prompt in [
            "Apply proposed file edits\nDestination: /tmp/README.md\n› 1. Yes, proceed (y)\n  2. Yes, remember (a)\n  3. No, revise (esc)\nPress enter to confirm or esc to cancel",
            "需要确认修改\n❯ 1) 允许 [y]\n  2) 拒绝 [n]\nReturn：确认 · Escape：取消",
            "Different future question\n> 1. Proceed (y)\n2. Cancel (esc)\nEnter / Esc",
            "› 1. Yes, proceed\n2. No, cancel\nENTER accepts; ESC goes back",
        ] {
            assert_eq!(detect_codex_terminal_state(prompt), Some(AgentState::Waiting), "{prompt}");
        }
    }

    #[test]
    fn rejects_history_partial_and_unrelated_menus() {
        for prompt in [
            "1. Yes (y)\n2. No (esc)\nEnter / Esc", // no active selection
            "› 1. Yes (y)\n3. No (esc)\nEnter / Esc", // incomplete list
            "› 1. Yesterday\n2. Nothing\nEnter / Esc", // not Yes/No words
            "› 1. Model A\n2. Model B\nEnter / Esc", // unrelated chooser
            "› 1. Yes (y)\n2. No (esc)",            // missing footer
            "› 1. Yes (y)\n2. No (esc)\nEnter / Esc\nTask completed",
        ] {
            assert_eq!(detect_codex_terminal_state(prompt), None, "{prompt}");
        }
    }

    #[test]
    fn terminal_records_are_matched_by_tty() {
        let output = format!("running{STATUS_SEPARATOR}/dev/ttys001{FIELD_SEPARATOR}{APPROVAL}{RECORD_SEPARATOR}/dev/ttys002{FIELD_SEPARATOR}Working (esc to interrupt){RECORD_SEPARATOR}");
        assert_eq!(
            parse_terminal_output(&output, "/dev/ttys001"),
            Some(AgentState::Waiting)
        );
        assert_eq!(
            parse_terminal_output(&output, "/dev/ttys002"),
            Some(AgentState::Working)
        );
        assert_eq!(parse_terminal_output(&output, "/dev/ttys003"), None);
        assert_eq!(parse_terminal_output("not_running", "/dev/ttys001"), None);
    }

    #[test]
    fn refresh_keeps_confirmation_until_new_probe_completes() {
        let mut probe = TerminalProbe::default();
        let activity = Some(SystemTime::UNIX_EPOCH);
        probe.results.insert(
            42,
            (
                Instant::now() - Duration::from_secs(6),
                activity,
                Some(AgentState::Waiting),
            ),
        );
        // Simulate an in-flight refresh without launching AppleScript.
        probe.pending.insert(42, activity);
        for _ in 0..3 {
            assert_eq!(probe.request(42, activity), Some(AgentState::Waiting));
        }
        probe
            .tx
            .send((42, activity, Some(AgentState::Working)))
            .unwrap();
        assert_eq!(probe.request(42, activity), Some(AgentState::Working));
    }

    #[test]
    fn new_activity_does_not_reuse_old_confirmation() {
        let mut probe = TerminalProbe::default();
        let old = Some(SystemTime::UNIX_EPOCH);
        let new = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1));
        probe
            .results
            .insert(42, (Instant::now(), old, Some(AgentState::Waiting)));
        probe.pending.insert(42, new);
        probe.tx.send((42, old, Some(AgentState::Waiting))).unwrap();
        assert_eq!(probe.request(42, new), None);
        probe.tx.send((42, new, Some(AgentState::Working))).unwrap();
        assert_eq!(probe.request(42, new), Some(AgentState::Working));
    }

    #[test]
    fn unsuccessful_refresh_releases_cached_confirmation() {
        let mut probe = TerminalProbe::default();
        let activity = Some(SystemTime::UNIX_EPOCH);
        probe.results.insert(
            42,
            (
                Instant::now() - Duration::from_secs(6),
                activity,
                Some(AgentState::Waiting),
            ),
        );
        probe.pending.insert(42, activity);
        probe.tx.send((42, activity, None)).unwrap();
        assert_eq!(probe.request(42, activity), None);
    }

    /// Opt in with ASI_TEST_CODEX_PID pointing to a Terminal-hosted Codex that
    /// is currently displaying a confirmation. Reads only; never answers it.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires a live Terminal confirmation and Automation permission"]
    fn live_terminal_confirmation() {
        let pid = std::env::var("ASI_TEST_CODEX_PID")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(probe_codex_sync(pid), Some(AgentState::Waiting));
    }
}
