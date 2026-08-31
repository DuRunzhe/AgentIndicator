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
}

impl MacProcessSource {
    pub fn processes(&self) -> Vec<ProcessRecord> {
        let Ok(output) = Command::new("/bin/ps")
            .args(["-axo", "pid=,ppid=,etime=,tty=,command="])
            .output()
        else {
            return vec![];
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_process_line)
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
                // than leaving a Codex rollout/cwd unavailable for 30 seconds.
                None => true,
                Some(cached) if cached.retry_soon => {
                    cached.checked_at.elapsed() >= Duration::from_secs(2)
                }
                Some(cached) => cached.checked_at.elapsed() >= Duration::from_secs(30),
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
}

fn parse_process_line(line: &str) -> Option<ProcessRecord> {
    let mut fields = line.split_whitespace();
    let pid = fields.next()?.parse().ok()?;
    let ppid = fields.next()?.parse().ok()?;
    let uptime = parse_etime(fields.next()?)?;
    let _tty = fields.next()?;
    let command = fields.collect::<Vec<_>>().join(" ");
    (!command.is_empty()).then_some(ProcessRecord {
        pid,
        ppid,
        uptime,
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
        let record =
            parse_process_line("42 1 01:02 ttys001 /opt/bin/codex resume thread-id").unwrap();
        assert_eq!(record.pid, 42);
        assert_eq!(record.ppid, 1);
        assert_eq!(record.uptime, Duration::from_secs(62));
        assert_eq!(record.command, "/opt/bin/codex resume thread-id");
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
