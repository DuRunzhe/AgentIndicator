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
    pub fn update(
        &mut self,
        instances: &[AgentInstance],
        enabled: bool,
    ) -> Vec<NotificationRequest> {
        if !enabled {
            self.instances.clear();
            return vec![];
        }
        let mut due_notifications = vec![];
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
                due_notifications.push(NotificationRequest::from_instance(instance, due));
                tracker.sent = due + 1;
            }
        }
        due_notifications
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationAction {
    FocusPid(u32),
    FocusUrl { url: String, reuse_tabs: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationRequest {
    pub title: String,
    pub body: String,
    pub action: NotificationAction,
}

impl NotificationRequest {
    fn from_instance(instance: &AgentInstance, stage: usize) -> Self {
        let body = crate::i18n::notification_message(
            match instance.state {
                AgentState::WaitingReply => "waiting_reply",
                _ => "waiting",
            },
            stage,
        );
        let action = instance
            .open_url
            .as_ref()
            .map(|url| NotificationAction::FocusUrl {
                url: url.clone(),
                // This setting is applied by App just before delivery. The default
                // keeps requests useful to non-UI callers and tests.
                reuse_tabs: true,
            })
            .unwrap_or(NotificationAction::FocusPid(instance.pid));
        Self {
            title: format!("{} · {}", instance.label, instance.state.label()),
            body: body.into(),
            action,
        }
    }

    pub fn with_browser_tab_reuse(mut self, reuse_tabs: bool) -> Self {
        if let NotificationAction::FocusUrl {
            reuse_tabs: value, ..
        } = &mut self.action
        {
            *value = reuse_tabs;
        }
        self
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

    #[test]
    fn web_instance_uses_url_focus_action() {
        let request = NotificationRequest::from_instance(
            &AgentInstance {
                kind: "DeepSeek Harness".into(),
                label: "DeepSeek".into(),
                pid: 10,
                cwd: None,
                state: AgentState::Waiting,
                uptime: Duration::ZERO,
                model: None,
                context: None,
                open_url: Some("http://127.0.0.1:3000".into()),
            },
            0,
        )
        .with_browser_tab_reuse(false);
        assert_eq!(
            request.action,
            NotificationAction::FocusUrl {
                url: "http://127.0.0.1:3000".into(),
                reuse_tabs: false,
            }
        );
    }

    #[test]
    fn first_attention_state_emits_one_notification() {
        let mut tracker = NotificationTracker::default();
        let instance = AgentInstance {
            kind: "Codex".into(),
            label: "Codex".into(),
            pid: 42,
            cwd: None,
            state: AgentState::WaitingReply,
            uptime: Duration::ZERO,
            model: None,
            context: None,
            open_url: None,
        };
        let requests = tracker.update(&[instance], true);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].action, NotificationAction::FocusPid(42));
        assert!(tracker.update(&[], false).is_empty());
    }
}
