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
    use objc2::rc::Retained;
    use objc2_app_kit::{
        NSAlert, NSAlertFirstButtonReturn, NSApplication, NSFont, NSImage, NSImageScaling,
        NSImageView, NSTextAlignment, NSTextField, NSView,
    };
    use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

    /// The About content is a fully custom accessory view: app icon, name,
    /// version, repository and copyright are stacked and horizontally
    /// centered; NSAlert only supplies the buttons below.
    fn accessory_panel(mtm: MainThreadMarker) -> Retained<NSView> {
        let width = 340.0;
        let container = NSView::new(mtm);
        container.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(width, 176.0),
        ));

        let icon = NSImageView::new(mtm);
        if let Some(image) = NSApplication::sharedApplication(mtm).applicationIconImage() {
            icon.setImage(Some(&image));
        }
        icon.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        icon.setFrame(NSRect::new(
            NSPoint::new((width - 72.0) / 2.0, 96.0),
            NSSize::new(72.0, 72.0),
        ));
        container.addSubview(&icon);

        let name_font = NSFont::boldSystemFontOfSize(15.0);
        add_centered_label(mtm, &container, APP_NAME, 67.0, 22.0, Some(&name_font));
        add_centered_label(
            mtm,
            &container,
            &format!("{} {VERSION}", i18n::text("about_version")),
            46.0,
            17.0,
            None,
        );
        add_centered_label(mtm, &container, REPOSITORY, 27.0, 17.0, None);
        add_centered_label(
            mtm,
            &container,
            i18n::text("about_copyright"),
            8.0,
            17.0,
            None,
        );
        container
    }

    fn add_centered_label(
        mtm: MainThreadMarker,
        container: &NSView,
        text: &str,
        y: f64,
        height: f64,
        font: Option<&NSFont>,
    ) {
        let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
        label.setAlignment(NSTextAlignment::Center);
        if let Some(font) = font {
            label.setFont(Some(font));
        }
        let width = container.frame().size.width;
        label.setFrame(NSRect::new(
            NSPoint::new(0.0, y),
            NSSize::new(width, height),
        ));
        container.addSubview(&label);
    }

    pub fn show() {
        let mtm = MainThreadMarker::new().expect("about dialog must run on the main thread");
        let alert = NSAlert::new(mtm);
        // A blank icon collapses the alert's left icon column so the centered
        // accessory view spans the whole panel.
        unsafe { alert.setIcon(Some(&NSImage::new())) };
        alert.setAccessoryView(Some(&accessory_panel(mtm)));
        let _ = alert.addButtonWithTitle(&NSString::from_str(i18n::text("about_open_repository")));
        let _ = alert.addButtonWithTitle(&NSString::from_str(i18n::text("close")));
        if alert.runModal() == NSAlertFirstButtonReturn {
            super::open_url(REPOSITORY);
        }
    }
}
