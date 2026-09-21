use std::{
    collections::HashMap,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct ProcessRecord {
    pub pid: u32,
    pub ppid: u32,
    pub uptime: Duration,
    /// The executed program's path, from `ps comm=`. Paths with spaces (for
    /// example "Codex Framework.framework") stay intact here, unlike `command`.
    pub executable: String,
    /// The full command line, from `ps command=`. Only its arguments are
    /// meaningful; its leading token can end mid-path when the path contains a
    /// space.
    pub command: String,
}

#[derive(Clone, Debug, Default)]
pub struct ProcessMetadata {
    pub cwd: Option<PathBuf>,
    pub files: Vec<PathBuf>,
}

#[derive(Default)]
pub struct MacProcessSource {
    metadata: HashMap<u32, CachedMetadata>,
}

struct CachedMetadata {
    value: ProcessMetadata,
    checked_at: Instant,
    retry_soon: bool,
    rollout_checked_at: Option<Instant>,
}

// Rollout files can be added to an already-running host process when the user
// opens or resumes a Codex conversation. Keep this at the monitor cadence so
// those sessions become visible without waiting for the long-lived process
// metadata cache to expire.
const METADATA_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const INCOMPLETE_METADATA_RETRY_INTERVAL: Duration = Duration::from_secs(2);
const ROLLOUT_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

impl MacProcessSource {
    pub fn processes(&self) -> Vec<ProcessRecord> {
        let Ok(output) = Command::new("/bin/ps")
            .args(["-axo", "pid=,ppid=,etime=,tty=,command="])
            .output()
        else {
            return vec![];
        };
        let executables = self.executable_paths();
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| parse_process_line(line, &executables))
            .collect()
    }

    /// The executed program per pid. Kept separate from the `command=` parse
    /// because whitespace splitting breaks paths containing spaces: the leading
    /// token of `.../Frameworks/Codex Framework.framework/...` is
    /// `.../Frameworks/Codex`, whose basename looks like the Codex agent.
    fn executable_paths(&self) -> HashMap<u32, String> {
        let Ok(output) = Command::new("/bin/ps")
            .args(["-axo", "pid=,comm="])
            .output()
        else {
            return HashMap::new();
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let (pid, path) = line.trim_start().split_once(char::is_whitespace)?;
                Some((pid.parse().ok()?, path.trim().to_owned()))
            })
            .collect()
    }

    pub fn metadata_for(&mut self, pids: &[u32]) -> HashMap<u32, ProcessMetadata> {
        let now = Instant::now();
        let missing: Vec<_> = pids
            .iter()
            .copied()
            .filter(|pid| match self.metadata.get(pid) {
                // A newly created/resumed process often races its first lsof
                // read. Retry incomplete bindings at the monitor cadence rather
                // than leaving the Codex rollout/cwd unavailable across scans.
                None => true,
                Some(cached) if cached.retry_soon => {
                    cached.checked_at.elapsed() >= INCOMPLETE_METADATA_RETRY_INTERVAL
                }
                Some(cached) => cached.checked_at.elapsed() >= METADATA_REFRESH_INTERVAL,
            })
            .collect();
        if !missing.is_empty() {
            let fresh = read_lsof_metadata(&missing);
            for pid in missing {
                // A process can race lsof during resume/exit. Keep the last
                // complete metadata instead of replacing cwd/session binding
                // with an empty record and briefly regressing the tray state.
                let value = fresh
                    .get(&pid)
                    .cloned()
                    .or_else(|| self.metadata.get(&pid).map(|cached| cached.value.clone()))
                    .unwrap_or_default();
                let retry_soon = value.cwd.is_none();
                self.metadata.insert(
                    pid,
                    CachedMetadata {
                        value,
                        checked_at: now,
                        retry_soon,
                        // The initial metadata read already includes rollout
                        // handles; schedule the targeted follow-up from the
                        // next monitor tick instead of issuing two lsof calls.
                        rollout_checked_at: Some(now),
                    },
                );
            }
        }
        self.metadata.retain(|pid, _| pids.contains(pid));
        pids.iter()
            .filter_map(|pid| {
                self.metadata
                    .get(pid)
                    .map(|cached| (*pid, cached.value.clone()))
            })
            .collect()
    }

    /// Refresh only rollout handles for Codex process trees. Codex hosts can
    /// open a new session while their process stays alive, so this is kept at
    /// the monitor cadence without making every agent pay for a high-frequency
    /// lsof call.
    pub fn refresh_codex_rollouts(&mut self, pids: &[u32]) {
        let now = Instant::now();
        let due: Vec<_> = pids
            .iter()
            .copied()
            .filter(|pid| {
                self.metadata
                    .get(pid)
                    .and_then(|cached| cached.rollout_checked_at)
                    .is_none_or(|at| at.elapsed() >= ROLLOUT_REFRESH_INTERVAL)
            })
            .collect();
        if due.is_empty() {
            return;
        }
        let fresh = read_lsof_metadata(&due);
        for pid in due {
            let Some(cached) = self.metadata.get_mut(&pid) else {
                continue;
            };
            // Keep cwd from the slower metadata pass, but replace rollout
            // handles so sessions opened by a long-lived host appear quickly.
            if let Some(metadata) = fresh.get(&pid) {
                cached.value.files = metadata.files.clone();
            }
            cached.rollout_checked_at = Some(now);
        }
    }
}

fn parse_process_line(line: &str, executables: &HashMap<u32, String>) -> Option<ProcessRecord> {
    let mut fields = line.split_whitespace();
    let pid = fields.next()?.parse().ok()?;
    let ppid = fields.next()?.parse().ok()?;
    let uptime = parse_etime(fields.next()?)?;
    let _tty = fields.next()?;
    let command = fields.collect::<Vec<_>>().join(" ");
    if command.is_empty() {
        return None;
    }
    // Fall back to the command line's leading token when `ps comm=` omitted the
    // pid, which is no worse than the previous behavior.
    let executable = executables.get(&pid).cloned().unwrap_or_else(|| {
        command
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned()
    });
    Some(ProcessRecord {
        pid,
        ppid,
        uptime,
        executable,
        command,
    })
}

fn parse_etime(value: &str) -> Option<Duration> {
    let (days, clock) = match value.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, value),
    };
    let values: Vec<_> = clock
        .split(':')
        .map(|part| part.parse::<u64>().ok())
        .collect();
    let values: Option<Vec<_>> = values.into_iter().collect();
    let values = values?;
    let seconds = match values.as_slice() {
        [minutes, seconds] => minutes * 60 + seconds,
        [hours, minutes, seconds] => hours * 3600 + minutes * 60 + seconds,
        _ => return None,
    };
    Some(Duration::from_secs(days * 86_400 + seconds))
}

fn read_lsof_metadata(pids: &[u32]) -> HashMap<u32, ProcessMetadata> {
    let list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let Ok(output) = Command::new("/usr/sbin/lsof")
        .args(["-Fn", "-p", &list])
        .output()
    else {
        return HashMap::new();
    };
    parse_lsof_metadata(&String::from_utf8_lossy(&output.stdout))
}

fn parse_lsof_metadata(output: &str) -> HashMap<u32, ProcessMetadata> {
    let mut result: HashMap<u32, ProcessMetadata> = HashMap::new();
    let mut pid = None;
    let mut want_cwd = false;
    for line in output.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse().ok();
            want_cwd = false;
        } else if line == "fcwd" {
            want_cwd = true;
        } else if let (Some(current), Some(value)) = (pid, line.strip_prefix('n')) {
            let metadata = result.entry(current).or_default();
            if want_cwd {
                metadata.cwd = Some(PathBuf::from(value));
                want_cwd = false;
            } else if value.ends_with(".jsonl") {
                metadata.files.push(PathBuf::from(value));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ps_snapshot_line() {
        let executables = HashMap::from([(42, "/opt/bin/codex".to_owned())]);
        let record = parse_process_line(
            "42 1 01:02 ttys001 /opt/bin/codex resume thread-id",
            &executables,
        )
        .unwrap();
        assert_eq!(record.pid, 42);
        assert_eq!(record.ppid, 1);
        assert_eq!(record.uptime, Duration::from_secs(62));
        assert_eq!(record.executable, "/opt/bin/codex");
        assert_eq!(record.command, "/opt/bin/codex resume thread-id");
    }

    #[test]
    fn falls_back_to_the_command_token_when_comm_is_missing() {
        let record =
            parse_process_line("42 1 01:02 ttys001 /opt/bin/codex", &HashMap::new()).unwrap();
        assert_eq!(record.executable, "/opt/bin/codex");
    }

    #[test]
    fn associates_lsof_session_files_with_the_correct_pid() {
        let metadata = parse_lsof_metadata(
            "p42\nfcwd\nn/project\nf12\nn/home/user/.codex/sessions/a/rollout-a.jsonl\np43\nfcwd\nn/child\nf7\nn/home/user/.codex/sessions/a/rollout-b.jsonl\n",
        );
        assert_eq!(metadata[&42].cwd, Some(PathBuf::from("/project")));
        assert_eq!(metadata[&42].files.len(), 1);
        assert_eq!(
            metadata[&43].files[0],
            PathBuf::from("/home/user/.codex/sessions/a/rollout-b.jsonl")
        );
    }
}
