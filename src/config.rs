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

/// The widest table that is still useful on first launch: today's usage.
pub const DEFAULT_USAGE_PERIOD: &str = "day";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub notifications_enabled: bool,
    pub notify_waiting_confirmation: bool,
    pub notify_waiting_reply: bool,
    /// One-shot notice when a row finishes a turn: working → ready.
    pub notify_completion: bool,
    pub notify_error: bool,
    pub show_waiting_notifications_in_auto_confirm_mode: bool,
    pub show_duration: bool,
    pub show_model: bool,
    pub show_context_percent: bool,
    pub show_context_used: bool,
    pub show_context_total: bool,
    /// Whether a session row carries its CPU figure, and whether the table gets
    /// its CPU row.
    ///
    /// One switch for one number: the figure on the row and the one in the
    /// table are the same measurement, so asking for one and hiding the other
    /// would only make the two easy to confuse.
    pub show_cpu: bool,
    /// Whether a session row carries its memory figure, and whether the table
    /// gets its memory row.
    pub show_memory: bool,
    /// The token rows of the merged table, and the day/week/month switch above
    /// it.
    pub show_usage: bool,
    /// Which [`crate::usage::Period`] the token table covers.
    pub usage_period: String,
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
    /// Whether the system accessibility prompt has already been shown.
    ///
    /// macOS shows that prompt at most once per signature, and it is also the
    /// only thing that puts this application in the Accessibility list. So the
    /// first ask spends the prompt and later asks open the pane directly, which
    /// keeps one click to one window.
    pub deepseek_accessibility_prompted: bool,
    /// Whether clicking a DeepSeek Harness desktop row may target the
    /// conversation itself, not just the window.
    ///
    /// A switch rather than a mode: the ladder always degrades on its own, so
    /// turning this off is only ever "stop trying to reach the conversation".
    pub deepseek_session_jump: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            notifications_enabled: false,
            notify_waiting_confirmation: true,
            notify_waiting_reply: true,
            notify_completion: true,
            notify_error: true,
            show_waiting_notifications_in_auto_confirm_mode: true,
            show_duration: true,
            show_model: true,
            show_context_percent: true,
            show_context_used: true,
            show_context_total: true,
            // A fresh install shows the token rows only. The two resource rows
            // are opt-in, and each switch answers for both places the figure
            // appears — the session row and the table.
            show_cpu: false,
            show_memory: false,
            show_usage: true,
            usage_period: DEFAULT_USAGE_PERIOD.into(),
            show_stopped_agents: true,
            browser_tab_reuse: false,
            locale: "auto".into(),
            conversation_window: DEFAULT_CONVERSATION_WINDOW.into(),
            deepseek_desktop_window: DEFAULT_CONVERSATION_WINDOW.into(),
            deepseek_session_jump: true,
            deepseek_accessibility_prompted: false,
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

    /// The calendar window the token table currently shows. An unknown value,
    /// such as one written by an older build, falls back to the day view.
    pub fn usage_period(&self) -> crate::usage::Period {
        crate::usage::Period::from_key(&self.usage_period)
    }

    pub fn load() -> Self {
        let Some(file) = config_path().and_then(|path| fs::File::open(path).ok()) else {
            return Self::default();
        };
        let Ok(value) = serde_json::from_reader::<_, serde_json::Value>(file) else {
            return Self::default();
        };
        Self::from_value(value)
    }

    /// Read a config file, folding in the retired `show_resources` gate.
    ///
    /// That key used to gate the table's two resource rows while `show_cpu` and
    /// `show_memory` answered only for the session rows. The gate is gone and
    /// those two now answer for both places, so a file that had the gate closed
    /// means the figures were unwanted wherever they appeared: keeping the two
    /// switches as they stand would resurrect rows the user had turned off. The
    /// key disappears the next time the file is written.
    fn from_value(mut value: serde_json::Value) -> Self {
        if let Some(object) = value.as_object_mut() {
            let gate_closed =
                object.get("show_resources").and_then(|gate| gate.as_bool()) == Some(false);
            if gate_closed {
                object.insert("show_cpu".into(), serde_json::Value::Bool(false));
                object.insert("show_memory".into(), serde_json::Value::Bool(false));
            }
        }
        serde_json::from_value(value).unwrap_or_default()
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
    fn missing_config_fields_fall_back_to_the_defaults() {
        let config: Config = serde_json::from_str(r#"{"notifications_enabled":true}"#).unwrap();
        assert!(config.notifications_enabled);
        assert!(config.notify_waiting_confirmation);
        assert!(config.notify_waiting_reply);
        assert!(config.notify_completion);
        assert!(config.notify_error);
        assert!(config.show_waiting_notifications_in_auto_confirm_mode);
        assert!(config.show_duration);
        assert!(config.show_model);
        assert!(config.show_context_percent);
        assert!(config.show_context_used);
        assert!(config.show_context_total);
        // The resource rows are opt-in: a fresh install shows the token table
        // and leaves the two resource switches off.
        assert!(!config.show_cpu);
        assert!(!config.show_memory);
        assert!(config.show_usage);
        assert_eq!(config.usage_period, DEFAULT_USAGE_PERIOD);
        assert!(config.show_stopped_agents);
        assert_eq!(config.conversation_window, DEFAULT_CONVERSATION_WINDOW);
    }

    #[test]
    fn a_retired_resource_gate_folds_into_the_two_row_switches() {
        // The gate used to hide the table's CPU and memory rows while those two
        // switches still governed the session rows, so a closed gate means the
        // figures were unwanted in both places. Neither may come back on
        // upgrade, and the retired key must not survive the next save.
        let closed = serde_json::from_str::<serde_json::Value>(
            r#"{"show_resources":false,"show_cpu":true,"show_memory":true}"#,
        )
        .expect("a legacy config");
        let config = Config::from_value(closed);
        assert!(!config.show_cpu);
        assert!(!config.show_memory);

        // An open gate leaves each switch exactly as the user left it.
        let open = serde_json::from_str::<serde_json::Value>(
            r#"{"show_resources":true,"show_cpu":true,"show_memory":false}"#,
        )
        .expect("a legacy config");
        let config = Config::from_value(open);
        assert!(config.show_cpu);
        assert!(!config.show_memory);

        // Saving drops the retired key instead of carrying it forward.
        let json = serde_json::to_string(&config).expect("serialise");
        assert!(!json.contains("show_resources"), "{json}");
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
    fn the_two_ranges_round_trip_as_separate_fields() {
        // Function and data, not just UI: the two choices must persist under
        // separate keys and come back unchanged.
        let mut config = Config::default();
        config.conversation_window = "15m".into();
        config.deepseek_desktop_window = "12h".into();
        let json = serde_json::to_string(&config).expect("serialise");
        assert!(json.contains(r#""conversation_window":"15m""#), "{json}");
        assert!(
            json.contains(r#""deepseek_desktop_window":"12h""#),
            "the second range must have its own persisted key: {json}"
        );
        let back: Config = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.conversation_window, "15m");
        assert_eq!(back.deepseek_desktop_window, "12h");
    }

    #[test]
    fn the_session_jump_switch_defaults_on_and_round_trips() {
        assert!(
            Config::default().deepseek_session_jump,
            "a fresh install must try the deepest destination it can reach"
        );
        // An older config file does not carry the key: missing means on, so an
        // upgrade improves the click instead of silently keeping the old one.
        let old: Config = serde_json::from_str(r#"{"show_usage":false}"#).expect("older config");
        assert!(old.deepseek_session_jump);

        let mut config = Config::default();
        config.deepseek_session_jump = false;
        let json = serde_json::to_string(&config).expect("serialise");
        assert!(json.contains(r#""deepseek_session_jump":false"#), "{json}");
        let back: Config = serde_json::from_str(&json).expect("deserialise");
        assert!(!back.deepseek_session_jump);
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
