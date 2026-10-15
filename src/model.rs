use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fmt, path::PathBuf, time::Duration};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Stopped,
    Ready,
    Working,
    WaitingReply,
    Waiting,
    Error,
}

impl AgentState {
    pub fn icon(self) -> &'static str {
        match self {
            Self::Waiting | Self::WaitingReply => "🟡",
            Self::Working => "🔵",
            Self::Ready => "🟢",
            Self::Stopped => "⚪",
            Self::Error => "🔴",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Waiting => crate::i18n::state("waiting"),
            Self::WaitingReply => crate::i18n::state("waiting_reply"),
            Self::Working => crate::i18n::state("working"),
            Self::Ready => crate::i18n::state("ready"),
            Self::Stopped => crate::i18n::state("stopped"),
            Self::Error => crate::i18n::state("error"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextUsage {
    pub used_tokens: u64,
    pub window_tokens: u64,
}

/// CPU and resident-memory usage of a row's process tree.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct ProcessUsage {
    /// CPU usage percentage, summed over the process tree. Can exceed 100 when
    /// several cores are busy.
    pub cpu: f32,
    /// Resident set size in bytes, summed over the process tree.
    pub memory_bytes: u64,
    /// True when the process is a GUI host shared by sibling sessions (several
    /// ChatGPT/DeepSeek conversations in one app-server). The figures are then
    /// host-level and cannot be attributed to a single session, so the row marks
    /// them as shared.
    pub shared: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentInstance {
    /// Stable row identity. Normally the process id; when one process hosts
    /// several sessions (the ChatGPT app-server drives many Codex threads) it
    /// is `<pid>:<thread>`, so every conversation owns its own menu row and its
    /// own notification state.
    #[serde(default)]
    pub key: String,
    pub kind: String,
    pub label: String,
    pub pid: u32,
    pub cwd: Option<PathBuf>,
    pub state: AgentState,
    pub uptime: Duration,
    pub model: Option<String>,
    pub context: Option<ContextUsage>,
    pub open_url: Option<String>,
    #[serde(default)]
    pub automatic_confirmation_mode: bool,
    /// A row that only reports something (for example how many conversations
    /// the menu left out). It has no session to focus, so it is rendered as
    /// plain text and excluded from the summary and from notifications.
    #[serde(default)]
    pub informational: bool,
    /// CPU/memory for the row's process tree. `None` for placeholder and
    /// informational rows, which have no process of their own.
    #[serde(default)]
    pub usage: Option<ProcessUsage>,
}

/// The agent families the resource summary always reports, in display order.
/// An idle agent still gets a column, so its absence shows as `--/--` instead
/// of disappearing.
pub const RESOURCE_AGENTS: [&str; 5] = ["Claude", "Codex", "OpenCode", "DeepSeek", "Pi"];

/// The summary column a row's `kind` belongs to. A hosted ChatGPT session is
/// still Codex's resource usage, and both DeepSeek forms share one column.
pub fn resource_group(kind: &str) -> Option<&'static str> {
    match kind {
        "Claude" => Some("Claude"),
        "Codex" | "ChatGPT" => Some("Codex"),
        "OpenCode" => Some("OpenCode"),
        _ if kind.starts_with("DeepSeek") => Some("DeepSeek"),
        "Pi" => Some("Pi"),
        _ => None,
    }
}

/// CPU and resident-memory totals for one agent family. `used` is false for an
/// agent with no live process, so the caller renders `--/--`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ResourceTotal {
    pub cpu: f32,
    pub memory_bytes: u64,
    pub used: bool,
}

/// Sum CPU/memory per agent family. Rows that share one host process (several
/// ChatGPT or DeepSeek conversations in one app-server) carry identical usage,
/// so a process is counted once by pid instead of once per conversation.
pub fn resource_totals(instances: &[AgentInstance]) -> Vec<ResourceTotal> {
    let mut totals = vec![ResourceTotal::default(); RESOURCE_AGENTS.len()];
    let mut seen = HashSet::new();
    for instance in instances {
        if instance.informational {
            continue;
        }
        let Some(usage) = instance.usage else {
            continue;
        };
        let Some(group) = resource_group(&instance.kind) else {
            continue;
        };
        if instance.pid != 0 && !seen.insert(instance.pid) {
            continue;
        }
        let Some(index) = RESOURCE_AGENTS.iter().position(|agent| *agent == group) else {
            continue;
        };
        totals[index].cpu += usage.cpu;
        totals[index].memory_bytes = totals[index]
            .memory_bytes
            .saturating_add(usage.memory_bytes);
        totals[index].used = true;
    }
    totals
}

impl fmt::Display for AgentInstance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {}: {}",
            self.state.icon(),
            self.label,
            self.state.label()
        )?;
        if self.uptime.as_secs() >= 60 {
            write!(f, " ({}m)", self.uptime.as_secs() / 60)?;
        }
        if let Some(model) = &self.model {
            write!(f, " · {model}")?;
        }
        if let Some(ctx) = &self.context {
            let percent = ctx.used_tokens as f64 * 100.0 / ctx.window_tokens.max(1) as f64;
            write!(
                f,
                " · {percent:.1}% ({}/{})",
                short_tokens(ctx.used_tokens),
                short_tokens(ctx.window_tokens)
            )?;
        }
        Ok(())
    }
}

fn short_tokens(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}m", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{}k", value / 1_000)
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(key: &str, kind: &str, pid: u32, usage: ProcessUsage) -> AgentInstance {
        AgentInstance {
            key: key.into(),
            kind: kind.into(),
            label: kind.into(),
            pid,
            cwd: None,
            state: AgentState::Working,
            uptime: Duration::ZERO,
            model: None,
            context: None,
            open_url: None,
            automatic_confirmation_mode: false,
            informational: false,
            usage: Some(usage),
        }
    }

    fn total<'a>(totals: &'a [ResourceTotal], agent: &str) -> &'a ResourceTotal {
        let index = RESOURCE_AGENTS
            .iter()
            .position(|candidate| *candidate == agent)
            .expect("a known agent");
        &totals[index]
    }

    #[test]
    fn resource_totals_sum_each_agent_and_leave_idle_ones_unused() {
        let usage = |cpu, memory_bytes| ProcessUsage {
            cpu,
            memory_bytes,
            shared: false,
        };
        let rows = [
            instance("1", "Pi", 1, usage(2.0, 100)),
            instance("2", "Pi", 2, usage(3.0, 200)),
            // A hosted ChatGPT conversation is Codex's usage.
            instance(
                "3",
                "ChatGPT",
                7,
                ProcessUsage {
                    cpu: 1.0,
                    memory_bytes: 500,
                    shared: true,
                },
            ),
        ];
        let totals = resource_totals(&rows);
        assert_eq!(total(&totals, "Pi").cpu, 5.0);
        assert_eq!(total(&totals, "Pi").memory_bytes, 300);
        assert!(total(&totals, "Pi").used);
        assert_eq!(total(&totals, "Codex").cpu, 1.0);
        assert!(total(&totals, "Codex").used);
        assert!(!total(&totals, "Claude").used);
        assert!(!total(&totals, "OpenCode").used);
        assert!(!total(&totals, "DeepSeek").used);
    }

    #[test]
    fn shared_host_rows_are_counted_once_per_process() {
        // Two conversations in one app-server carry identical host usage, so the
        // process must not be counted twice.
        let shared = ProcessUsage {
            cpu: 4.0,
            memory_bytes: 900,
            shared: true,
        };
        let rows = [
            instance("7:a", "ChatGPT", 7, shared),
            instance("7:b", "ChatGPT", 7, shared),
        ];
        let totals = resource_totals(&rows);
        assert_eq!(total(&totals, "Codex").cpu, 4.0);
        assert_eq!(total(&totals, "Codex").memory_bytes, 900);
    }

    #[test]
    fn informational_rows_do_not_contribute() {
        let mut row = instance(
            "1",
            "Pi",
            1,
            ProcessUsage {
                cpu: 9.0,
                memory_bytes: 9,
                shared: false,
            },
        );
        row.informational = true;
        let totals = resource_totals(&[row]);
        assert!(!total(&totals, "Pi").used);
    }
}
