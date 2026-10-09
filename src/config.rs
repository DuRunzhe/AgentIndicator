use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, time::Duration};

/// Conversation display windows, in menu order. Each entry is the key stored in
/// the config and how long a finished conversation keeps its row; `None` keeps
/// every conversation.
///
/// The same keys drive two independent settings: the hosted (ChatGPT) range and
/// the DeepSeek Harness range. A desktop conversation and a hosted one have
/// different lifetimes, so one range cannot serve both.
pub const CONVERSATION_WINDOWS: [(&str, Option<u64>); 5] = [
    ("15m", Some(15 * 60)),
    ("1h", Some(60 * 60)),
    ("12h", Some(12 * 60 * 60)),
    ("24h", Some(24 * 60 * 60)),
    ("all", None),
];

pub const DEFAULT_CONVERSATION_WINDOW: &str = "24h";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub notifications_enabled: bool,
    pub notify_waiting_confirmation: bool,
    pub notify_waiting_reply: bool,
    pub notify_error: bool,
    pub show_waiting_notifications_in_auto_confirm_mode: bool,
    pub show_duration: bool,
    pub show_model: bool,
    pub show_context_percent: bool,
    pub show_context_used: bool,
    pub show_context_total: bool,
    pub show_stopped_agents: bool,
    pub browser_tab_reuse: bool,
    pub locale: String,
    /// Which [`CONVERSATION_WINDOWS`] key applies to hosted (ChatGPT)
    /// conversations.
    pub conversation_window: String,
    /// Which [`CONVERSATION_WINDOWS`] key applies to the DeepSeek Harness
    /// desktop application's conversations. Independent of
    /// [`Self::conversation_window`]: the two are configured separately.
    pub deepseek_desktop_window: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            notifications_enabled: false,
            notify_waiting_confirmation: true,
            notify_waiting_reply: true,
            notify_error: true,
            show_waiting_notifications_in_auto_confirm_mode: true,
            show_duration: true,
            show_model: true,
            show_context_percent: true,
            show_context_used: true,
            show_context_total: true,
            show_stopped_agents: true,
            browser_tab_reuse: false,
            locale: "auto".into(),
            conversation_window: DEFAULT_CONVERSATION_WINDOW.into(),
            deepseek_desktop_window: DEFAULT_CONVERSATION_WINDOW.into(),
        }
    }
}

impl Config {
    /// How long a finished ChatGPT conversation keeps its row; `None` keeps it
    /// forever ("all"). An unknown value, such as one written by an older
    /// version, falls back to the default window.
    pub fn conversation_window_duration(&self) -> Option<Duration> {
        window_duration(&self.conversation_window)
    }

    /// How long a finished DeepSeek Harness desktop conversation keeps its row.
    /// Independent of [`Self::conversation_window_duration`].
    pub fn deepseek_desktop_window_duration(&self) -> Option<Duration> {
        window_duration(&self.deepseek_desktop_window)
    }

    pub fn load() -> Self {
        config_path()
            .and_then(|path| fs::File::open(path).ok())
            .and_then(|file| serde_json::from_reader(file).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) {
        let Some(path) = config_path() else { return };
        let Some(parent) = path.parent() else { return };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        if fs::File::create(&temporary)
            .ok()
            .and_then(|file| serde_json::to_writer_pretty(file, self).ok())
            .is_some()
        {
            let _ = fs::rename(temporary, path);
        }
    }
}

/// Resolve a [`CONVERSATION_WINDOWS`] key to a duration, falling back to the
/// default for a key this build does not know (such as one written by an older
/// version).
fn window_duration(key: &str) -> Option<Duration> {
    let known = CONVERSATION_WINDOWS
        .iter()
        .map(|(candidate, _)| *candidate)
        .find(|candidate| *candidate == key)
        .unwrap_or(DEFAULT_CONVERSATION_WINDOW);
    CONVERSATION_WINDOWS
        .iter()
        .find(|(candidate, _)| *candidate == known)
        .and_then(|(_, seconds)| seconds.map(Duration::from_secs))
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|root| root.join("agent-status-indicator/config.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_fields_default_to_visible() {
        let config: Config = serde_json::from_str(r#"{"notifications_enabled":true}"#).unwrap();
        assert!(config.notifications_enabled);
        assert!(config.notify_waiting_confirmation);
        assert!(config.notify_waiting_reply);
        assert!(config.notify_error);
        assert!(config.show_waiting_notifications_in_auto_confirm_mode);
        assert!(config.show_duration);
        assert!(config.show_model);
        assert!(config.show_context_percent);
        assert!(config.show_context_used);
        assert!(config.show_context_total);
        assert!(config.show_stopped_agents);
        assert_eq!(config.conversation_window, DEFAULT_CONVERSATION_WINDOW);
    }

    #[test]
    fn the_two_conversation_ranges_are_independent() {
        // The whole point of the separate setting: changing one must not move
        // the other. They share the key vocabulary, not the value.
        let mut config = Config::default();
        config.conversation_window = "1h".into();
        config.deepseek_desktop_window = "all".into();
        assert_eq!(
            config.conversation_window_duration(),
            Some(Duration::from_secs(60 * 60))
        );
        assert_eq!(config.deepseek_desktop_window_duration(), None);

        config.deepseek_desktop_window = "15m".into();
        assert_eq!(
            config.conversation_window_duration(),
            Some(Duration::from_secs(60 * 60)),
            "the hosted range must not follow the DeepSeek one"
        );
        assert_eq!(
            config.deepseek_desktop_window_duration(),
            Some(Duration::from_secs(15 * 60))
        );

        // An older config file has no DeepSeek key: it gets the default, and the
        // hosted value it did carry is preserved.
        let old: Config =
            serde_json::from_str(r#"{"conversation_window":"12h"}"#).expect("an older config");
        assert_eq!(old.conversation_window, "12h");
        assert_eq!(old.deepseek_desktop_window, DEFAULT_CONVERSATION_WINDOW);
    }

    #[test]
    fn conversation_window_keys_map_to_durations() {
        let window = |key: &str| {
            Config {
                conversation_window: key.into(),
                ..Default::default()
            }
            .conversation_window_duration()
        };
        assert_eq!(window("15m"), Some(Duration::from_secs(15 * 60)));
        assert_eq!(window("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(window("12h"), Some(Duration::from_secs(12 * 3600)));
        assert_eq!(window("24h"), Some(Duration::from_secs(24 * 3600)));
        // "all" keeps every conversation; an unknown value written by an older
        // version falls back to the default window.
        assert_eq!(window("all"), None);
        assert_eq!(window("nonsense"), Some(Duration::from_secs(24 * 3600)));
        assert_eq!(
            Config::default().conversation_window_duration(),
            Some(Duration::from_secs(24 * 3600))
        );
    }
}
