use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf, time::Duration};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Stopped,
    Ready,
    Working,
    WaitingReply,
    Waiting,
}

impl AgentState {
    pub fn icon(self) -> &'static str {
        match self {
            Self::Waiting | Self::WaitingReply => "🟡",
            Self::Working => "🔵",
            Self::Ready => "🟢",
            Self::Stopped => "⚪",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Waiting => "等待确认",
            Self::WaitingReply => "等待回复",
            Self::Working => "进行中",
            Self::Ready => "就绪",
            Self::Stopped => "已停止",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextUsage {
    pub used_tokens: u64,
    pub window_tokens: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentInstance {
    pub kind: String,
    pub label: String,
    pub pid: u32,
    pub cwd: Option<PathBuf>,
    pub state: AgentState,
    pub uptime: Duration,
    pub model: Option<String>,
    pub context: Option<ContextUsage>,
    pub open_url: Option<String>,
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
