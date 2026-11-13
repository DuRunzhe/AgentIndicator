//! Native "About" panel plus the self-update progress window.
//!
//! On macOS the About panel is a small custom `NSWindow` with a standard title
//! bar: app icon, name, version, a short product description, the project
//! home / issue tracker / releases links and the copyright are stacked with
//! even padding and horizontally centered. Below them sit the update button, a
//! small blue link that switches the update check to the alpha channel (kept
//! visually quiet so the stable update button keeps the user's attention) and,
//! at the very bottom, the close button. A plain window is used instead of
//! `NSAlert` so the inner padding and the button order can be fully controlled.
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
        NSForegroundColorAttributeName, NSImageScaling, NSImageView, NSProgressIndicator,
        NSProgressIndicatorStyle, NSStringDrawing, NSStringDrawingDeprecated,
        NSStringDrawingOptions, NSTextAlignment, NSTextField, NSView, NSWindow, NSWindowButton,
        NSWindowStyleMask, NSWorkspace,
    };
    use objc2_foundation::{
        MainThreadMarker, NSAttributedString, NSAttributedStringKey, NSDictionary, NSPoint, NSRect,
        NSSize, NSString, NSURL,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;

    const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
    /// Name shown in the panel; the crate name is lowercase and package-style.
    const APP_DISPLAY_NAME: &str = "AgentStatusIndicator";
    const VERSION: &str = env!("CARGO_PKG_VERSION");

    const PANEL_WIDTH: f64 = 380.0;
    const ICON_SIZE: f64 = 64.0;
    /// Row height for the 15pt name. Kept on the 4px grid so every following
    /// top edge stays on it too.
    const NAME_HEIGHT: f64 = 24.0;
    const VERSION_HEIGHT: f64 = 16.0;
    const COPYRIGHT_HEIGHT: f64 = 16.0;
    /// Button height for the update and close buttons, on the 4px grid.
    const BUTTON_HEIGHT: f64 = 28.0;
    /// Link row height, shared by the navigation links and the alpha entry.
    const LINK_HEIGHT: f64 = 16.0;
    /// One size for every link, so the alpha entry is not a fifth text size in
    /// a panel that only needs three (name, body, fine print).
    const LINK_FONT_SIZE: f64 = 12.0;
    const DESC_FONT_SIZE: f64 = 12.0;
    /// Width the product description wraps at.
    const DESC_WIDTH: f64 = 336.0;
    const UPDATE_BUTTON_WIDTH: f64 = 240.0;
    const CLOSE_BUTTON_WIDTH: f64 = 96.0;

    const UPDATE_WIDTH: f64 = 380.0;
    const UPDATE_HEIGHT: f64 = 150.0;

    /// Set by the panel's button and drained by the tray loop, which owns the
    /// actual check/install state machine.
    static UPDATE_REQUESTED: AtomicBool = AtomicBool::new(false);
    /// Set by the alpha link: the same state machine, but on the alpha channel.
    static ALPHA_REQUESTED: AtomicBool = AtomicBool::new(false);

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

            unsafe extern "C-unwind" fn open_repository(
                _this: &AnyObject,
                _cmd: Sel,
                _sender: &AnyObject,
            ) {
                open_external(REPOSITORY);
            }

            unsafe extern "C-unwind" fn open_issues(
                _this: &AnyObject,
                _cmd: Sel,
                _sender: &AnyObject,
            ) {
                open_external(&format!("{REPOSITORY}/issues"));
            }

            unsafe extern "C-unwind" fn open_releases(
                _this: &AnyObject,
                _cmd: Sel,
                _sender: &AnyObject,
            ) {
                open_external(&format!("{REPOSITORY}/releases"));
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
                    sel!(openRepository:),
                    open_repository as unsafe extern "C-unwind" fn(_, _, _),
                );
                builder.add_method(
                    sel!(openIssues:),
                    open_issues as unsafe extern "C-unwind" fn(_, _, _),
                );
                builder.add_method(
                    sel!(openReleases:),
                    open_releases as unsafe extern "C-unwind" fn(_, _, _),
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

    /// Hands `url` to whatever the system registered for it. The panel has no
    /// status line for a failure, and `NSWorkspace` does not block the dialog
    /// the way spawning `open` would.
    fn open_external(url: &str) {
        let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) else {
            return;
        };
        let _ = NSWorkspace::sharedWorkspace().openURL(&url);
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
            let window = panel_window(mtm, i18n::text("update_title"), UPDATE_WIDTH, UPDATE_HEIGHT);
            let container = NSView::new(mtm);
            container.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(UPDATE_WIDTH, UPDATE_HEIGHT),
            ));

            // The window title bar already carries the update title.
            let detail = format!("{} {}", i18n::text("update_to"), version);
            add_centered_label(mtm, &container, &detail, 112.0, 18.0, None, false);

            let bar = NSProgressIndicator::new(mtm);
            bar.setStyle(NSProgressIndicatorStyle::Bar);
            bar.setControlSize(NSControlSize::Small);
            bar.setIndeterminate(true);
            bar.setMinValue(0.0);
            bar.setMaxValue(1.0);
            // SAFETY: called on the main thread with a valid control.
            unsafe { bar.startAnimation(None) };
            bar.setFrame(NSRect::new(
                NSPoint::new((UPDATE_WIDTH - 300.0) / 2.0, 88.0),
                NSSize::new(300.0, 12.0),
            ));
            container.addSubview(&bar);

            let status = add_centered_label(
                mtm,
                &container,
                i18n::text("update_downloading"),
                60.0,
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
                14.0,
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
        let target = action_target();
        let parts = panel_content(mtm, &target);
        let size = parts.content.frame().size;
        let window = panel_window(mtm, i18n::menu("about"), size.width, size.height);
        window.setContentView(Some(&parts.content));
        window.center();
        activate(mtm);
        window.makeKeyAndOrderFront(None);

        let mut panel = AboutPanel {
            window,
            update_button: parts.update_button,
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

    /// A regular titled panel: the title bar is the system's, so the window can
    /// be dragged and closed the way any other window can.
    fn panel_window(
        mtm: MainThreadMarker,
        title: &str,
        width: f64,
        height: f64,
    ) -> Retained<NSWindow> {
        let window = unsafe { NSWindow::new(mtm) };
        window.setStyleMask(NSWindowStyleMask::Titled | NSWindowStyleMask::Closable);
        window.setTitle(&NSString::from_str(title));
        window.setContentSize(NSSize::new(width, height));
        // Only closing is allowed; the other two traffic lights would just sit
        // there disabled.
        for button in [
            NSWindowButton::MiniaturizeButton,
            NSWindowButton::ZoomButton,
        ] {
            if let Some(button) = window.standardWindowButton(button) {
                button.setHidden(true);
            }
        }
        // The panel is owned by Rust for its whole lifetime; without this the
        // window would release itself when it is closed from the title bar.
        unsafe { window.setReleasedWhenClosed(false) };
        window
    }

    /// The controls the tray loop relabels after a check.
    struct PanelParts {
        content: Retained<NSView>,
        update_button: Retained<NSButton>,
    }

    /// Vertical layout of the About panel measured from the top edge, so the
    /// stack simply grows when a language needs more description lines.
    #[derive(Debug)]
    struct Metrics {
        height: f64,
        icon_top: f64,
        name_top: f64,
        version_top: f64,
        desc_top: f64,
        desc_height: f64,
        links_top: f64,
        copyright_top: f64,
        update_top: f64,
        alpha_top: f64,
        close_top: f64,
    }

    fn metrics() -> Metrics {
        let desc_font = NSFont::systemFontOfSize(DESC_FONT_SIZE);
        // Rounded up to the grid so a language with a longer description does
        // not push every following gap off the 4px rhythm.
        let desc_height = (wrapped_height(i18n::text("about_desc"), &desc_font, DESC_WIDTH)
            .max(VERSION_HEIGHT)
            / 4.0)
            .ceil()
            * 4.0;
        // Every gap is a multiple of 4. The stack runs icon, name, version,
        // description, update actions, links, copyright, close: the copyright
        // closes the panel instead of sitting between the description and the
        // buttons.
        let icon_top = 24.0;
        let name_top = icon_top + ICON_SIZE + 8.0;
        let version_top = name_top + NAME_HEIGHT + 4.0;
        let desc_top = version_top + VERSION_HEIGHT + 12.0;
        let update_top = desc_top + desc_height + 20.0;
        let alpha_top = update_top + BUTTON_HEIGHT + 8.0;
        let links_top = alpha_top + LINK_HEIGHT + 16.0;
        let copyright_top = links_top + LINK_HEIGHT + 20.0;
        let close_top = copyright_top + COPYRIGHT_HEIGHT + 20.0;
        Metrics {
            height: close_top + BUTTON_HEIGHT + 24.0,
            icon_top,
            name_top,
            version_top,
            desc_top,
            desc_height,
            links_top,
            copyright_top,
            update_top,
            alpha_top,
            close_top,
        }
    }

    /// Builds the About panel's content view and returns the buttons whose
    /// titles change later.
    fn panel_content(mtm: MainThreadMarker, target: &AnyObject) -> PanelParts {
        let layout = metrics();
        let container = NSView::new(mtm);
        container.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(PANEL_WIDTH, layout.height),
        ));
        // Views are positioned from the bottom edge, the metrics from the top.
        let y = |top: f64, height: f64| layout.height - top - height;

        let icon = NSImageView::new(mtm);
        if let Some(image) = NSApplication::sharedApplication(mtm).applicationIconImage() {
            icon.setImage(Some(&image));
        }
        icon.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        icon.setFrame(NSRect::new(
            NSPoint::new(
                (PANEL_WIDTH - ICON_SIZE) / 2.0,
                y(layout.icon_top, ICON_SIZE),
            ),
            NSSize::new(ICON_SIZE, ICON_SIZE),
        ));
        container.addSubview(&icon);

        let name_font = NSFont::boldSystemFontOfSize(15.0);
        add_centered_label(
            mtm,
            &container,
            APP_DISPLAY_NAME,
            y(layout.name_top, NAME_HEIGHT),
            NAME_HEIGHT,
            Some(&name_font),
            true,
        );
        add_centered_label(
            mtm,
            &container,
            &format!("{} {VERSION}", i18n::text("about_version")),
            y(layout.version_top, VERSION_HEIGHT),
            VERSION_HEIGHT,
            None,
            false,
        );
        add_description(
            mtm,
            &container,
            y(layout.desc_top, layout.desc_height),
            layout.desc_height,
        );

        let copyright_font = NSFont::systemFontOfSize(10.0);
        add_centered_label(
            mtm,
            &container,
            i18n::text("about_copyright"),
            y(layout.copyright_top, COPYRIGHT_HEIGHT),
            COPYRIGHT_HEIGHT,
            Some(&copyright_font),
            false,
        );

        let update_button = add_button(
            mtm,
            &container,
            i18n::text("update_checking"),
            sel!(requestUpdate:),
            (PANEL_WIDTH - UPDATE_BUTTON_WIDTH) / 2.0,
            y(layout.update_top, BUTTON_HEIGHT),
            UPDATE_BUTTON_WIDTH,
            BUTTON_HEIGHT,
            Some("\r"),
            target,
        );
        // A quiet way into the alpha channel. It has no key equivalent and
        // refuses first responder so Return still fires the stable update
        // button.
        add_link(
            mtm,
            &container,
            i18n::text("about_alpha"),
            sel!(requestAlpha:),
            0.0,
            y(layout.alpha_top, LINK_HEIGHT),
            PANEL_WIDTH,
            LINK_FONT_SIZE,
            target,
        );
        // Project home, issue tracker and releases, centered as one row just
        // above the close button.
        let links = [
            (i18n::text("about_link_home"), sel!(openRepository:)),
            (i18n::text("about_link_issues"), sel!(openIssues:)),
            (i18n::text("about_link_releases"), sel!(openReleases:)),
        ];
        let link_font = NSFont::systemFontOfSize(LINK_FONT_SIZE);
        let widths: Vec<f64> = links
            .iter()
            .map(|(text, _)| measure(&link_font, text) + 14.0)
            .collect();
        let gap = 12.0;
        let total = widths.iter().sum::<f64>() + gap * (links.len() - 1) as f64;
        let mut x = (PANEL_WIDTH - total) / 2.0;
        for ((text, action), width) in links.iter().zip(&widths) {
            add_link(
                mtm,
                &container,
                text,
                *action,
                x,
                y(layout.links_top, LINK_HEIGHT),
                *width,
                LINK_FONT_SIZE,
                target,
            );
            x += width + gap;
        }

        // Bottom of the panel: the close button, centered.
        add_button(
            mtm,
            &container,
            i18n::text("close"),
            sel!(closePanel:),
            (PANEL_WIDTH - CLOSE_BUTTON_WIDTH) / 2.0,
            y(layout.close_top, BUTTON_HEIGHT),
            CLOSE_BUTTON_WIDTH,
            BUTTON_HEIGHT,
            Some("\u{1b}"),
            target,
        );

        PanelParts {
            content: container,
            update_button,
        }
    }

    #[cfg(test)]
    mod layout_tests {
        use super::*;

        #[test]
        fn every_gap_sits_on_the_four_pixel_grid() {
            let layout = metrics();
            let tops = [
                layout.icon_top,
                layout.name_top,
                layout.version_top,
                layout.desc_top,
                layout.update_top,
                layout.alpha_top,
                layout.links_top,
                layout.copyright_top,
                layout.close_top,
                layout.height,
                layout.desc_height,
            ];
            for top in tops {
                assert_eq!(top % 4.0, 0.0, "{top} is off the 4px grid in {layout:?}");
            }
        }

        #[test]
        fn the_stack_runs_top_to_bottom_without_collisions() {
            let layout = metrics();
            let stack = [
                (layout.icon_top, ICON_SIZE),
                (layout.name_top, NAME_HEIGHT),
                (layout.version_top, VERSION_HEIGHT),
                (layout.desc_top, layout.desc_height),
                (layout.update_top, BUTTON_HEIGHT),
                (layout.alpha_top, LINK_HEIGHT),
                (layout.links_top, LINK_HEIGHT),
                (layout.copyright_top, COPYRIGHT_HEIGHT),
                (layout.close_top, BUTTON_HEIGHT),
            ];
            for pair in stack.windows(2) {
                let (top, height) = pair[0];
                let next = pair[1].0;
                assert!(
                    next - (top + height) >= 4.0,
                    "rows {top}..{} and {next} collide in {layout:?}",
                    top + height
                );
            }
            assert!(layout.height >= layout.close_top + BUTTON_HEIGHT);
        }

        #[test]
        fn the_copyright_closes_the_panel() {
            // The fine print belongs under the links, not between the
            // description and the buttons.
            let layout = metrics();
            assert!(layout.copyright_top > layout.links_top);
            assert!(layout.close_top > layout.copyright_top);
            assert!(layout.update_top < layout.links_top);
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

    /// Small tinted title for a text control that acts like a link. It keeps the
    /// system link colour and drops the underline, the way current macOS panels
    /// show link rows.
    fn link_title(text: &str, font_size: f64) -> Retained<NSAttributedString> {
        let font = NSFont::systemFontOfSize(font_size);
        let color = NSColor::linkColor();
        let attributes: Retained<NSDictionary<NSAttributedStringKey, AnyObject>> =
            NSDictionary::from_retained_objects(
                &[unsafe { NSFontAttributeName }, unsafe {
                    NSForegroundColorAttributeName
                }],
                &[font.into(), color.into()],
            );
        unsafe {
            NSAttributedString::initWithString_attributes(
                NSAttributedString::alloc(),
                &NSString::from_str(text),
                Some(&attributes),
            )
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add_link(
        mtm: MainThreadMarker,
        container: &NSView,
        text: &str,
        action: Sel,
        x: f64,
        y: f64,
        width: f64,
        font_size: f64,
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
        button.setAttributedTitle(&link_title(text, font_size));
        button.setAlignment(NSTextAlignment::Center);
        button.setRefusesFirstResponder(true);
        button.setFrame(NSRect::new(
            NSPoint::new(x, y),
            NSSize::new(width, LINK_HEIGHT),
        ));
        container.addSubview(&button);
        button
    }

    /// Height `text` needs when wrapped to `width`.
    fn wrapped_height(text: &str, font: &Retained<NSFont>, width: f64) -> f64 {
        let attributes: Retained<NSDictionary<NSAttributedStringKey, AnyObject>> =
            NSDictionary::from_retained_objects(
                &[unsafe { NSFontAttributeName }],
                &[font.clone().into()],
            );
        let bounds = unsafe {
            NSString::from_str(text).boundingRectWithSize_options_attributes(
                NSSize::new(width, 10_000.0),
                NSStringDrawingOptions::UsesLineFragmentOrigin,
                Some(&attributes),
            )
        };
        bounds.size.height.ceil()
    }

    /// Multi-line secondary label holding the product description.
    fn add_description(
        mtm: MainThreadMarker,
        container: &NSView,
        y: f64,
        height: f64,
    ) -> Retained<NSTextField> {
        let label = NSTextField::wrappingLabelWithString(
            &NSString::from_str(i18n::text("about_desc")),
            mtm,
        );
        label.setAlignment(NSTextAlignment::Center);
        label.setFont(Some(&NSFont::systemFontOfSize(DESC_FONT_SIZE)));
        label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        label.setFrame(NSRect::new(
            NSPoint::new((PANEL_WIDTH - DESC_WIDTH) / 2.0, y),
            NSSize::new(DESC_WIDTH, height),
        ));
        container.addSubview(&label);
        label
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
