//! Native "About" dialog for the tray menu.
//!
//! On macOS the dialog is a small custom `NSWindow`: app icon, name, version,
//! repository and copyright are stacked with even padding and horizontally
//! centered, with an "Open repository" and a "Close" button underneath. A
//! plain window is used instead of `NSAlert` so the alert chrome at the top
//! can be dropped and the inner padding fully controlled.
//!
//! Other platforms have no native dialog backend in this crate, so the action
//! falls back to opening the repository page directly.

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
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, NSObject, Sel};
    use objc2::{msg_send, sel, ClassType};
    use objc2_app_kit::{
        NSApplication, NSButton, NSColor, NSFont, NSImageScaling, NSImageView, NSTextAlignment,
        NSTextField, NSView, NSWindow, NSWindowStyleMask, NSWindowTitleVisibility,
    };
    use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};
    use std::sync::OnceLock;

    const PANEL_WIDTH: f64 = 340.0;
    const PANEL_HEIGHT: f64 = 264.0;
    const ICON_SIZE: f64 = 64.0;
    const BUTTON_HEIGHT: f64 = 26.0;
    const OPEN_BUTTON_WIDTH: f64 = 150.0;
    const CLOSE_BUTTON_WIDTH: f64 = 84.0;
    const BUTTON_GAP: f64 = 12.0;
    const BUTTONS_WIDTH: f64 = OPEN_BUTTON_WIDTH + BUTTON_GAP + CLOSE_BUTTON_WIDTH;

    /// Returns the shared action target backing the panel's buttons.
    ///
    /// A tiny Objective-C class is registered once; the buttons only need to
    /// forward two actions, so the instance itself carries no state.
    fn action_target() -> Retained<AnyObject> {
        static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();
        let class = *CLASS.get_or_init(|| {
            let mut builder =
                ClassBuilder::new(c"AgentStatusIndicatorAboutTarget", NSObject::class())
                    .expect("about action target class registered twice");

            unsafe extern "C-unwind" fn open_repository(_this: &AnyObject, _cmd: Sel) {
                let _ = super::open_url(REPOSITORY);
            }

            unsafe extern "C-unwind" fn close_panel(_this: &AnyObject, _cmd: Sel) {
                let Some(mtm) = MainThreadMarker::new() else {
                    return;
                };
                let app = NSApplication::sharedApplication(mtm);
                if let Some(window) = app.keyWindow() {
                    window.close();
                }
                app.stopModal();
            }

            unsafe {
                builder.add_method(
                    sel!(openRepository),
                    open_repository as unsafe extern "C-unwind" fn(_, _),
                );
                builder.add_method(
                    sel!(closePanel),
                    close_panel as unsafe extern "C-unwind" fn(_, _),
                );
            }
            builder.register()
        });

        unsafe { msg_send![class, new] }
    }

    /// Builds the panel's content view.
    ///
    /// Coordinates are AppKit style (origin at the bottom-left). The vertical
    /// rhythm is laid out explicitly so the padding stays even: ~29pt above
    /// the icon and 24pt below the buttons.
    fn panel_content(mtm: MainThreadMarker, target: &AnyObject) -> Retained<NSView> {
        let container = NSView::new(mtm);
        container.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(PANEL_WIDTH, PANEL_HEIGHT),
        ));

        let icon = NSImageView::new(mtm);
        if let Some(image) = NSApplication::sharedApplication(mtm).applicationIconImage() {
            icon.setImage(Some(&image));
        }
        icon.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        icon.setFrame(NSRect::new(
            NSPoint::new((PANEL_WIDTH - ICON_SIZE) / 2.0, 171.0),
            NSSize::new(ICON_SIZE, ICON_SIZE),
        ));
        container.addSubview(&icon);

        let name_font = NSFont::boldSystemFontOfSize(15.0);
        add_centered_label(
            mtm,
            &container,
            APP_NAME,
            133.0,
            22.0,
            Some(&name_font),
            true,
        );
        add_centered_label(
            mtm,
            &container,
            &format!("{} {VERSION}", i18n::text("about_version")),
            112.0,
            17.0,
            None,
            false,
        );
        add_centered_label(mtm, &container, REPOSITORY, 92.0, 16.0, None, false);
        add_centered_label(
            mtm,
            &container,
            i18n::text("about_copyright"),
            70.0,
            16.0,
            None,
            false,
        );

        add_button(
            mtm,
            &container,
            i18n::text("about_open_repository"),
            sel!(openRepository),
            (PANEL_WIDTH - BUTTONS_WIDTH) / 2.0,
            24.0,
            OPEN_BUTTON_WIDTH,
            Some("\r"),
            target,
        );
        add_button(
            mtm,
            &container,
            i18n::text("close"),
            sel!(closePanel),
            (PANEL_WIDTH - BUTTONS_WIDTH) / 2.0 + OPEN_BUTTON_WIDTH + BUTTON_GAP,
            24.0,
            CLOSE_BUTTON_WIDTH,
            Some("\u{1b}"),
            target,
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
        primary: bool,
    ) {
        let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
        label.setAlignment(NSTextAlignment::Center);
        if let Some(font) = font {
            label.setFont(Some(font));
        }
        if !primary {
            label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        }
        let width = container.frame().size.width;
        label.setFrame(NSRect::new(
            NSPoint::new(0.0, y),
            NSSize::new(width, height),
        ));
        container.addSubview(&label);
    }

    #[allow(clippy::too_many_arguments)]
    fn add_button(
        mtm: MainThreadMarker,
        container: &NSView,
        title: &str,
        action: Sel,
        x: f64,
        y: f64,
        width: f64,
        key_equivalent: Option<&str>,
        target: &AnyObject,
    ) {
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                Some(target),
                Some(action),
                mtm,
            )
        };
        if let Some(key) = key_equivalent {
            button.setKeyEquivalent(&NSString::from_str(key));
        }
        button.setFrame(NSRect::new(
            NSPoint::new(x, y),
            NSSize::new(width, BUTTON_HEIGHT),
        ));
        container.addSubview(&button);
    }

    pub fn show() {
        let mtm = MainThreadMarker::new().expect("about dialog must run on the main thread");
        let target = action_target();

        let window = unsafe { NSWindow::new(mtm) };
        window.setStyleMask(NSWindowStyleMask::Titled | NSWindowStyleMask::FullSizeContentView);
        window.setTitlebarAppearsTransparent(true);
        window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        window.setMovable(false);
        window.setContentSize(NSSize::new(PANEL_WIDTH, PANEL_HEIGHT));
        // The panel is owned by Rust for its whole lifetime; without this the
        // window would release itself when the Close button is pressed.
        unsafe { window.setReleasedWhenClosed(false) };

        let content = panel_content(mtm, &target);
        window.setContentView(Some(&content));
        window.center();

        let app = NSApplication::sharedApplication(mtm);
        app.runModalForWindow(&window);
        window.close();
    }
}
