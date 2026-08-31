use crate::{dialog, i18n};
use std::process::Command;

pub fn configure(currently_enabled: bool) -> bool {
    if currently_enabled {
        return false;
    }
    let choice = dialog::choose(
        i18n::text("browser_prompt"),
        i18n::menu("browser"),
        &[i18n::text("cancel"), i18n::text("continue")],
        i18n::text("continue"),
        Some(i18n::text("cancel")),
    );
    if choice.as_deref() != Some(i18n::text("continue")) {
        return false;
    }
    if !probe_automation() {
        let _ = open_automation_settings();
        let verified = dialog::choose(
            i18n::text("browser_verify"),
            i18n::text("browser_verify_title"),
            &[i18n::text("not_yet"), i18n::text("authorized")],
            i18n::text("authorized"),
            Some(i18n::text("not_yet")),
        );
        if verified.as_deref() != Some(i18n::text("authorized")) {
            return false;
        }
    }
    dialog::notice(i18n::text("browser_enabled"), i18n::menu("browser"));
    true
}

pub fn open_automation_settings() -> bool {
    #[cfg(target_os = "macos")]
    return Command::new("/usr/bin/open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Automation")
        .status()
        .is_ok_and(|status| status.success());
    #[cfg(not(target_os = "macos"))]
    false
}

fn probe_automation() -> bool {
    #[cfg(target_os = "macos")]
    return Command::new("/usr/bin/osascript")
        .args(["-e", "tell application \"Google Chrome\" to if it is running then get URL of tabs of windows"])
        .output().is_ok_and(|output| output.status.success());
    #[cfg(not(target_os = "macos"))]
    false
}
