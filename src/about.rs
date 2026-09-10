//! Native "About" panel plus the self-update progress window.
//!
//! On macOS the About panel is a small custom `NSWindow`: app icon, name,
//! version, repository and copyright are stacked with even padding and
//! horizontally centered, with an update and a close button underneath. A
//! plain window is used instead of `NSAlert` so the alert chrome at the top
//! can be dropped and the inner padding fully controlled.
//!
//! The panel is deliberately non-modal. It is owned by the tray event loop,
//! which keeps polling it: the update check runs on a worker thread and the
//! result arrives as a normal event, so neither the tray nor the dialog ever
//! blocks on the network. Other platforms have no dialog backend in this crate,
//! so `open` simply reports that nothing was shown.

use crate::update::{CheckOutcome, Progress};

#[cfg(target_os = "macos")]
pub use macos::{AboutPanel, UpdatePanel};

#[cfg(not(target_os = "macos"))]
pub struct AboutPanel;

#[cfg(not(target_os = "macos"))]
impl AboutPanel {
    pub fn is_visible(&self) -> bool {
        false
    }
    pub fn poll_check(&mut self) -> Option<CheckOutcome> {
        None
    }
    pub fn update_requested(&self) -> bool {
        false
    }
    pub fn start_check(&mut self) {}
    pub fn set_update_available(&self, _: &str) {}
    pub fn set_up_to_date(&self) {}
    pub fn set_check_failed(&self) {}
    pub fn close(&self) {}
}

#[cfg(not(target_os = "macos"))]
pub struct UpdatePanel;

#[cfg(not(target_os = "macos"))]
impl UpdatePanel {
    pub fn new(_: &str) -> Self {
        Self
    }
    pub fn set_progress(&self, _: &Progress) {}
    pub fn set_done(&self) {}
    pub fn set_error(&self, _: &str) {}
    pub fn is_visible(&self) -> bool {
        false
    }
    pub fn close(&self) {}
}

/// Shows the About panel and kicks off an update check. Returns `None` on
/// platforms without a native dialog.
pub fn open() -> Option<AboutPanel> {
    #[cfg(target_os = "macos")]
    {
        macos::open()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Human-readable status text for a progress stage.
#[cfg(target_os = "macos")]
fn stage_text(stage: crate::update::Stage) -> &'static str {
    match stage {
        crate::update::Stage::Downloading => crate::i18n::text("update_downloading"),
        crate::update::Stage::Verifying => crate::i18n::text("update_verifying"),
        crate::update::Stage::Extracting => crate::i18n::text("update_extracting"),
        crate::update::Stage::Installing => crate::i18n::text("update_installing"),
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{stage_text, CheckOutcome, Progress};
    use crate::{i18n, update};
    use crossbeam_channel::{Receiver, TryRecvError};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, NSObject, Sel};
    use objc2::{msg_send, sel, ClassType};
    use objc2_app_kit::{
        NSApplication, NSButton, NSColor, NSControlSize, NSFont, NSImageScaling, NSImageView,
        NSProgressIndicator, NSProgressIndicatorStyle, NSTextAlignment, NSTextField, NSView,
        NSWindow, NSWindowButton, NSWindowStyleMask, NSWindowTitleVisibility,
    };
    use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;

    const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
    const APP_NAME: &str = env!("CARGO_PKG_NAME");
    const VERSION: &str = env!("CARGO_PKG_VERSION");

    const PANEL_WIDTH: f64 = 360.0;
    const PANEL_HEIGHT: f64 = 264.0;
    const ICON_SIZE: f64 = 64.0;
    const BUTTON_HEIGHT: f64 = 26.0;
    const UPDATE_BUTTON_WIDTH: f64 = 178.0;
    const CLOSE_BUTTON_WIDTH: f64 = 96.0;
    const BUTTON_GAP: f64 = 12.0;
    const BUTTONS_WIDTH: f64 = UPDATE_BUTTON_WIDTH + BUTTON_GAP + CLOSE_BUTTON_WIDTH;

    const UPDATE_WIDTH: f64 = 380.0;
    const UPDATE_HEIGHT: f64 = 168.0;

    /// Set by the panel's button and drained by the tray loop, which owns the
    /// actual check/install state machine.
    static UPDATE_REQUESTED: AtomicBool = AtomicBool::new(false);

    /// Returns the shared action target backing both panels' buttons.
    fn action_target() -> Retained<AnyObject> {
        static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();
        let class = *CLASS.get_or_init(|| {
            let mut builder =
                ClassBuilder::new(c"AgentStatusIndicatorAboutTarget", NSObject::class())
                    .expect("about action target class registered twice");

            unsafe extern "C-unwind" fn request_update(
                _this: &AnyObject,
                _cmd: Sel,
                _sender: &AnyObject,
            ) {
                UPDATE_REQUESTED.store(true, Ordering::SeqCst);
            }

            unsafe extern "C-unwind" fn close_panel(
                _this: &AnyObject,
                _cmd: Sel,
                sender: &AnyObject,
            ) {
                // Close the window the clicked button lives in, which is more
                // reliable than trusting `keyWindow` when the app is inactive.
                let window: Option<Retained<NSWindow>> = unsafe { msg_send![sender, window] };
                if let Some(window) = window {
                    window.close();
                }
            }

            unsafe {
                builder.add_method(
                    sel!(requestUpdate:),
                    request_update as unsafe extern "C-unwind" fn(_, _, _),
                );
                builder.add_method(
                    sel!(closePanel:),
                    close_panel as unsafe extern "C-unwind" fn(_, _, _),
                );
            }
            builder.register()
        });

        unsafe { msg_send![class, new] }
    }

    pub struct AboutPanel {
        window: Retained<NSWindow>,
        update_button: Retained<NSButton>,
        check_rx: Option<Receiver<CheckOutcome>>,
        // NSControl targets are weak; keep the action target alive as long as
        // the buttons that point at it.
        _target: Retained<AnyObject>,
    }

    impl AboutPanel {
        pub fn is_visible(&self) -> bool {
            self.window.isVisible()
        }

        pub fn poll_check(&mut self) -> Option<CheckOutcome> {
            let receiver = self.check_rx.as_ref()?;
            match receiver.try_recv() {
                Ok(outcome) => {
                    self.check_rx = None;
                    Some(outcome)
                }
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    self.check_rx = None;
                    Some(CheckOutcome::Failed("the update check stopped".into()))
                }
            }
        }

        /// Drains the button click so the tray loop can act on it once.
        pub fn update_requested(&self) -> bool {
            UPDATE_REQUESTED.swap(false, Ordering::SeqCst)
        }

        pub fn start_check(&mut self) {
            self.set_button(i18n::text("update_checking"), false);
            self.check_rx = Some(update::spawn_check());
        }

        pub fn set_update_available(&self, version: &str) {
            self.set_button(&format!("{} {version}", i18n::text("update_to")), true);
        }

        pub fn set_up_to_date(&self) {
            self.set_button(i18n::text("update_latest"), false);
        }

        pub fn set_check_failed(&self) {
            self.set_button(i18n::text("update_check"), true);
        }

        pub fn close(&self) {
            self.window.close();
        }

        fn set_button(&self, title: &str, enabled: bool) {
            self.update_button.setTitle(&NSString::from_str(title));
            self.update_button.setEnabled(enabled);
        }
    }

    pub struct UpdatePanel {
        window: Retained<NSWindow>,
        bar: Retained<NSProgressIndicator>,
        status: Retained<NSTextField>,
        close_button: Retained<NSButton>,
        _target: Retained<AnyObject>,
    }

    impl UpdatePanel {
        pub fn new(version: &str) -> Self {
            let mtm = MainThreadMarker::new().expect("update dialog must run on the main thread");
            let target = action_target();
            let window = panel_window(mtm, UPDATE_WIDTH, UPDATE_HEIGHT);
            let container = NSView::new(mtm);
            container.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(UPDATE_WIDTH, UPDATE_HEIGHT),
            ));

            let title_font = NSFont::boldSystemFontOfSize(14.0);
            add_centered_label(
                mtm,
                &container,
                i18n::text("update_title"),
                128.0,
                20.0,
                Some(&title_font),
                true,
            );
            let detail = format!("{} {}", i18n::text("update_to"), version);
            add_centered_label(mtm, &container, &detail, 104.0, 18.0, None, false);

            let bar = NSProgressIndicator::new(mtm);
            bar.setStyle(NSProgressIndicatorStyle::Bar);
            bar.setControlSize(NSControlSize::Small);
            bar.setIndeterminate(true);
            bar.setMinValue(0.0);
            bar.setMaxValue(1.0);
            // SAFETY: called on the main thread with a valid control.
            unsafe { bar.startAnimation(None) };
            bar.setFrame(NSRect::new(
                NSPoint::new((UPDATE_WIDTH - 300.0) / 2.0, 72.0),
                NSSize::new(300.0, 12.0),
            ));
            container.addSubview(&bar);

            let status = add_centered_label(
                mtm,
                &container,
                i18n::text("update_downloading"),
                46.0,
                18.0,
                None,
                false,
            );

            let close_button = add_button(
                mtm,
                &container,
                i18n::text("close"),
                sel!(closePanel:),
                (UPDATE_WIDTH - 96.0) / 2.0,
                12.0,
                96.0,
                None,
                &target,
            );
            close_button.setHidden(true);

            window.setContentView(Some(&container));
            window.center();
            activate(mtm);
            window.makeKeyAndOrderFront(None);

            Self {
                window,
                bar,
                status,
                close_button,
                _target: target,
            }
        }

        pub fn set_progress(&self, progress: &Progress) {
            let mut text = stage_text(progress.stage).to_owned();
            let fraction = match progress.stage {
                update::Stage::Downloading => {
                    progress.total.filter(|total| *total > 0).map(|total| {
                        let fraction = progress.received as f64 / total as f64;
                        text = format!("{text} {}%", (fraction * 100.0).round() as u64);
                        fraction
                    })
                }
                _ => None,
            };
            self.status.setStringValue(&NSString::from_str(&text));
            match fraction {
                Some(fraction) => {
                    self.bar.setIndeterminate(false);
                    self.bar.setDoubleValue(fraction.clamp(0.0, 1.0));
                }
                None => {
                    self.bar.setIndeterminate(true);
                    // SAFETY: called on the main thread with a valid control.
                    unsafe { self.bar.startAnimation(None) };
                }
            }
        }

        pub fn set_done(&self) {
            self.status
                .setStringValue(&NSString::from_str(i18n::text("update_done")));
            self.bar.setIndeterminate(false);
            self.bar.setDoubleValue(1.0);
        }

        pub fn set_error(&self, message: &str) {
            let text = format!("{}: {message}", i18n::text("update_failed"));
            self.status.setStringValue(&NSString::from_str(&text));
            self.bar.setIndeterminate(false);
            self.close_button.setHidden(false);
        }

        pub fn is_visible(&self) -> bool {
            self.window.isVisible()
        }

        pub fn close(&self) {
            self.window.close();
        }
    }

    pub fn open() -> Option<AboutPanel> {
        let mtm = MainThreadMarker::new()?;
        UPDATE_REQUESTED.store(false, Ordering::SeqCst);
        let target = action_target();
        let window = panel_window(mtm, PANEL_WIDTH, PANEL_HEIGHT);
        let (content, update_button) = panel_content(mtm, &target);
        window.setContentView(Some(&content));
        window.center();
        activate(mtm);
        window.makeKeyAndOrderFront(None);

        let mut panel = AboutPanel {
            window,
            update_button,
            check_rx: None,
            _target: target,
        };
        panel.start_check();
        Some(panel)
    }

    #[allow(deprecated)]
    fn activate(mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        app.activateIgnoringOtherApps(true);
    }

    fn panel_window(mtm: MainThreadMarker, width: f64, height: f64) -> Retained<NSWindow> {
        let window = unsafe { NSWindow::new(mtm) };
        window.setStyleMask(NSWindowStyleMask::Titled | NSWindowStyleMask::FullSizeContentView);
        window.setTitlebarAppearsTransparent(true);
        window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        window.setMovable(false);
        window.setContentSize(NSSize::new(width, height));
        // The panel is owned by Rust for its whole lifetime; without this the
        // window would release itself when the Close button is pressed.
        unsafe { window.setReleasedWhenClosed(false) };
        for button in [
            NSWindowButton::CloseButton,
            NSWindowButton::MiniaturizeButton,
            NSWindowButton::ZoomButton,
        ] {
            if let Some(button) = window.standardWindowButton(button) {
                button.setHidden(true);
            }
        }
        window
    }

    /// Builds the About panel's content view and returns the update button so
    /// the tray loop can relabel it when the check finishes.
    fn panel_content(
        mtm: MainThreadMarker,
        target: &AnyObject,
    ) -> (Retained<NSView>, Retained<NSButton>) {
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

        let update_button = add_button(
            mtm,
            &container,
            i18n::text("update_checking"),
            sel!(requestUpdate:),
            (PANEL_WIDTH - BUTTONS_WIDTH) / 2.0,
            24.0,
            UPDATE_BUTTON_WIDTH,
            Some("\r"),
            target,
        );
        add_button(
            mtm,
            &container,
            i18n::text("close"),
            sel!(closePanel:),
            (PANEL_WIDTH - BUTTONS_WIDTH) / 2.0 + UPDATE_BUTTON_WIDTH + BUTTON_GAP,
            24.0,
            CLOSE_BUTTON_WIDTH,
            Some("\u{1b}"),
            target,
        );

        (container, update_button)
    }

    fn add_centered_label(
        mtm: MainThreadMarker,
        container: &NSView,
        text: &str,
        y: f64,
        height: f64,
        font: Option<&NSFont>,
        primary: bool,
    ) -> Retained<NSTextField> {
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
        label
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
    ) -> Retained<NSButton> {
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
        button
    }
}
