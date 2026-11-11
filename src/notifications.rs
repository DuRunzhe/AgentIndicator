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
    /// Last *session-backed* state per row key, for every live row. A
    /// completion needs the previous state of a row that is *not* asking for
    /// attention (a working row gets no reminders), so this cannot live in
    /// [`Tracker`]. Scans whose state is only a process-activity guess leave
    /// this untouched: they say nothing about the task, and letting them
    /// overwrite the baseline would both fabricate completions and hide real
    /// ones.
    states: HashMap<String, AgentState>,
    /// Keyed by [`AgentInstance::key`]: one process can host several sessions,
    /// and each needs its own reminder schedule.
    reminders: HashMap<String, Tracker>,
}

impl NotificationTracker {
    pub fn update(
        &mut self,
        instances: &[AgentInstance],
        config: &crate::config::Config,
    ) -> Vec<NotificationRequest> {
        if !config.notifications_enabled {
            self.states.clear();
            self.reminders.clear();
            return vec![];
        }
        let mut due_notifications = vec![];
        let now = Instant::now();
        let live: std::collections::HashSet<&str> = instances
            .iter()
            .filter(|instance| !instance.informational)
            .map(|instance| instance.key.as_str())
            .collect();
        self.states.retain(|key, _| live.contains(key.as_str()));
        self.reminders.retain(|key, _| live.contains(key.as_str()));
        for instance in instances {
            if instance.informational {
                continue;
            }
            if instance.state_from_session {
                let previous = self.states.insert(instance.key.clone(), instance.state);
                if completed(previous, instance, config) {
                    due_notifications.push(NotificationRequest::from_instance(instance, 0));
                }
            }
            if !should_notify(instance, config) {
                self.reminders.remove(&instance.key);
                continue;
            }
            let tracker = self
                .reminders
                .entry(instance.key.clone())
                .or_insert(Tracker {
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

/// A finished turn: the row was working at the previous scan and is ready now.
/// The exact step is what makes it a completion — a row first seen as ready,
/// or one coming back from waiting or error, was not finishing anything. The
/// notice is one-shot by construction, because the transition itself is.
///
/// Both sides of the step come from the agent's session record: [`update`] only
/// calls this for a session-backed `instance`, and a process-activity guess
/// never enters [`NotificationTracker::states`]. A row that loses its session
/// binding falls back to "a running child means working", so a tool call that
/// starts and ends would otherwise look like a finished turn and send a
/// completion notice every time.
fn completed(
    previous: Option<AgentState>,
    instance: &AgentInstance,
    config: &crate::config::Config,
) -> bool {
    config.notify_completion
        && previous == Some(AgentState::Working)
        && instance.state == AgentState::Ready
}

pub fn should_notify(instance: &AgentInstance, config: &crate::config::Config) -> bool {
    if instance.informational {
        return false;
    }
    match instance.state {
        AgentState::Error => config.notify_error,
        AgentState::WaitingReply => config.notify_waiting_reply,
        AgentState::Waiting => {
            config.notify_waiting_confirmation
                && (config.show_waiting_notifications_in_auto_confirm_mode
                    || !instance.automatic_confirmation_mode)
        }
        _ => false,
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
        let body = crate::i18n::notification_message(body_key(instance.state), stage);
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

/// The message family a row's state speaks with. A ready row is only ever
/// announced as a completion, never as a reminder.
fn body_key(state: AgentState) -> &'static str {
    match state {
        AgentState::Error => "error",
        AgentState::WaitingReply => "waiting_reply",
        AgentState::Ready => "completion",
        _ => "waiting",
    }
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

    fn row(state: AgentState) -> AgentInstance {
        AgentInstance {
            key: "1".into(),
            kind: "Pi".into(),
            label: "Pi".into(),
            pid: 1,
            cwd: None,
            state,
            state_from_session: true,
            uptime: Duration::ZERO,
            model: None,
            context: None,
            open_url: None,
            automatic_confirmation_mode: false,
            informational: false,
            usage: None,
        }
    }

    fn enabled_config() -> crate::config::Config {
        crate::config::Config {
            notifications_enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn only_attention_states_are_eligible_for_notifications() {
        let config = crate::config::Config::default();
        let instance = |state| AgentInstance {
            key: "1".into(),
            kind: "Test".into(),
            label: "Test".into(),
            pid: 1,
            cwd: None,
            state,
            state_from_session: true,
            uptime: Duration::ZERO,
            model: None,
            context: None,
            open_url: None,
            automatic_confirmation_mode: false,
            informational: false,
            usage: None,
        };
        assert!(should_notify(&instance(AgentState::Waiting), &config));
        assert!(should_notify(&instance(AgentState::WaitingReply), &config));
        assert!(should_notify(&instance(AgentState::Error), &config));
        assert!(!should_notify(&instance(AgentState::Working), &config));
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
                key: "10".into(),
                kind: "DeepSeek Harness".into(),
                label: "DeepSeek".into(),
                pid: 10,
                cwd: None,
                state: AgentState::Waiting,
                state_from_session: true,
                uptime: Duration::ZERO,
                model: None,
                context: None,
                open_url: Some("http://127.0.0.1:3000".into()),
                automatic_confirmation_mode: false,
                informational: false,
                usage: None,
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
            key: "42".into(),
            kind: "Codex".into(),
            label: "Codex".into(),
            pid: 42,
            cwd: None,
            state: AgentState::WaitingReply,
            state_from_session: true,
            uptime: Duration::ZERO,
            model: None,
            context: None,
            open_url: None,
            automatic_confirmation_mode: false,
            informational: false,
            usage: None,
        };
        let mut config = crate::config::Config::default();
        config.notifications_enabled = true;
        let requests = tracker.update(&[instance], &config);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].action, NotificationAction::FocusPid(42));
        config.notifications_enabled = false;
        assert!(tracker.update(&[], &config).is_empty());
    }

    #[test]
    fn auto_confirm_waiting_can_be_silenced_without_silencing_replies() {
        let mut config = crate::config::Config::default();
        config.notifications_enabled = true;
        config.show_waiting_notifications_in_auto_confirm_mode = false;
        let waiting = AgentInstance {
            key: "7".into(),
            kind: "Codex".into(),
            label: "Codex".into(),
            pid: 7,
            cwd: None,
            state: AgentState::Waiting,
            state_from_session: true,
            uptime: Duration::ZERO,
            model: None,
            context: None,
            open_url: None,
            automatic_confirmation_mode: true,
            informational: false,
            usage: None,
        };
        assert!(!should_notify(&waiting, &config));
        let reply = AgentInstance {
            state: AgentState::WaitingReply,
            ..waiting
        };
        assert!(should_notify(&reply, &config));
    }

    #[test]
    fn finishing_a_turn_notifies_once_per_transition() {
        let config = enabled_config();
        let mut tracker = NotificationTracker::default();
        // A working row asks for nothing; it only sets up the completion below.
        assert!(tracker
            .update(&[row(AgentState::Working)], &config)
            .is_empty());
        let requests = tracker.update(&[row(AgentState::Ready)], &config);
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].body,
            crate::i18n::notification_message("completion", 0)
        );
        assert!(requests[0].title.ends_with(AgentState::Ready.label()));
        // A ready row has no reminder schedule, so later scans stay silent.
        assert!(tracker
            .update(&[row(AgentState::Ready)], &config)
            .is_empty());
        // The next turn finishes again and announces again.
        assert!(tracker
            .update(&[row(AgentState::Working)], &config)
            .is_empty());
        assert_eq!(tracker.update(&[row(AgentState::Ready)], &config).len(), 1);
    }

    #[test]
    fn only_the_working_to_ready_step_is_a_completion() {
        let config = enabled_config();
        // A row that is ready the first time it is seen finished nothing here.
        let mut tracker = NotificationTracker::default();
        assert!(tracker
            .update(&[row(AgentState::Ready)], &config)
            .is_empty());
        // Neither did a row coming back from another state: it was not working.
        for previous in [
            AgentState::Stopped,
            AgentState::Waiting,
            AgentState::WaitingReply,
            AgentState::Error,
        ] {
            let mut tracker = NotificationTracker::default();
            tracker.update(&[row(previous)], &config);
            assert!(
                tracker
                    .update(&[row(AgentState::Ready)], &config)
                    .is_empty(),
                "{previous:?} → ready must not read as a finished turn"
            );
        }
    }

    #[test]
    fn completion_notifications_can_be_switched_off() {
        let config = crate::config::Config {
            notify_completion: false,
            ..enabled_config()
        };
        let mut tracker = NotificationTracker::default();
        tracker.update(&[row(AgentState::Working)], &config);
        assert!(tracker
            .update(&[row(AgentState::Ready)], &config)
            .is_empty());
    }

    #[test]
    fn a_shared_process_announces_each_session_separately() {
        let config = enabled_config();
        let mut first = row(AgentState::Working);
        first.key = "9:a".into();
        let mut second = row(AgentState::Working);
        second.key = "9:b".into();
        let mut ready = second.clone();
        ready.state = AgentState::Ready;
        let mut tracker = NotificationTracker::default();
        assert!(tracker.update(&[first.clone(), second], &config).is_empty());
        // Only the conversation that just finished is announced, even though
        // both rows come from one host process.
        let requests = tracker.update(&[first, ready], &config);
        assert_eq!(requests.len(), 1);
        assert!(requests[0].title.starts_with("Pi"));
    }

    #[test]
    fn a_process_only_state_never_announces_a_completion() {
        let config = enabled_config();
        // A row that lost its session binding says "working" only because a
        // child process is running; when the child exits it turns ready again.
        // No turn finished, so neither edge may notify.
        let mut guess_working = row(AgentState::Working);
        guess_working.state_from_session = false;
        let mut tracker = NotificationTracker::default();
        tracker.update(&[guess_working], &config);
        assert!(tracker
            .update(&[row(AgentState::Ready)], &config)
            .is_empty());

        // The reverse edge counts too: a ready row that is only a process
        // guess did not necessarily finish the turn that was running.
        let mut guess_ready = row(AgentState::Ready);
        guess_ready.state_from_session = false;
        let mut tracker = NotificationTracker::default();
        tracker.update(&[row(AgentState::Working)], &config);
        assert!(tracker.update(&[guess_ready], &config).is_empty());
    }

    #[test]
    fn an_unbacked_scan_does_not_erase_the_last_session_state() {
        let config = enabled_config();
        let mut tracker = NotificationTracker::default();
        tracker.update(&[row(AgentState::Working)], &config);
        // A scan that lost the session binding says "ready" only because no
        // child is running. It must neither notify nor replace the working
        // baseline, so the real turn end still announces once.
        let mut guess = row(AgentState::Ready);
        guess.state_from_session = false;
        assert!(tracker.update(&[guess], &config).is_empty());
        assert_eq!(tracker.update(&[row(AgentState::Ready)], &config).len(), 1);
    }

    #[test]
    fn informational_rows_never_announce_a_completion() {
        let config = enabled_config();
        let mut working = row(AgentState::Working);
        working.informational = true;
        let mut ready = working.clone();
        ready.state = AgentState::Ready;
        let mut tracker = NotificationTracker::default();
        tracker.update(&[working], &config);
        assert!(tracker.update(&[ready], &config).is_empty());
    }
}
