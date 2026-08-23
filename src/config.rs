use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub notifications_enabled: bool,
    pub show_duration: bool,
    pub show_model: bool,
    pub show_context_percent: bool,
    pub show_context_used: bool,
    pub show_context_total: bool,
    pub browser_tab_reuse: bool,
    pub locale: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            notifications_enabled: false,
            show_duration: true,
            show_model: true,
            show_context_percent: true,
            show_context_used: true,
            show_context_total: true,
            browser_tab_reuse: false,
            locale: "auto".into(),
        }
    }
}

impl Config {
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

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|root| root.join("agent-status-indicator/config.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_display_fields_default_to_visible() {
        let config: Config = serde_json::from_str(r#"{"notifications_enabled":true}"#).unwrap();
        assert!(config.notifications_enabled);
        assert!(config.show_duration);
        assert!(config.show_model);
        assert!(config.show_context_percent);
        assert!(config.show_context_used);
        assert!(config.show_context_total);
    }
}
