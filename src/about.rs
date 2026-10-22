//! Native "About" panel plus the self-update progress window.
//!
//! On macOS the About panel is a small custom `NSWindow`: app icon, name,
//! version, repository and copyright are stacked with even padding and
//! horizontally centered, with an update and a close button underneath. The
//! repository row carries a copy button for the URL, and the very bottom of the
//! panel holds a small blue link that switches the update check to the alpha
//! channel; it stays visually quiet so the stable update button keeps the
//! user's attention. A plain window is used instead of `NSAlert` so the alert
//! chrome at the top can be dropped and the inner padding fully controlled.
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
    pub fn alpha_requested(&self) -> bool {
        false
    }
    pub fn start_check(&mut self) {}
    pub fn start_alpha_check(&mut self) {}
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
    use objc2::{msg_send, sel, AnyThread, ClassType};
    use objc2_app_kit::{
        NSApplication, NSButton, NSColor, NSControlSize, NSFont, NSFontAttributeName,
        NSForegroundColorAttributeName, NSImageScaling, NSImageView, NSPasteboard,
        NSPasteboardTypeString, NSProgressIndicator, NSProgressIndicatorStyle, NSStringDrawing,
        NSTextAlignment, NSTextField, NSUnderlineStyleAttributeName, NSView, NSWindow,
        NSWindowButton, NSWindowStyleMask, NSWindowTitleVisibility,
    };
    use objc2_foundation::{
        MainThreadMarker, NSAttributedString, NSAttributedStringKey, NSDictionary, NSNumber,
        NSPoint, NSRect, NSSize, NSString,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
    const APP_NAME: &str = env!("CARGO_PKG_NAME");
    const VERSION: &str = env!("CARGO_PKG_VERSION");

    const PANEL_WIDTH: f64 = 360.0;
    const PANEL_HEIGHT: f64 = 264.0;
    const ICON_SIZE: f64 = 64.0;
    const BUTTON_HEIGHT: f64 = 26.0;
    const BUTTONS_Y: f64 = 30.0;
    /// Small link at the very bottom of the panel; a smaller font and a shorter
    /// frame keep it below the update button in the visual hierarchy.
    const LINK_Y: f64 = 8.0;
    const LINK_HEIGHT: f64 = 16.0;
    const LINK_FONT_SIZE: f64 = 10.0;
    const COPY_GAP: f64 = 8.0;
    const COPY_HEIGHT: f64 = 18.0;
    /// How long the copy button keeps its "copied" label.
    const COPIED_LABEL: Duration = Duration::from_millis(1200);
    const UPDATE_BUTTON_WIDTH: f64 = 178.0;
    const CLOSE_BUTTON_WIDTH: f64 = 96.0;
    const BUTTON_GAP: f64 = 12.0;
    const BUTTONS_WIDTH: f64 = UPDATE_BUTTON_WIDTH + BUTTON_GAP + CLOSE_BUTTON_WIDTH;

    const UPDATE_WIDTH: f64 = 380.0;
    const UPDATE_HEIGHT: f64 = 168.0;

    /// Set by the panel's button and drained by the tray loop, which owns the
    /// actual check/install state machine.
    static UPDATE_REQUESTED: AtomicBool = AtomicBool::new(false);
    /// Set by the alpha link: the same state machine, but on the alpha channel.
    static ALPHA_REQUESTED: AtomicBool = AtomicBool::new(false);
    /// Set by the copy button so the panel can flash a confirmation label.
    static COPY_REQUESTED: AtomicBool = AtomicBool::new(false);

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

            unsafe extern "C-unwind" fn request_alpha(
                _this: &AnyObject,
                _cmd: Sel,
                _sender: &AnyObject,
            ) {
                ALPHA_REQUESTED.store(true, Ordering::SeqCst);
            }

            unsafe extern "C-unwind" fn copy_repository(
                _this: &AnyObject,
                _cmd: Sel,
                _sender: &AnyObject,
            ) {
                let pasteboard = NSPasteboard::generalPasteboard();
                pasteboard.clearContents();
                let copied = pasteboard
                    .setString_forType(&NSString::from_str(REPOSITORY), unsafe {
                        NSPasteboardTypeString
                    });
                if copied {
                    COPY_REQUESTED.store(true, Ordering::SeqCst);
                }
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
                    sel!(requestAlpha:),
                    request_alpha as unsafe extern "C-unwind" fn(_, _, _),
                );
                builder.add_method(
                    sel!(copyRepository:),
                    copy_repository as unsafe extern "C-unwind" fn(_, _, _),
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
        copy_button: Retained<NSButton>,
        /// When the copy button returns to its normal label, if it is showing
        /// the confirmation one.
        copied_until: Option<Instant>,
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
            self.refresh_copy_label();
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

        /// Drains a click on the alpha link the same way.
        pub fn alpha_requested(&self) -> bool {
            ALPHA_REQUESTED.swap(false, Ordering::SeqCst)
        }

        pub fn start_check(&mut self) {
            self.start_check_on(update::Channel::Stable);
        }

        pub fn start_alpha_check(&mut self) {
            self.start_check_on(update::Channel::Alpha);
        }

        fn start_check_on(&mut self, channel: update::Channel) {
            self.set_button(i18n::text("update_checking"), false);
            self.check_rx = Some(update::spawn_check(channel));
        }

        /// Flashes the copy button's confirmation label after a click and
        /// restores the normal one a moment later.
        fn refresh_copy_label(&mut self) {
            let label = if COPY_REQUESTED.swap(false, Ordering::SeqCst) {
                self.copied_until = Some(Instant::now() + COPIED_LABEL);
                i18n::text("about_copied")
            } else if self
                .copied_until
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                self.copied_until = None;
                i18n::text("about_copy")
            } else {
                return;
            };
            self.copy_button.setTitle(&NSString::from_str(label));
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
                BUTTON_HEIGHT,
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
        ALPHA_REQUESTED.store(false, Ordering::SeqCst);
        COPY_REQUESTED.store(false, Ordering::SeqCst);
        let target = action_target();
        let window = panel_window(mtm, PANEL_WIDTH, PANEL_HEIGHT);
        let parts = panel_content(mtm, &target);
        window.setContentView(Some(&parts.content));
        window.center();
        activate(mtm);
        window.makeKeyAndOrderFront(None);

        let mut panel = AboutPanel {
            window,
            update_button: parts.update_button,
            copy_button: parts.copy_button,
            copied_until: None,
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

    /// The controls the tray loop relabels after a check or a copy.
    struct PanelParts {
        content: Retained<NSView>,
        update_button: Retained<NSButton>,
        copy_button: Retained<NSButton>,
    }

    /// Builds the About panel's content view and returns the buttons whose
    /// titles change later.
    fn panel_content(mtm: MainThreadMarker, target: &AnyObject) -> PanelParts {
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
        // The repository URL and its copy button share one row, centered as a
        // group so the row still reads as the middle of the panel.
        let repo_font = NSFont::systemFontOfSize(12.0);
        let copy_title = i18n::text("about_copy");
        let copy_font = NSFont::systemFontOfSize(11.0);
        // Wide enough for the confirmation label as well, so the button does
        // not resize when it flashes after a click.
        let copy_width = measure(&copy_font, copy_title)
            .max(measure(&copy_font, i18n::text("about_copied")))
            + 18.0;
        let repo_width = measure(&repo_font, REPOSITORY) + 1.0;
        let row_left = (PANEL_WIDTH - (repo_width + COPY_GAP + copy_width)) / 2.0;
        let repo = add_centered_label(
            mtm,
            &container,
            REPOSITORY,
            92.0,
            16.0,
            Some(&repo_font),
            false,
        );
        repo.setFrame(NSRect::new(
            NSPoint::new(row_left, 92.0),
            NSSize::new(repo_width, 16.0),
        ));
        repo.setAlignment(NSTextAlignment::Left);

        let copy_button = add_button(
            mtm,
            &container,
            copy_title,
            sel!(copyRepository:),
            row_left + repo_width + COPY_GAP,
            91.0,
            copy_width,
            COPY_HEIGHT,
            None,
            target,
        );
        copy_button.setControlSize(NSControlSize::Mini);
        copy_button.setFont(Some(&copy_font));
        copy_button.setRefusesFirstResponder(true);

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
            BUTTONS_Y,
            UPDATE_BUTTON_WIDTH,
            BUTTON_HEIGHT,
            Some("\r"),
            target,
        );
        add_button(
            mtm,
            &container,
            i18n::text("close"),
            sel!(closePanel:),
            (PANEL_WIDTH - BUTTONS_WIDTH) / 2.0 + UPDATE_BUTTON_WIDTH + BUTTON_GAP,
            BUTTONS_Y,
            CLOSE_BUTTON_WIDTH,
            BUTTON_HEIGHT,
            Some("\u{1b}"),
            target,
        );
        // Last row of the panel: a quiet way into the alpha channel. It has no
        // key equivalent and refuses first responder so Return still fires the
        // stable update button.
        add_link(
            mtm,
            &container,
            i18n::text("about_alpha"),
            sel!(requestAlpha:),
            LINK_Y,
            target,
        );

        PanelParts {
            content: container,
            update_button,
            copy_button,
        }
    }

    /// Width `text` occupies in the font it will be drawn with.
    fn measure(font: &Retained<NSFont>, text: &str) -> f64 {
        let attributes: Retained<NSDictionary<NSAttributedStringKey, AnyObject>> =
            NSDictionary::from_retained_objects(
                &[unsafe { NSFontAttributeName }],
                &[font.clone().into()],
            );
        unsafe { NSString::from_str(text).sizeWithAttributes(Some(&attributes)) }.width
    }

    /// Small blue underlined title for a text control that acts like a link.
    fn link_title(text: &str) -> Retained<NSAttributedString> {
        let font = NSFont::systemFontOfSize(LINK_FONT_SIZE);
        let color = NSColor::linkColor();
        // NSUnderlineStyleSingle.
        let underline = NSNumber::numberWithInt(1);
        let attributes: Retained<NSDictionary<NSAttributedStringKey, AnyObject>> =
            NSDictionary::from_retained_objects(
                &[
                    unsafe { NSFontAttributeName },
                    unsafe { NSForegroundColorAttributeName },
                    unsafe { NSUnderlineStyleAttributeName },
                ],
                &[font.into(), color.into(), underline.into()],
            );
        unsafe {
            NSAttributedString::initWithString_attributes(
                NSAttributedString::alloc(),
                &NSString::from_str(text),
                Some(&attributes),
            )
        }
    }

    fn add_link(
        mtm: MainThreadMarker,
        container: &NSView,
        text: &str,
        action: Sel,
        y: f64,
        target: &AnyObject,
    ) -> Retained<NSButton> {
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(""),
                Some(target),
                Some(action),
                mtm,
            )
        };
        button.setBordered(false);
        button.setAttributedTitle(&link_title(text));
        button.setAlignment(NSTextAlignment::Center);
        button.setRefusesFirstResponder(true);
        button.setFrame(NSRect::new(
            NSPoint::new(0.0, y),
            NSSize::new(container.frame().size.width, LINK_HEIGHT),
        ));
        container.addSubview(&button);
        button
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
        height: f64,
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
        button.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(width, height)));
        container.addSubview(&button);
        button
    }
}
