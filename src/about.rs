//! Native "About" dialog for the tray menu.
//!
//! Shows the application icon, version, source repository and copyright
//! notice. On macOS this is an AppKit `NSAlert` whose primary button opens
//! the repository in the default browser. Other platforms have no native
//! dialog backend in this crate, so the action falls back to opening the
//! repository page directly.

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

#[cfg(target_os = "macos")]
const APP_NAME: &str = env!("CARGO_PKG_NAME");
#[cfg(target_os = "macos")]
const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn show() {
    #[cfg(target_os = "macos")]
    macos::show();
    #[cfg(not(target_os = "macos"))]
    {
        let _ = open_url(REPOSITORY);
    }
}

/// Opens `url` in the user's default browser.
pub fn open_url(url: &str) -> bool {
    #[cfg(target_os = "windows")]
    let mut command = std::process::Command::new("cmd");
    #[cfg(not(target_os = "windows"))]
    let mut command = std::process::Command::new(if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    });

    #[cfg(target_os = "windows")]
    command.args(["/C", "start", "", url]);
    #[cfg(not(target_os = "windows"))]
    command.arg(url);

    command.status().is_ok_and(|status| status.success())
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{APP_NAME, REPOSITORY, VERSION};
    use crate::i18n;
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication};
    use objc2_foundation::{MainThreadMarker, NSString};

    pub fn show() {
        let mtm = MainThreadMarker::new().expect("about dialog must run on the main thread");
        let alert = NSAlert::new(mtm);
        // The dock / menu-bar application icon doubles as the About artwork.
        if let Some(icon) = NSApplication::sharedApplication(mtm).applicationIconImage() {
            unsafe { alert.setIcon(Some(&icon)) };
        }
        alert.setMessageText(&NSString::from_str(APP_NAME));
        let body = format!(
            "{} {VERSION}\n{}: {REPOSITORY}\n{}",
            i18n::text("about_version"),
            i18n::text("about_repository"),
            i18n::text("about_copyright"),
        );
        alert.setInformativeText(&NSString::from_str(&body));
        let _ = alert.addButtonWithTitle(&NSString::from_str(i18n::text("about_open_repository")));
        let _ = alert.addButtonWithTitle(&NSString::from_str(i18n::text("close")));
        if alert.runModal() == NSAlertFirstButtonReturn {
            super::open_url(REPOSITORY);
        }
    }
}
