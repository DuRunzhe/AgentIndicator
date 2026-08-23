use crate::model::{AgentInstance, AgentState};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

const REMINDERS: [Duration; 3] = [
    Duration::ZERO,
    Duration::from_secs(60),
    Duration::from_secs(180),
];

#[derive(Clone)]
struct Tracker {
    state: AgentState,
    since: Instant,
    sent: usize,
}

#[derive(Default)]
pub struct NotificationTracker {
    instances: HashMap<u32, Tracker>,
}

impl NotificationTracker {
    pub fn update(&mut self, instances: &[AgentInstance], enabled: bool) {
        if !enabled {
            self.instances.clear();
            return;
        }
        let now = Instant::now();
        let live: std::collections::HashSet<_> = instances.iter().map(|i| i.pid).collect();
        self.instances.retain(|pid, _| live.contains(pid));
        for instance in instances {
            if !is_attention(instance.state) {
                self.instances.remove(&instance.pid);
                continue;
            }
            let tracker = self.instances.entry(instance.pid).or_insert(Tracker {
                state: instance.state,
                since: now,
                sent: 0,
            });
            if tracker.state != instance.state {
                *tracker = Tracker {
                    state: instance.state,
                    since: now,
                    sent: 0,
                };
            }
            let elapsed = now.duration_since(tracker.since);
            let due = reminder_stage(elapsed);
            if tracker.sent <= due {
                send(instance, due);
                tracker.sent = due + 1;
            }
        }
    }
}

fn is_attention(state: AgentState) -> bool {
    matches!(state, AgentState::Waiting | AgentState::WaitingReply)
}

fn reminder_stage(elapsed: Duration) -> usize {
    REMINDERS
        .iter()
        .rposition(|delay| elapsed >= *delay)
        .unwrap_or(0)
}

fn send(instance: &AgentInstance, stage: usize) {
    let message = match stage {
        0 => "需要你的操作",
        1 => "已等待 1 分钟",
        _ => "已等待 3 分钟",
    };
    let _ = notify_rust::Notification::new()
        .appname("AgentStatusIndicator")
        .summary(&format!("{} · {}", instance.label, instance.state.label()))
        .body(message)
        .show();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_attention_states_are_tracked() {
        assert!(is_attention(AgentState::Waiting));
        assert!(is_attention(AgentState::WaitingReply));
        assert!(!is_attention(AgentState::Working));
    }

    #[test]
    fn reminder_schedule_matches_reference_behavior() {
        assert_eq!(reminder_stage(Duration::from_secs(0)), 0);
        assert_eq!(reminder_stage(Duration::from_secs(59)), 0);
        assert_eq!(reminder_stage(Duration::from_secs(60)), 1);
        assert_eq!(reminder_stage(Duration::from_secs(179)), 1);
        assert_eq!(reminder_stage(Duration::from_secs(180)), 2);
        assert_eq!(reminder_stage(Duration::from_secs(600)), 2);
    }
}
