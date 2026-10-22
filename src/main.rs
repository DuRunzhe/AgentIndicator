mod about;
mod browser_tabs;
mod claude_statusline;
mod codex_state;
mod config;
mod deepseek;
mod deepseek_desktop;
mod detector;
mod dialog;
mod focus;
mod i18n;
mod instance_lock;
#[cfg(target_os = "macos")]
mod macos_menu_symbols;
mod macos_process;
mod model;
mod native_notifications;
mod notification_settings;
mod notifications;
mod opencode;
mod pi;
mod session;
mod startup;
mod terminal;
mod update;
mod usage;
mod web;

use anyhow::Result;
use config::Config;
use crossbeam_channel::{bounded, unbounded, Receiver, Sender, TryRecvError};
use detector::Detector;
use model::{AgentInstance, AgentState};
use native_notifications::NotificationService;
use notifications::{NotificationAction, NotificationTracker};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use tray_icon::{
    menu::{
        Icon as MenuIcon, IconMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
    },
    Icon, TrayIcon, TrayIconBuilder,
};

use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::WindowId,
};

fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args().collect();
    if arguments
        .iter()
        .any(|argument| argument == "--claude-statusline")
    {
        claude_statusline::collect_from_stdin();
        return Ok(());
    }
    if arguments.iter().any(|argument| argument == "--diagnose") {
        let mut detector = Detector::new();
        let rows = detector.scan();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        // The `dsh` CLI drivers the scan found, so a row that navigates to the
        // wrong place can be explained.
        if arguments
            .iter()
            .any(|argument| argument == "--with-drivers")
        {
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&detector::drivers_json())?
            );
        }
        return Ok(());
    }
    // Why the DeepSeek Harness desktop application is or is not being reported:
    // the process it was recognized as, the profile it points at, and the
    // conversations read from that profile's projection cache.
    if arguments
        .iter()
        .any(|argument| argument == "--diagnose-deepseek-desktop")
    {
        println!(
            "{}",
            serde_json::to_string_pretty(&detector::diagnose_deepseek_desktop())?
        );
        return Ok(());
    }
    // The token table's own numbers, for checking them against what each agent
    // reports for itself: `--diagnose-usage [--usage-period week|month]`.
    if arguments
        .iter()
        .any(|argument| argument == "--diagnose-usage")
    {
        let period = arguments
            .iter()
            .position(|argument| argument == "--usage-period")
            .and_then(|index| arguments.get(index + 1))
            .map(|value| usage::Period::from_key(value))
            .unwrap_or(usage::Period::Day);
        let snapshot = usage::scan(period);
        if arguments.iter().any(|argument| argument == "--render") {
            // The menu rows exactly as the tray would show them: the selector's
            // title followed by its three choices.
            println!("{}", usage_period_title_text(snapshot.period));
            for line in usage_period_lines(snapshot.period) {
                println!("{line}");
            }
            for line in usage_lines(&snapshot) {
                println!("{line}");
            }
        } else {
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
        }
        return Ok(());
    }
    if arguments
        .iter()
        .any(|argument| argument == "--check-update")
    {
        match update::check() {
            update::CheckOutcome::UpToDate => {
                println!("up to date ({})", update::CURRENT_VERSION)
            }
            update::CheckOutcome::Available(version) => println!("update available: {version}"),
            update::CheckOutcome::Failed(error) => println!("check failed: {error}"),
        }
        return Ok(());
    }
    let debug_ui = arguments.iter().any(|argument| argument == "--debug-ui");
    let instance_lock = match instance_lock::acquire()? {
        Some(lock) => lock,
        None => {
            eprintln!("AgentStatusIndicator is already running.");
            return Ok(());
        }
    };
    // If Claude Code's statusline still points at a collector installed by an
    // earlier channel, re-point it at this executable so a fresh npm/Homebrew/
    // curl install takes over without a manual menu action.
    claude_statusline::auto_repoint_if_stale();
    let (refresh_tx, refresh_rx) = bounded(1);
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let event_proxy = event_loop.create_proxy();
    let latest_snapshot = Arc::new(Mutex::new(None));
    let worker_snapshot = Arc::clone(&latest_snapshot);
    thread::spawn(move || {
        let mut detector = Detector::new();
        loop {
            // Keep just the newest state while AppKit is handling a menu action.
            // The UI never replays stale snapshots after it becomes available.
            *worker_snapshot.lock().expect("snapshot lock") = Some(detector.scan());
            if event_proxy.send_event(UserEvent::Scan).is_err() {
                break;
            }
            // Match the reference monitor's full detection cadence. Tray animation
            // remains independent and updates on the UI event loop.
            match refresh_rx.recv_timeout(Duration::from_secs(2)) {
                // A restart rebuilds every analyzer and re-reads the config.
                Ok(WorkerCommand::Restart) => detector = Detector::new(),
                Ok(WorkerCommand::DeepSeekDesktopWindow(window)) => {
                    detector.set_deepseek_desktop_window(window);
                }
                Ok(WorkerCommand::ConversationWindow(window)) => {
                    detector.set_conversation_window(window);
                }
                Ok(WorkerCommand::Refresh) | Err(_) => {}
            }
        }
    });
    let mut app = App::new(
        refresh_tx,
        latest_snapshot,
        debug_ui,
        instance_lock,
        event_loop.create_proxy(),
    );
    event_loop.run_app(&mut app)?;
    if let Some(executable) = app.take_restart_target() {
        // Release the single-instance lock before the replacement starts.
        drop(app);
        update::restart(&executable);
    }
    Ok(())
}

struct App {
    _instance_lock: fs::File,
    refresh_tx: Sender<WorkerCommand>,
    latest_snapshot: Arc<Mutex<Option<Vec<AgentInstance>>>>,
    debug_ui: bool,
    action_tx: Sender<ActionResult>,
    action_rx: Receiver<ActionResult>,
    action_busy: bool,
    tray: Option<TrayIcon>,
    current: Vec<AgentInstance>,
    notifications: NotificationTracker,
    notification_service: NotificationService,
    notification_action_rx: Receiver<NotificationAction>,
    notification_permission_tx: Sender<bool>,
    notification_permission_rx: Receiver<bool>,
    pending_notification_test: bool,
    /// Recomputes the token table in the background; the menu only reads it.
    usage: usage::Worker,
    /// Which spinner frame the period row shows while a scan is running.
    loading_frame: usize,
    config: Config,
    menu: Option<MenuView>,
    #[cfg(target_os = "macos")]
    macos_menu_symbols: macos_menu_symbols::SymbolCache,
    animation: Animation,
    last_updated: Option<std::time::SystemTime>,
    last_locale_check: Instant,
    about: Option<about::AboutPanel>,
    update_panel: Option<about::UpdatePanel>,
    update_rx: Option<Receiver<update::Message>>,
    pending_update: Option<String>,
    restart_target: Option<PathBuf>,
}

enum UserEvent {
    Scan,
    /// A fresh token table is available in [`App::usage`].
    Usage,
}
enum WorkerCommand {
    Refresh,
    Restart,
    ConversationWindow(Option<Duration>),
    DeepSeekDesktopWindow(Option<Duration>),
}

enum ActionResult {
    BrowserTabs(bool),
    ClaudeStatusLine {
        /// The row showed "uninstall", so this run removed the collector.
        uninstall: bool,
        result: Result<(), String>,
    },
    Finished,
}

struct Animation {
    state: AgentState,
    frame: usize,
    changed_at: Instant,
}

impl Default for Animation {
    fn default() -> Self {
        Self {
            state: AgentState::Stopped,
            frame: 0,
            changed_at: Instant::now(),
        }
    }
}

struct MenuView {
    menu: Menu,
    signature: Vec<(u32, AgentState)>,
    instances: Vec<MenuItem>,
    /// The two per-agent resource rows (names, then CPU/memory). Empty when the
    /// summary is turned off.
    resource: Vec<MenuItem>,
    /// The day/week/month selector above the token table. It is a submenu like
    /// Settings: hovering opens the three choices, and its own title echoes the
    /// selected period. Absent when the table is off.
    usage_period: Option<Submenu>,
    /// The three rows inside [`Self::usage_period`], so their marks can follow
    /// the selection and the spinner.
    usage_period_rows: Vec<MenuItem>,
    /// The token table itself: column titles, one row per agent, and the total.
    /// Empty when the table is turned off.
    usage: Vec<MenuItem>,
    notifications: IconMenuItem,
    notification_preferences: Vec<IconMenuItem>,
    test_notification: IconMenuItem,
    startup: IconMenuItem,
    display: Vec<IconMenuItem>,
    browser_tabs: IconMenuItem,
    claude_statusline: IconMenuItem,
}

impl App {
    fn new(
        refresh_tx: Sender<WorkerCommand>,
        latest_snapshot: Arc<Mutex<Option<Vec<AgentInstance>>>>,
        debug_ui: bool,
        instance_lock: fs::File,
        usage_proxy: EventLoopProxy<UserEvent>,
    ) -> Self {
        let config = Config::load();
        i18n::set_locale(&config.locale);
        let usage = usage::Worker::spawn(config.usage_period(), move || {
            let _ = usage_proxy.send_event(UserEvent::Usage);
        });
        let (action_tx, action_rx) = unbounded();
        let (notification_action_tx, notification_action_rx) = unbounded();
        let (notification_permission_tx, notification_permission_rx) = unbounded();
        Self {
            _instance_lock: instance_lock,
            refresh_tx,
            latest_snapshot,
            debug_ui,
            action_tx,
            action_rx,
            action_busy: false,
            tray: None,
            current: vec![],
            notifications: NotificationTracker::default(),
            notification_service: NotificationService::new(notification_action_tx),
            notification_action_rx,
            notification_permission_tx,
            notification_permission_rx,
            pending_notification_test: false,
            usage,
            loading_frame: 0,
            config,
            menu: None,
            #[cfg(target_os = "macos")]
            macos_menu_symbols: macos_menu_symbols::SymbolCache::default(),
            animation: Animation::default(),
            last_updated: None,
            last_locale_check: Instant::now(),
            about: None,
            update_panel: None,
            update_rx: None,
            pending_update: None,
            restart_target: None,
        }
    }
    /// Draw the resource summary and the token table with tab stops, so their
    /// columns line up whatever the menu font does with a localized label.
    #[cfg(target_os = "macos")]
    fn apply_table_fonts(&self) {
        let Some(menu) = &self.menu else {
            return;
        };
        if let Some(anchor) = menu.resource.first() {
            let (names, values) = resource_rows(&self.current);
            macos_menu_symbols::apply_table(&menu.menu, anchor, &[names, values]);
        }
        if let Some(anchor) = menu.usage.first() {
            macos_menu_symbols::apply_table(
                &menu.menu,
                anchor,
                &usage_lines(&self.usage.snapshot()),
            );
        }
    }
    /// Text of the three period rows, in menu order.
    fn usage_period_texts(&self) -> Vec<String> {
        let selected = self.config.usage_period();
        // On macOS the check mark is the menu row's SF Symbol, the same one the
        // other switches use; elsewhere the row has to spell the mark out.
        usage::Period::ALL
            .iter()
            .map(|period| {
                #[cfg(target_os = "macos")]
                {
                    let _ = selected;
                    period.label().to_owned()
                }
                #[cfg(not(target_os = "macos"))]
                {
                    usage_period_row(*period, *period == selected, None)
                }
            })
            .collect()
    }

    /// The submenu's own title: the period it echoes, or the spinner while a
    /// scan runs.
    fn usage_period_title(&self) -> String {
        let period = self.config.usage_period();
        match self.usage.is_loading() {
            true => usage_period_loading_title(period, self.loading_frame),
            false => usage_period_title_text(period),
        }
    }

    /// Advance the spinner frame and repaint the period row in place. The menu
    /// stays untouched otherwise, so this is a single `set_text` per frame.
    fn step_usage_spinner(&mut self) {
        if !self.usage.is_loading() {
            self.loading_frame = 0;
            return;
        }
        self.loading_frame = self.loading_frame.wrapping_add(1);
        let title = self.usage_period_title();
        let texts = self.usage_period_texts();
        let Some(menu) = &self.menu else {
            return;
        };
        if let Some(period) = &menu.usage_period {
            period.set_text(title);
        }
        for (item, text) in menu.usage_period_rows.iter().zip(texts) {
            item.set_text(text);
        }
    }

    fn rebuild(&mut self) {
        let signature: Vec<_> = visible_instances(&self.current, &self.config)
            .map(|instance| (instance.pid, instance.state))
            .collect();
        let needs_native_menu = self
            .menu
            .as_ref()
            .is_none_or(|menu| menu.signature != signature);
        if needs_native_menu {
            let menu = self.create_menu(signature);
            #[cfg(target_os = "macos")]
            self.macos_menu_symbols.apply(&menu.menu, &self.config);
            if let Some(tray) = &self.tray {
                // A new root NSMenu avoids AppKit retaining titles from the prior
                // status-item menu after a resumed session changes state.
                tray.set_menu(Some(Box::new(menu.menu.clone())));
            } else {
                let title = summary(&self.current);
                self.tray = TrayIconBuilder::new()
                    .with_menu(Box::new(menu.menu.clone()))
                    .with_tooltip(&title)
                    .with_title(&title)
                    .with_icon(status_icon(summary_state(&self.current), 0))
                    .build()
                    .ok();
            }
            self.menu = Some(menu);
            self.update_tray_status(true);
            #[cfg(target_os = "macos")]
            self.apply_table_fonts();
            return;
        }
        // Built before the menu is borrowed: the labels depend on the worker's
        // loading flag, which needs an immutable borrow of the app.
        let usage_period_texts = self.usage_period_texts();
        let usage_period_title = self.usage_period_title();
        if let Some(menu) = &mut self.menu {
            for (item, instance) in menu
                .instances
                .iter()
                .zip(visible_instances(&self.current, &self.config))
            {
                item.set_text(format_instance(instance, &self.config));
                item.set_enabled(!instance.informational && instance.state != AgentState::Stopped);
            }
            if !menu.resource.is_empty() {
                let (names, values) = resource_rows(&self.current);
                for (item, text) in menu.resource.iter().zip([names, values]) {
                    item.set_text(text);
                }
            }
            if !menu.usage.is_empty() {
                let snapshot = self.usage.snapshot();
                for (item, text) in menu.usage.iter().zip(usage_lines(&snapshot)) {
                    item.set_text(text);
                }
                if let Some(period) = &menu.usage_period {
                    period.set_text(usage_period_title.clone());
                }
                for (item, text) in menu.usage_period_rows.iter().zip(&usage_period_texts) {
                    item.set_text(text.clone());
                }
            }
            menu.notifications
                .set_text(notification_action_label(self.config.notifications_enabled));
            #[cfg(not(target_os = "macos"))]
            menu.notifications
                .set_icon(Some(toggle_menu_icon(self.config.notifications_enabled)));
            for (item, (_, label, enabled)) in menu
                .notification_preferences
                .iter()
                .zip(notification_preferences(&self.config))
            {
                item.set_enabled(self.config.notifications_enabled);
                item.set_text(toggle_label(enabled, label));
                #[cfg(not(target_os = "macos"))]
                item.set_icon(Some(toggle_menu_icon(enabled)));
            }
            menu.browser_tabs
                .set_text(browser_tab_action_label(self.config.browser_tab_reuse));
            #[cfg(not(target_os = "macos"))]
            menu.browser_tabs
                .set_icon(Some(toggle_menu_icon(self.config.browser_tab_reuse)));
            menu.startup
                .set_text(startup_action_label(startup::is_enabled()));
            #[cfg(not(target_os = "macos"))]
            menu.startup
                .set_icon(Some(toggle_menu_icon(startup::is_enabled())));
            menu.notifications.set_enabled(!self.action_busy);
            menu.test_notification
                .set_enabled(self.config.notifications_enabled && !self.action_busy);
            menu.browser_tabs.set_enabled(!self.action_busy);
            // The Claude row toggles between install and uninstall, so its
            // label, icon, and macOS symbol depend on the live collector state.
            let claude_installed = claude_statusline::is_installed();
            menu.claude_statusline
                .set_text(claude_action_label(claude_installed));
            #[cfg(not(target_os = "macos"))]
            menu.claude_statusline
                .set_icon(Some(toggle_menu_icon(claude_installed)));
            menu.claude_statusline.set_enabled(!self.action_busy);
            menu.startup.set_enabled(!self.action_busy);
            for (item, (key, label)) in menu.display.iter().zip(display_settings()) {
                item.set_text(toggle_label(config_value(&self.config, key), label));
                #[cfg(not(target_os = "macos"))]
                item.set_icon(Some(toggle_menu_icon(config_value(&self.config, key))));
            }
            #[cfg(target_os = "macos")]
            self.macos_menu_symbols
                .apply_if_settings_changed(&menu.menu, &self.config);
            self.update_tray_status(false);
        }
        // Re-applied after the text changes: the tab stops belong to the rows as
        // they are drawn now.
        #[cfg(target_os = "macos")]
        self.apply_table_fonts();
    }

    fn create_menu(&self, signature: Vec<(u32, AgentState)>) -> MenuView {
        let menu = Menu::new();
        let mut instance_items = Vec::new();
        let visible: Vec<_> = visible_instances(&self.current, &self.config).collect();
        for instance in &visible {
            let id = format!("focus:{}", instance.key);
            let item = MenuItem::with_id(
                id,
                format_instance(instance, &self.config),
                !instance.informational && instance.state != AgentState::Stopped,
                None,
            );
            let _ = menu.append(&item);
            instance_items.push(item);
        }
        if visible.is_empty() {
            let item = MenuItem::new(i18n::text("no_agents"), false, None);
            let _ = menu.append(&item);
        }
        // A per-agent resource summary sits below the sessions and above the
        // Settings menu: the names on one row and the CPU/memory under each on
        // the next. Turned off it contributes no rows at all.
        let mut resource_items = Vec::new();
        if self.config.show_resources {
            // Separate the summary from the session list above it, so it reads
            // as its own block; the second separator below closes it off from
            // the Settings menu.
            let _ = menu.append(&PredefinedMenuItem::separator());
            let (names, values) = resource_rows(&self.current);
            for text in [names, values] {
                let item = MenuItem::new(text, false, None);
                let _ = menu.append(&item);
                resource_items.push(item);
            }
        }
        // The token table sits below the CPU/memory summary: a day/week/month
        // switch, a header row, one row per agent and a total. Its numbers come
        // from the agents' own session records, recomputed in the background.
        let mut usage_rows = Vec::new();
        let mut usage_period_items = Vec::new();
        let mut usage_period_menu = None;
        if self.config.show_usage {
            let _ = menu.append(&PredefinedMenuItem::separator());
            let snapshot = self.usage.snapshot();
            let period_menu = Submenu::new(self.usage_period_title(), true);
            for (period, text) in usage::Period::ALL.iter().zip(self.usage_period_texts()) {
                let item =
                    MenuItem::with_id(format!("usage_period:{}", period.key()), text, true, None);
                let _ = period_menu.append(&item);
                usage_period_items.push(item);
            }
            let _ = menu.append(&period_menu);
            usage_period_menu = Some(period_menu);
            for text in usage_lines(&snapshot) {
                let item = MenuItem::new(text, false, None);
                let _ = menu.append(&item);
                usage_rows.push(item);
            }
        }
        let _ = menu.append(&PredefinedMenuItem::separator());
        let settings = Submenu::new(i18n::menu("settings"), true);
        let startup_menu = Submenu::new(i18n::menu("startup"), startup::is_supported());
        let startup_item = IconMenuItem::with_id(
            "startup",
            startup_action_label(startup::is_enabled()),
            startup::is_supported(),
            menu_toggle_icon(startup::is_enabled()),
            None,
        );
        let _ = startup_menu.append(&startup_item);
        let _ = startup_menu.append(&MenuItem::with_id(
            "open_login_settings",
            i18n::menu("login"),
            cfg!(target_os = "macos"),
            None,
        ));
        let _ = settings.append(&startup_menu);
        let notification_menu = Submenu::new(i18n::menu("notifications"), true);
        let notification_item = IconMenuItem::with_id(
            "notifications",
            notification_action_label(self.config.notifications_enabled),
            true,
            menu_toggle_icon(self.config.notifications_enabled),
            None,
        );
        let _ = notification_menu.append(&notification_item);
        let mut notification_preference_items = Vec::new();
        for (key, label, enabled) in notification_preferences(&self.config) {
            // The reference menu only exposes notification options after the
            // master switch is on; mirror that by graying the type rows out.
            let item = IconMenuItem::with_id(
                format!("notification_preference:{key}"),
                toggle_label(enabled, label),
                self.config.notifications_enabled,
                menu_toggle_icon(enabled),
                None,
            );
            let _ = notification_menu.append(&item);
            notification_preference_items.push(item);
        }
        let test_notification = IconMenuItem::with_id(
            "test_notification",
            i18n::menu("test_notification"),
            self.config.notifications_enabled,
            menu_symbol_menu_icon([0, 122, 255]),
            None,
        );
        let _ = notification_menu.append(&test_notification);
        let _ = notification_menu.append(&MenuItem::with_id(
            "open_notification_settings",
            i18n::text("open_notifications"),
            cfg!(target_os = "macos"),
            None,
        ));
        let _ =
            notification_menu.append(&MenuItem::new(i18n::text("notification_app"), false, None));
        let _ = settings.append(&notification_menu);
        let browser_menu = Submenu::new(i18n::menu("browser"), cfg!(target_os = "macos"));
        let browser_tabs = IconMenuItem::with_id(
            "browser_tabs",
            browser_tab_action_label(self.config.browser_tab_reuse),
            cfg!(target_os = "macos"),
            menu_toggle_icon(self.config.browser_tab_reuse),
            None,
        );
        let _ = browser_menu.append(&browser_tabs);
        let _ = browser_menu.append(&MenuItem::with_id(
            "open_automation_settings",
            i18n::menu("automation"),
            cfg!(target_os = "macos"),
            None,
        ));
        let _ = browser_menu.append(&MenuItem::new(i18n::menu("permission"), false, None));
        let _ = settings.append(&browser_menu);
        let claude_installed = claude_statusline::is_installed();
        let claude_statusline = IconMenuItem::with_id(
            "claude_statusline",
            claude_action_label(claude_installed),
            true,
            menu_toggle_icon(claude_installed),
            None,
        );
        let _ = settings.append(&claude_statusline);
        // Two independent ranges: the hosted (ChatGPT) conversations and the
        // DeepSeek Harness desktop conversations have different lifetimes, so
        // one setting cannot serve both.
        let conversation_menu = Submenu::new(i18n::menu("chatgpt_window"), true);
        for (key, _) in config::CONVERSATION_WINDOWS {
            let _ = conversation_menu.append(&IconMenuItem::with_id(
                format!("conversation_window:{key}"),
                i18n::conversation_window_label(key),
                true,
                menu_toggle_icon(self.config.conversation_window == key),
                None,
            ));
        }
        let _ = settings.append(&conversation_menu);
        let deepseek_menu = Submenu::new(i18n::menu("deepseek_window"), true);
        for (key, _) in config::CONVERSATION_WINDOWS {
            let _ = deepseek_menu.append(&IconMenuItem::with_id(
                format!("deepseek_window:{key}"),
                i18n::conversation_window_label(key),
                true,
                menu_toggle_icon(self.config.deepseek_desktop_window == key),
                None,
            ));
        }
        let _ = settings.append(&deepseek_menu);
        let language_menu = Submenu::new(i18n::menu("language"), true);
        for value in i18n::LANGUAGES {
            let selected = self.config.locale == value;
            let _ = language_menu.append(&IconMenuItem::with_id(
                format!("locale:{value}"),
                i18n::language_name(value),
                true,
                menu_toggle_icon(selected),
                None,
            ));
        }
        let _ = settings.append(&language_menu);
        let display_menu = Submenu::new(i18n::menu("display"), true);
        let mut display_items = Vec::new();
        for (key, label) in display_settings() {
            let item = IconMenuItem::with_id(
                format!("display:{key}"),
                toggle_label(config_value(&self.config, key), label),
                true,
                menu_toggle_icon(config_value(&self.config, key)),
                None,
            );
            let _ = display_menu.append(&item);
            display_items.push(item);
        }
        let _ = settings.append(&display_menu);
        let _ = menu.append(&settings);
        let _ = menu.append(&MenuItem::new(
            &last_updated_label(self.last_updated),
            false,
            None,
        ));
        let _ = menu.append(&MenuItem::with_id(
            "refresh",
            i18n::menu("refresh"),
            true,
            None,
        ));
        let _ = menu.append(&MenuItem::with_id(
            "restart_detector",
            i18n::text("restart_detector"),
            true,
            None,
        ));
        let _ = menu.append(&MenuItem::new(
            &format!(
                "{}: {}",
                i18n::text("about_version"),
                env!("CARGO_PKG_VERSION")
            ),
            false,
            None,
        ));
        let _ = menu.append(&MenuItem::with_id("about", i18n::menu("about"), true, None));
        let _ = menu.append(&MenuItem::with_id("quit", i18n::menu("quit"), true, None));
        MenuView {
            menu,
            signature,
            instances: instance_items,
            resource: resource_items,
            usage_period: usage_period_menu,
            usage_period_rows: usage_period_items,
            usage: usage_rows,
            notifications: notification_item,
            notification_preferences: notification_preference_items,
            test_notification,
            startup: startup_item,
            display: display_items,
            browser_tabs,
            claude_statusline,
        }
    }

    fn update_tray_status(&mut self, force: bool) {
        let Some(tray) = &self.tray else { return };
        let state = summary_state(&self.current);
        let now = Instant::now();
        let state_changed = self.animation.state != state;
        if state_changed {
            self.animation.state = state;
            self.animation.frame = 0;
            self.animation.changed_at = now;
        }
        let frame = animation_frame(state, now.duration_since(self.animation.changed_at));
        let frame_changed = self.animation.frame != frame;
        self.animation.frame = frame;
        let title = summary(&self.current);
        if force || state_changed {
            let _ = tray.set_tooltip(Some(&title));
            tray.set_title(Some(&title));
        }
        if force || state_changed || frame_changed {
            let _ = tray.set_icon(Some(status_icon(state, frame)));
        }
    }
}

fn notification_action_label(enabled: bool) -> &'static str {
    if enabled {
        i18n::text("disable_notifications")
    } else {
        i18n::text("enable_notifications")
    }
}

fn startup_action_label(enabled: bool) -> &'static str {
    if enabled {
        i18n::text("disable_startup")
    } else {
        i18n::text("enable_startup")
    }
}

fn browser_tab_action_label(enabled: bool) -> &'static str {
    if enabled {
        i18n::text("disable_browser_tabs")
    } else {
        i18n::text("enable_browser_tabs")
    }
}

/// The Claude collector row describes the state the user sees: installing when
/// absent, uninstalling when the collector already owns Claude's statusLine.
fn claude_action_label(installed: bool) -> &'static str {
    if installed {
        i18n::menu("uninstall_claude")
    } else {
        i18n::menu("install_claude")
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, _: &ActiveEventLoop) {
        self.rebuild();
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.last_locale_check.elapsed() >= Duration::from_secs(60) {
            self.last_locale_check = Instant::now();
            // The "follow system" row leads with the system language, so keep
            // that detection fresh even when an explicit language is selected.
            if i18n::refresh_system_locale() {
                self.menu = None;
                self.rebuild();
            }
        }
        if self.poll_dialogs() {
            event_loop.exit();
            return;
        }
        while let Ok(action) = self.notification_action_rx.try_recv() {
            thread::spawn(move || match action {
                NotificationAction::FocusPid(pid) => {
                    let _ = focus::focus(pid);
                }
                NotificationAction::FocusUrl { url, reuse_tabs } => {
                    let _ = web::focus_url(&url, reuse_tabs);
                }
            });
        }
        while let Ok(granted) = self.notification_permission_rx.try_recv() {
            self.config.notifications_enabled = granted;
            self.config.save();
            self.action_busy = false;
            if self.pending_notification_test && granted {
                self.send_test_notification();
            } else if self.pending_notification_test {
                dialog::notice(
                    i18n::text("notification_denied"),
                    i18n::text("notifications_disabled"),
                );
            }
            self.pending_notification_test = false;
            self.rebuild();
        }
        while let Ok(result) = self.action_rx.try_recv() {
            if let ActionResult::BrowserTabs(enabled) = result {
                self.config.browser_tab_reuse = enabled;
                self.config.save();
            }
            if let ActionResult::ClaudeStatusLine { uninstall, result } = result {
                match (uninstall, result) {
                    (true, Ok(())) => dialog::notice(
                        i18n::text("claude_uninstalled"),
                        i18n::menu("install_claude"),
                    ),
                    (false, Ok(())) => dialog::notice(
                        i18n::text("claude_installed"),
                        i18n::menu("uninstall_claude"),
                    ),
                    (true, Err(error)) => {
                        dialog::notice(&error, i18n::text("claude_uninstall_failed"))
                    }
                    (false, Err(error)) => {
                        dialog::notice(&error, i18n::text("claude_install_failed"))
                    }
                }
            }
            self.action_busy = false;
            self.rebuild();
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            let id = event.id.0.as_str();
            if id == "quit" {
                event_loop.exit();
            } else if id == "about" {
                self.open_about();
            } else if id == "notifications" {
                if self.action_busy {
                    continue;
                }
                self.action_busy = true;
                self.rebuild();
                if self.config.notifications_enabled {
                    self.config.notifications_enabled = false;
                    self.config.save();
                    self.action_busy = false;
                    self.rebuild();
                } else {
                    self.notification_service
                        .request_authorization(self.notification_permission_tx.clone());
                }
            } else if id == "test_notification" {
                self.action_busy = true;
                self.pending_notification_test = true;
                self.rebuild();
                self.notification_service
                    .request_authorization(self.notification_permission_tx.clone());
            } else if let Some(key) = id.strip_prefix("notification_preference:") {
                toggle_notification_preference(&mut self.config, key);
                self.config.save();
                self.menu = None;
                self.rebuild();
            } else if id == "startup" {
                if self.action_busy {
                    continue;
                }
                self.action_busy = true;
                self.rebuild();
                let tx = self.action_tx.clone();
                thread::spawn(move || {
                    startup::toggle();
                    let _ = tx.send(ActionResult::Finished);
                });
            } else if id == "browser_tabs" {
                if self.action_busy {
                    continue;
                }
                self.action_busy = true;
                self.rebuild();
                let current = self.config.browser_tab_reuse;
                let tx = self.action_tx.clone();
                thread::spawn(move || {
                    let enabled = browser_tabs::configure(current);
                    let _ = tx.send(ActionResult::BrowserTabs(enabled));
                });
            } else if id == "claude_statusline" {
                if self.action_busy {
                    continue;
                }
                // Read the state at click time so the dialog matches the row
                // the user just clicked, even if Claude settings changed.
                let uninstall = claude_statusline::is_installed();
                let title = claude_action_label(uninstall);
                let confirm = if uninstall {
                    i18n::text("uninstall")
                } else {
                    i18n::text("install")
                };
                let prompt = if uninstall {
                    i18n::text("claude_uninstall_prompt")
                } else {
                    i18n::text("claude_install_prompt")
                };
                let choice = dialog::choose(
                    prompt,
                    title,
                    &[i18n::text("cancel"), confirm],
                    confirm,
                    Some(i18n::text("cancel")),
                );
                if choice.as_deref() != Some(confirm) {
                    continue;
                }
                self.action_busy = true;
                self.rebuild();
                let tx = self.action_tx.clone();
                thread::spawn(move || {
                    let result = if uninstall {
                        claude_statusline::uninstall()
                    } else {
                        claude_statusline::install()
                    };
                    let _ = tx.send(ActionResult::ClaudeStatusLine { uninstall, result });
                });
            } else if id == "open_login_settings" {
                thread::spawn(startup::open_settings);
            } else if id == "open_notification_settings" {
                thread::spawn(notification_settings::open_settings);
            } else if id == "open_automation_settings" {
                thread::spawn(browser_tabs::open_automation_settings);
            } else if id == "refresh" {
                let _ = self.refresh_tx.try_send(WorkerCommand::Refresh);
            } else if id == "restart_detector" {
                let _ = self.refresh_tx.try_send(WorkerCommand::Restart);
            } else if let Some(key) = id.strip_prefix("usage_period:") {
                // Each row selects the period it names; the background worker
                // rescans at once instead of waiting for its next tick.
                let next = usage::Period::from_key(key);
                if next == self.config.usage_period() {
                    continue;
                }
                self.config.usage_period = next.key().into();
                self.config.save();
                self.usage.set_period(next);
                self.menu = None;
                self.rebuild();
            } else if let Some(key) = id.strip_prefix("display:") {
                toggle_config(&mut self.config, key);
                self.config.save();
                self.menu = None;
                self.rebuild();
            } else if let Some(key) = id.strip_prefix("conversation_window:") {
                self.config.conversation_window = key.into();
                self.config.save();
                let _ = self.refresh_tx.try_send(WorkerCommand::ConversationWindow(
                    self.config.conversation_window_duration(),
                ));
                self.menu = None;
                self.rebuild();
            } else if let Some(key) = id.strip_prefix("deepseek_window:") {
                self.config.deepseek_desktop_window = key.into();
                self.config.save();
                let _ = self
                    .refresh_tx
                    .try_send(WorkerCommand::DeepSeekDesktopWindow(
                        self.config.deepseek_desktop_window_duration(),
                    ));
                self.menu = None;
                self.rebuild();
            } else if let Some(locale) = id.strip_prefix("locale:") {
                self.config.locale = locale.into();
                self.config.save();
                i18n::set_locale(locale);
                self.menu = None;
                self.rebuild();
            } else if let Some(key) = id.strip_prefix("focus:") {
                // Rows are addressed by key, not pid: one process can host
                // several conversations, each with its own row.
                let target = self
                    .current
                    .iter()
                    .find(|instance| instance.key == key)
                    .map(|instance| (instance.open_url.clone(), instance.pid));
                if let Some((web_url, pid)) = target {
                    let reuse_tabs = self.config.browser_tab_reuse;
                    thread::spawn(move || {
                        if let Some(url) = web_url {
                            let _ = web::focus_url(&url, reuse_tabs);
                        } else {
                            let _ = focus::focus(pid);
                        }
                    });
                }
            }
        }
        self.update_tray_status(false);
        self.step_usage_spinner();
        let interval = if self.about.is_some() || self.update_rx.is_some() {
            80
        } else if self.usage.is_loading() {
            // Four frames per turn of the spinner; the scan itself is cached and
            // usually lands well inside one revolution.
            70
        } else {
            250
        };
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            std::time::Instant::now() + Duration::from_millis(interval),
        ));
    }
    fn user_event(&mut self, _: &ActiveEventLoop, event: UserEvent) {
        match event {
            // The table's rows already exist; only their text changed.
            UserEvent::Usage => self.rebuild(),
            UserEvent::Scan => self.apply_scan(),
        }
    }
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

impl App {
    /// Take the newest status snapshot, notify on what needs attention, and
    /// redraw the menu.
    fn apply_scan(&mut self) {
        let Some(latest) = self.latest_snapshot.lock().expect("snapshot lock").take() else {
            return;
        };
        for request in self.notifications.update(&latest, &self.config) {
            self.notification_service
                .send(request.with_browser_tab_reuse(self.config.browser_tab_reuse));
        }
        self.current = latest;
        self.last_updated = Some(std::time::SystemTime::now());
        self.rebuild();
        if self.debug_ui {
            write_ui_diagnosis(&self.current);
        }
    }
}

impl App {
    /// Opens the About panel and starts a fresh update check.
    fn open_about(&mut self) {
        // An install is already running; keep its progress window in front.
        if self.update_rx.is_some() {
            return;
        }
        if let Some(panel) = self.about.take() {
            panel.close();
        }
        // Dismiss a leftover "update failed" window from an earlier attempt.
        if let Some(panel) = self.update_panel.take() {
            panel.close();
        }
        self.pending_update = None;
        self.about = about::open();
    }

    /// Advances both dialogs. Returns `true` once an installed update is ready
    /// and the event loop should stop so the process can be replaced.
    fn poll_dialogs(&mut self) -> bool {
        self.poll_about();
        self.poll_update()
    }

    fn poll_about(&mut self) {
        let Some(panel) = self.about.as_mut() else {
            return;
        };
        if let Some(outcome) = panel.poll_check() {
            match outcome {
                update::CheckOutcome::Available(version) => {
                    panel.set_update_available(&version);
                    self.pending_update = Some(version);
                }
                update::CheckOutcome::UpToDate => panel.set_up_to_date(),
                update::CheckOutcome::Failed(_) => panel.set_check_failed(),
            }
        }
        let requested = panel.update_requested();
        let visible = panel.is_visible();
        if !visible {
            self.about = None;
        } else if requested {
            match self.pending_update.take() {
                Some(version) => {
                    panel.close();
                    self.about = None;
                    self.start_update(version);
                }
                // No update known yet: the click was a retry after a failure.
                None => panel.start_check(),
            }
        }
    }

    fn start_update(&mut self, version: String) {
        if let Some(panel) = self.update_panel.take() {
            panel.close();
        }
        self.update_panel = Some(about::UpdatePanel::new(&version));
        self.update_rx = Some(update::spawn_install(version));
    }

    fn poll_update(&mut self) -> bool {
        let Some(receiver) = self.update_rx.clone() else {
            self.dismiss_update_panel();
            return false;
        };
        let mut restart = false;
        loop {
            match receiver.try_recv() {
                Ok(update::Message::Progress(progress)) => {
                    if let Some(panel) = self.update_panel.as_ref() {
                        panel.set_progress(&progress);
                    }
                }
                Ok(update::Message::Finished(result)) => {
                    self.update_rx = None;
                    match result {
                        Ok(executable) => {
                            if let Some(panel) = self.update_panel.as_ref() {
                                panel.set_done();
                            }
                            self.restart_target = Some(executable);
                            restart = true;
                        }
                        Err(error) => {
                            if let Some(panel) = self.update_panel.as_ref() {
                                panel.set_error(&error);
                            }
                        }
                    }
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.update_rx = None;
                    break;
                }
            }
        }
        // A failed install keeps the panel around so the user can read the
        // error; drop it as soon as its Close button dismisses the window.
        self.dismiss_update_panel();
        restart
    }

    fn dismiss_update_panel(&mut self) {
        if self.update_rx.is_none()
            && self
                .update_panel
                .as_ref()
                .is_some_and(|panel| !panel.is_visible())
        {
            self.update_panel = None;
        }
    }

    fn take_restart_target(&mut self) -> Option<PathBuf> {
        self.restart_target.take()
    }

    fn send_test_notification(&mut self) {
        self.notification_service
            .send(notifications::NotificationRequest {
                title: i18n::text("test_notification_title").into(),
                body: i18n::text("test_notification_body").into(),
                action: NotificationAction::FocusPid(0),
            });
    }
}

fn last_updated_label(value: Option<std::time::SystemTime>) -> String {
    let seconds = value
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_secs() % 86_400);
    match seconds {
        Some(value) => format!(
            "{}: {:02}:{:02}",
            i18n::text("last_updated"),
            value / 3600,
            value % 3600 / 60
        ),
        None => format!("{}: --:--", i18n::text("last_updated")),
    }
}

fn write_ui_diagnosis(instances: &[AgentInstance]) {
    let Some(path) = dirs::home_dir().map(|home| home.join(".agent-status-indicator-ui.json"))
    else {
        return;
    };
    let value = serde_json::json!({
        "updated_at": format!("{:?}", std::time::SystemTime::now()),
        "summary": summary(instances),
        "instances": instances,
    });
    let temporary = PathBuf::from(format!("{}.tmp", path.display()));
    let Ok(file) = fs::File::create(&temporary) else {
        return;
    };
    if serde_json::to_writer(file, &value).is_ok() {
        let _ = fs::rename(temporary, path);
    }
}

fn toggle_label(_enabled: bool, label: &str) -> String {
    // Selection state is expressed solely by the row icon (filled vs outline);
    // keep the label itself plain so no checkmark text leaks into any language.
    label.into()
}

/// The Stopped placeholders occupy a tray row only while the matching display
/// setting is on; hiding them leaves just the running sessions in the menu.
fn visible_instances<'a>(
    instances: &'a [AgentInstance],
    config: &Config,
) -> impl Iterator<Item = &'a AgentInstance> {
    let show_stopped = config.show_stopped_agents;
    instances
        .iter()
        .filter(move |instance| instance.state != AgentState::Stopped || show_stopped)
}

fn display_settings() -> [(&'static str, &'static str); 10] {
    [
        ("duration", i18n::text("show_duration")),
        ("model", i18n::text("show_model")),
        ("context_percent", i18n::text("show_context_percent")),
        ("context_used", i18n::text("show_context_used")),
        ("context_total", i18n::text("show_context_total")),
        ("cpu", i18n::text("show_cpu")),
        ("memory", i18n::text("show_memory")),
        ("resources", i18n::text("show_resources")),
        ("usage", i18n::text("show_usage")),
        ("stopped_agents", i18n::text("show_stopped_agents")),
    ]
}

fn notification_preferences(config: &Config) -> [(&'static str, &'static str, bool); 4] {
    [
        (
            "waiting_confirmation",
            i18n::text("notify_waiting_confirmation"),
            config.notify_waiting_confirmation,
        ),
        (
            "waiting_reply",
            i18n::text("notify_waiting_reply"),
            config.notify_waiting_reply,
        ),
        ("error", i18n::text("notify_error"), config.notify_error),
        (
            "auto_confirm",
            i18n::text("notify_auto_confirm"),
            config.show_waiting_notifications_in_auto_confirm_mode,
        ),
    ]
}

fn toggle_notification_preference(config: &mut Config, key: &str) {
    match key {
        "waiting_confirmation" => {
            config.notify_waiting_confirmation = !config.notify_waiting_confirmation
        }
        "waiting_reply" => config.notify_waiting_reply = !config.notify_waiting_reply,
        "error" => config.notify_error = !config.notify_error,
        "auto_confirm" => {
            config.show_waiting_notifications_in_auto_confirm_mode =
                !config.show_waiting_notifications_in_auto_confirm_mode
        }
        _ => {}
    }
}

pub(crate) fn config_value(config: &Config, key: &str) -> bool {
    match key {
        "duration" => config.show_duration,
        "model" => config.show_model,
        "context_percent" => config.show_context_percent,
        "context_used" => config.show_context_used,
        "context_total" => config.show_context_total,
        "cpu" => config.show_cpu,
        "memory" => config.show_memory,
        "resources" => config.show_resources,
        "usage" => config.show_usage,
        "stopped_agents" => config.show_stopped_agents,
        _ => false,
    }
}

fn toggle_config(config: &mut Config, key: &str) {
    match key {
        "duration" => config.show_duration = !config.show_duration,
        "model" => config.show_model = !config.show_model,
        "context_percent" => config.show_context_percent = !config.show_context_percent,
        "context_used" => config.show_context_used = !config.show_context_used,
        "context_total" => config.show_context_total = !config.show_context_total,
        "cpu" => config.show_cpu = !config.show_cpu,
        "memory" => config.show_memory = !config.show_memory,
        "resources" => config.show_resources = !config.show_resources,
        "usage" => config.show_usage = !config.show_usage,
        "stopped_agents" => config.show_stopped_agents = !config.show_stopped_agents,
        _ => {}
    }
}

fn format_instance(instance: &AgentInstance, config: &Config) -> String {
    let mut text = format!(
        "{} {}: {}",
        instance.state.icon(),
        instance.label,
        instance.state.label()
    );
    if config.show_duration && instance.uptime.as_secs() >= 60 {
        let minutes = instance.uptime.as_secs() / 60;
        if minutes >= 60 {
            text.push_str(&format!(" ({}h{}m)", minutes / 60, minutes % 60));
        } else {
            text.push_str(&format!(" ({minutes}m)"));
        }
    }
    if config.show_model {
        if let Some(model) = &instance.model {
            text.push_str(&format!(" · {model}"));
        }
    }
    if let Some(ctx) = &instance.context {
        if config.show_context_percent {
            let percent = ctx.used_tokens as f64 * 100.0 / ctx.window_tokens.max(1) as f64;
            text.push_str(&format!(" · {percent:.1}%"));
        }
        match (config.show_context_used, config.show_context_total) {
            (true, true) => text.push_str(&format!(
                " ({}/{})",
                short_tokens(ctx.used_tokens),
                short_tokens(ctx.window_tokens)
            )),
            (true, false) => text.push_str(&format!(
                " · {} {}",
                i18n::text("context_used"),
                short_tokens(ctx.used_tokens)
            )),
            (false, true) => text.push_str(&format!(
                " · {} {}",
                i18n::text("context_total"),
                short_tokens(ctx.window_tokens)
            )),
            _ => {}
        }
    }
    if let Some(usage) = &instance.usage {
        // DeepSeek conversations run through one host process whose figures are
        // host-level, and any other GUI host shared by sibling sessions is the
        // same: leave them to the summary and never show them on a session row.
        if !usage.shared && !instance.kind.starts_with("DeepSeek") {
            if config.show_cpu {
                text.push_str(&format!(" · CPU {:.1}%", usage.cpu));
            }
            if config.show_memory {
                text.push_str(&format!(" · {}", short_bytes(usage.memory_bytes)));
            }
        }
    }
    text
}

/// Two menu rows summarizing per-agent resource usage. The first row carries
/// the column titles and the second the CPU/memory under each, led by a
/// `CPU/memory` stub so the numbers are self-describing and closed by the
/// total. The cells are handed to [`table_text`], which lines the columns up. An
/// agent with no live process reads `--/--` instead of `0`. Host-level figures
/// are summarized here only; a session row never renders them.
fn resource_rows(instances: &[AgentInstance]) -> (String, String) {
    let totals = model::resource_totals(instances);
    // The leading stub names the value row; its header stays blank so the total
    // and every agent line up with the figures below them.
    let mut labels: Vec<String> = vec![String::new()];
    labels.extend(
        model::RESOURCE_AGENTS
            .iter()
            .map(|agent| (*agent).to_owned()),
    );
    // The total closes the row, so it reads as the sum of the columns before it.
    labels.push(i18n::text("resource_total").to_owned());
    let total_cpu: f32 = totals.iter().map(|total| total.cpu).sum();
    let total_memory = totals
        .iter()
        .fold(0u64, |sum, total| sum.saturating_add(total.memory_bytes));
    let mut values: Vec<String> = vec![i18n::text("resource_stub").to_owned()];
    values.extend(
        totals
            .iter()
            .map(|total| usage_text(total.used, total.cpu, total.memory_bytes)),
    );
    values.push(usage_text(
        totals.iter().any(|total| total.used),
        total_cpu,
        total_memory,
    ));
    let mut lines = table_text(&[labels, values]);
    let values = lines.pop().unwrap_or_default();
    let names = lines.pop().unwrap_or_default();
    (names, values)
}

/// `CPU/memory` for one column, or `--/--` when the agent has no live process.
fn usage_text(used: bool, cpu: f32, memory_bytes: u64) -> String {
    if used {
        format!("{cpu:.1}%/{}", short_bytes(memory_bytes))
    } else {
        "--/--".to_owned()
    }
}

/// What a column reads when a source has nothing to report for the window. It
/// matches the resource summary's `--/--`, so an idle agent reads the same way
/// in both tables.
const USAGE_EMPTY: &str = "--/--";

/// The output column carries a count rather than a `total/hit` pair, so its
/// empty cell is a single dash.
const USAGE_EMPTY_OUTPUT: &str = "--";

/// The token table, one string per menu row: the agents across the columns
/// (closed by the total) and the three metrics down the rows. [`table_text`]
/// turns the cells into rows whose columns line up the way the CPU/memory
/// summary above already does.
fn usage_lines(snapshot: &usage::UsageSnapshot) -> Vec<String> {
    // The rows are the metrics: the first names the agents and the total, the
    // three below it are input, output and cost. Each agent appends one cell to
    // every row, so the agents run across the columns.
    let mut rows: Vec<Vec<String>> = vec![
        vec![String::new()],
        vec![i18n::text("usage_input").to_owned()],
        vec![i18n::text("usage_output").to_owned()],
        vec![i18n::text("usage_cost").to_owned()],
    ];
    for agent in &snapshot.agents {
        rows[0].push(agent.kind.to_owned());
        match agent.counts {
            Some(counts) => {
                rows[1].push(usage_pair(counts.input, counts.input_cached));
                rows[2].push(short_tokens(counts.output));
                rows[3].push(usage_cost(counts.cost));
            }
            None => {
                rows[1].push(USAGE_EMPTY.into());
                rows[2].push(USAGE_EMPTY_OUTPUT.into());
                rows[3].push(USAGE_EMPTY.into());
            }
        }
    }
    // The total closes the table, so it reads as the sum of the columns before it.
    let total = &snapshot.total;
    rows[0].push(i18n::text("resource_total").to_owned());
    if snapshot.has_data {
        rows[1].push(usage_pair(total.input, total.input_cached));
        rows[2].push(short_tokens(total.output));
        rows[3].push(usage_cost(total.cost));
    } else {
        rows[1].push(USAGE_EMPTY.into());
        rows[2].push(USAGE_EMPTY_OUTPUT.into());
        rows[3].push(USAGE_EMPTY.into());
    }
    table_text(&rows)
}

/// A `total/hit` cell: the column's full count and how much of it was served
/// from cache.
fn usage_pair(total: u64, cached: u64) -> String {
    format!("{}/{}", short_tokens(total), short_tokens(cached))
}

fn usage_cost(cost: Option<f64>) -> String {
    match cost {
        Some(value) => format!("${value:.2}"),
        None => USAGE_EMPTY.into(),
    }
}

/// The frames of the load spinner. ASCII only, because the menu font is
/// proportional and the 14 supported languages all render these four glyphs.
pub(crate) const USAGE_SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

/// Text of one period row: a radio-style mark and the period name. The selected
/// row carries a filled circle, or the spinner while a scan runs.
fn usage_period_row(candidate: usage::Period, selected: bool, spinner: Option<usize>) -> String {
    let mark = match spinner {
        Some(frame) if selected => USAGE_SPINNER[frame % USAGE_SPINNER.len()],
        _ if selected => "●",
        _ => "○",
    };
    format!("{mark} {}", candidate.label())
}

/// The submenu title at rest: the period it echoes.
pub(crate) fn usage_period_title_text(period: usage::Period) -> String {
    format!("{}:  {} ▾", i18n::text("usage_period"), period.label())
}

/// The submenu title while a scan runs, with the spinner frame in front of the
/// period name.
pub(crate) fn usage_period_loading_title(period: usage::Period, frame: usize) -> String {
    format!(
        "{}:  {} {} ▾",
        i18n::text("usage_period"),
        USAGE_SPINNER[frame % USAGE_SPINNER.len()],
        period.label()
    )
}

/// The three period rows as the menu shows them, with a filled circle on the
/// selected one.
fn usage_period_lines(period: usage::Period) -> Vec<String> {
    usage::Period::ALL
        .iter()
        .map(|candidate| usage_period_row(*candidate, *candidate == period, None))
        .collect()
}

/// Join table cells into menu rows. macOS draws these rows with tab stops (see
/// `macos_menu_symbols::apply_table`), so the cells travel tab-separated: the
/// menu font is proportional and its CJK glyphs are not a whole number of cells
/// wide, so padding a localized label with spaces can never line it up with the
/// numbers after it. Everywhere else the menu is monospaced, so the cells are
/// padded to fixed widths: the label column left-aligned and the rest right.
fn table_text(rows: &[Vec<String>]) -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        rows.iter().map(|row| row.join("\t")).collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let columns = rows.first().map(Vec::len).unwrap_or(0);
        let mut widths = vec![0; columns];
        for row in rows {
            for (index, cell) in row.iter().enumerate() {
                widths[index] = widths[index].max(display_width(cell));
            }
        }
        rows.iter()
            .map(|row| {
                let mut line = String::new();
                for (index, cell) in row.iter().enumerate() {
                    if index > 0 {
                        line.push_str("  ");
                    }
                    if index == 0 {
                        line.push_str(&pad_to(cell, widths[index]));
                    } else {
                        line.push_str(&pad_start(cell, widths[index]));
                    }
                }
                line.trim_end().to_owned()
            })
            .collect()
    }
}

/// The mirror of [`pad_to`] used by the numeric columns.
#[cfg(not(target_os = "macos"))]
fn pad_start(value: &str, width: usize) -> String {
    let mut text = " ".repeat(width.saturating_sub(display_width(value)));
    text.push_str(value);
    text
}

/// Pad `value` with spaces to `width` cells. Only the monospaced menus outside
/// macOS need this; the macOS rows carry tab stops instead.
#[cfg(not(target_os = "macos"))]
fn pad_to(value: &str, width: usize) -> String {
    let mut text = value.to_owned();
    text.push_str(&" ".repeat(width.saturating_sub(display_width(value))));
    text
}

/// Display width of `value` in monospace cells: East Asian wide characters take
/// two cells, everything else one. Only the monospaced menus outside macOS need
/// this; the macOS rows carry tab stops instead.
#[cfg(not(target_os = "macos"))]
fn display_width(value: &str) -> usize {
    value
        .chars()
        .map(|character| match character as u32 {
            0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x20000..=0x3FFFD => 2,
            _ => 1,
        })
        .sum()
}

fn short_bytes(value: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let value = value as f64;
    if value >= GIB {
        format!("{:.1}G", value / GIB)
    } else if value >= MIB {
        format!("{:.0}M", value / MIB)
    } else {
        format!("{:.0}K", value / KIB)
    }
}

fn short_tokens(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}m", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{}k", value / 1_000)
    } else {
        value.to_string()
    }
}

fn summary_state(items: &[AgentInstance]) -> AgentState {
    items
        .iter()
        .filter(|item| !item.informational && item.state != AgentState::Stopped)
        .map(|i| i.state)
        .max()
        .unwrap_or(AgentState::Stopped)
}
fn summary(items: &[AgentInstance]) -> String {
    let active: Vec<_> = items
        .iter()
        .filter(|item| !item.informational && item.state != AgentState::Stopped)
        .collect();
    if active.is_empty() {
        i18n::no_activity().into()
    } else {
        [
            AgentState::Error,
            AgentState::Waiting,
            AgentState::WaitingReply,
            AgentState::Working,
            AgentState::Ready,
        ]
        .into_iter()
        .filter_map(|state| {
            let count = active.iter().filter(|item| item.state == state).count();
            (count > 0).then(|| i18n::state_count(count, state.label()))
        })
        .collect::<Vec<_>>()
        .join(" · ")
    }
}
fn animation_frame(state: AgentState, elapsed: Duration) -> usize {
    match state {
        AgentState::Waiting | AgentState::WaitingReply => elapsed.as_secs() as usize % 2,
        AgentState::Working => (elapsed.as_secs() / 2) as usize % 2,
        AgentState::Ready | AgentState::Stopped | AgentState::Error => 0,
    }
}

fn status_icon(state: AgentState, frame: usize) -> Icon {
    // tray-icon displays a status item at 18pt.  Supplying a high-resolution,
    // antialiased source lets AppKit downsample it cleanly instead of enlarging
    // the original 16px diagnostic raster.
    const STATUS_ICON_PIXELS: usize = 128;
    Icon::from_rgba(
        status_icon_rgba_at_size(state, frame, STATUS_ICON_PIXELS),
        STATUS_ICON_PIXELS as u32,
        STATUS_ICON_PIXELS as u32,
    )
    .expect("valid icon")
}

fn status_icon_rgba_at_size(state: AgentState, frame: usize, size: usize) -> Vec<u8> {
    let (rgb, outer_radius) = match (state, frame) {
        (AgentState::Waiting | AgentState::WaitingReply, 0) => ([255, 176, 0], 6.0_f32),
        (AgentState::Waiting | AgentState::WaitingReply, _) => ([255, 214, 10], 7.0_f32),
        (AgentState::Working, 0) => ([0, 122, 255], 6.0_f32),
        (AgentState::Working, _) => ([100, 210, 255], 7.0_f32),
        (AgentState::Ready, _) => ([52, 199, 89], 6.0_f32),
        (AgentState::Stopped, _) => ([142, 142, 147], 6.0_f32),
        (AgentState::Error, _) => ([255, 59, 48], 6.0_f32),
    };
    let scale = size as f32 / 16.0;
    let center = size as f32 / 2.0;
    let outer_radius = outer_radius * scale;
    let mut rgba = vec![0; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let distance =
                ((x as f32 + 0.5 - center).powi(2) + (y as f32 + 0.5 - center).powi(2)).sqrt();
            let alpha = match state {
                AgentState::Waiting | AgentState::WaitingReply | AgentState::Working => {
                    let ring = edge_alpha(outer_radius - distance)
                        .min(edge_alpha(distance - (outer_radius - 2.0 * scale)));
                    let center_dot = edge_alpha(
                        (if outer_radius >= 7.0 * scale {
                            2.0
                        } else {
                            1.0
                        }) * scale
                            - distance,
                    );
                    ring.max(center_dot)
                }
                AgentState::Ready | AgentState::Stopped | AgentState::Error => {
                    edge_alpha(outer_radius - distance)
                }
            };
            if alpha > 0 {
                let pixel = (y * size + x) * 4;
                rgba[pixel..pixel + 3].copy_from_slice(&rgb);
                rgba[pixel + 3] = alpha;
            }
        }
    }
    rgba
}

fn edge_alpha(inside_distance: f32) -> u8 {
    ((inside_distance + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
fn status_icon_rgba(state: AgentState, frame: usize) -> Vec<u8> {
    let (rgb, outer_radius) = match (state, frame) {
        (AgentState::Waiting | AgentState::WaitingReply, 0) => ([255, 176, 0], 6_i32),
        (AgentState::Waiting | AgentState::WaitingReply, _) => ([255, 214, 10], 7_i32),
        (AgentState::Working, 0) => ([0, 122, 255], 6_i32),
        (AgentState::Working, _) => ([100, 210, 255], 7_i32),
        (AgentState::Ready, _) => ([52, 199, 89], 6_i32),
        (AgentState::Stopped, _) => ([142, 142, 147], 6_i32),
        (AgentState::Error, _) => ([255, 59, 48], 6_i32),
    };
    let mut rgba = vec![0; 16 * 16 * 4];
    for y in 0..16 {
        for x in 0..16 {
            let distance = (x as i32 - 8).pow(2) + (y as i32 - 8).pow(2);
            let active = matches!(
                state,
                AgentState::Waiting | AgentState::WaitingReply | AgentState::Working
            );
            let visible = if active {
                let inner_ring_radius = outer_radius - 2;
                let center_radius = if outer_radius >= 7 { 2_i32 } else { 1_i32 };
                (distance <= outer_radius.pow(2) && distance >= inner_ring_radius.pow(2))
                    || distance <= center_radius.pow(2)
            } else {
                distance <= outer_radius.pow(2)
            };
            if visible {
                let p = (y * 16 + x) * 4;
                rgba[p..p + 3].copy_from_slice(&rgb);
                rgba[p + 3] = 255;
            }
        }
    }
    rgba
}

/// Raster menu symbols keep the native menu self-contained on every supported
/// platform.  The reference implementation uses SF Symbols; `muda` does not
/// expose SF Symbols as cross-platform menu images, so these are deliberately
/// small equivalents rather than loading a macOS-only asset at runtime.
#[cfg(not(target_os = "macos"))]
fn toggle_menu_icon(enabled: bool) -> MenuIcon {
    let mut rgba = empty_menu_icon();
    let color = if enabled {
        [52, 199, 89]
    } else {
        [142, 142, 147]
    };
    let center = MENU_ICON_PIXELS as i32 / 2;
    let outer_radius = 7 * MENU_ICON_SCALE;
    for y in 0..MENU_ICON_PIXELS {
        for x in 0..MENU_ICON_PIXELS {
            let distance = ((x as f32 + 0.5 - center as f32).powi(2)
                + (y as f32 + 0.5 - center as f32).powi(2))
            .sqrt();
            let coverage = if enabled {
                edge_coverage(outer_radius as f32 - distance)
            } else {
                edge_coverage(outer_radius as f32 - distance).min(edge_coverage(
                    distance - (outer_radius - 2 * MENU_ICON_SCALE) as f32,
                ))
            };
            if coverage > 0 {
                set_menu_pixel_with_alpha(&mut rgba, x, y, color, coverage);
            }
        }
    }
    if enabled {
        // `checkmark.circle.fill`, matching the checked SF Symbol used by the
        // SwiftBar menu. It is rendered at 4× and downsampled by AppKit.
        draw_menu_line(&mut rgba, (4.0, 8.0), (7.0, 11.0), 1.15, [255, 255, 255]);
        draw_menu_line(&mut rgba, (7.0, 11.0), (12.0, 6.0), 1.15, [255, 255, 255]);
    }
    MenuIcon::from_rgba(rgba, MENU_ICON_PIXELS as u32, MENU_ICON_PIXELS as u32)
        .expect("valid menu toggle icon")
}

/// A compact colored information badge for one-off menu commands (test
/// notification and Claude context collection).  It is intentionally an icon
/// rather than a Unicode glyph so its color and alignment match toggle icons.
#[cfg(not(target_os = "macos"))]
fn menu_symbol_icon(rgb: [u8; 3]) -> MenuIcon {
    let mut rgba = empty_menu_icon();
    let center = MENU_ICON_PIXELS as i32 / 2;
    let radius = 7 * MENU_ICON_SCALE;
    for y in 0..MENU_ICON_PIXELS {
        for x in 0..MENU_ICON_PIXELS {
            let distance = ((x as f32 + 0.5 - center as f32).powi(2)
                + (y as f32 + 0.5 - center as f32).powi(2))
            .sqrt();
            let coverage = edge_coverage(radius as f32 - distance);
            if coverage > 0 {
                set_menu_pixel_with_alpha(&mut rgba, x, y, rgb, coverage);
            }
        }
    }
    // A white `i` makes the blue test-notification command discoverable while
    // still reading as a neutral app badge for the Claude installer.
    draw_menu_line(&mut rgba, (8.5, 6.0), (8.5, 11.5), 0.8, [255, 255, 255]);
    for y in 3 * MENU_ICON_SCALE..5 * MENU_ICON_SCALE {
        for x in 8 * MENU_ICON_SCALE..10 * MENU_ICON_SCALE {
            set_menu_pixel(&mut rgba, x as usize, y as usize, [255, 255, 255]);
        }
    }
    MenuIcon::from_rgba(rgba, MENU_ICON_PIXELS as u32, MENU_ICON_PIXELS as u32)
        .expect("valid menu symbol icon")
}

#[cfg(target_os = "macos")]
fn menu_toggle_icon(_: bool) -> Option<MenuIcon> {
    // macos_menu_symbols attaches the vector image after muda constructs the
    // NSMenu. Avoid allocating an intermediate bitmap on every refresh.
    None
}

#[cfg(not(target_os = "macos"))]
fn menu_toggle_icon(enabled: bool) -> Option<MenuIcon> {
    Some(toggle_menu_icon(enabled))
}

#[cfg(target_os = "macos")]
fn menu_symbol_menu_icon(_: [u8; 3]) -> Option<MenuIcon> {
    None
}

#[cfg(not(target_os = "macos"))]
fn menu_symbol_menu_icon(rgb: [u8; 3]) -> Option<MenuIcon> {
    Some(menu_symbol_icon(rgb))
}

// The native macOS menu renders images at 18pt.  Keep a 128px backing image
// (far beyond a 2× Retina representation) so AppKit can downsample smooth
// curves instead of magnifying a low-resolution bitmap.
#[cfg(not(target_os = "macos"))]
const MENU_ICON_PIXELS: usize = 128;
#[cfg(not(target_os = "macos"))]
const MENU_ICON_SCALE: i32 = 8;

#[cfg(not(target_os = "macos"))]
fn empty_menu_icon() -> Vec<u8> {
    vec![0; MENU_ICON_PIXELS * MENU_ICON_PIXELS * 4]
}

#[cfg(not(target_os = "macos"))]
fn set_menu_pixel(rgba: &mut [u8], x: usize, y: usize, rgb: [u8; 3]) {
    set_menu_pixel_with_alpha(rgba, x, y, rgb, 255);
}

#[cfg(not(target_os = "macos"))]
fn set_menu_pixel_with_alpha(rgba: &mut [u8], x: usize, y: usize, rgb: [u8; 3], alpha: u8) {
    if x >= MENU_ICON_PIXELS || y >= MENU_ICON_PIXELS {
        return;
    }
    let pixel = (y * MENU_ICON_PIXELS + x) * 4;
    let source_alpha = alpha as f32 / 255.0;
    let destination_alpha = rgba[pixel + 3] as f32 / 255.0;
    let output_alpha = source_alpha + destination_alpha * (1.0 - source_alpha);
    if output_alpha == 0.0 {
        return;
    }
    for channel in 0..3 {
        let source = rgb[channel] as f32;
        let destination = rgba[pixel + channel] as f32;
        rgba[pixel + channel] = ((source * source_alpha
            + destination * destination_alpha * (1.0 - source_alpha))
            / output_alpha)
            .round() as u8;
    }
    rgba[pixel + 3] = (output_alpha * 255.0).round() as u8;
}

#[cfg(not(target_os = "macos"))]
fn edge_coverage(inside_distance: f32) -> u8 {
    ((inside_distance + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(not(target_os = "macos"))]
fn draw_menu_line(rgba: &mut [u8], start: (f32, f32), end: (f32, f32), radius: f32, rgb: [u8; 3]) {
    let scale = MENU_ICON_SCALE as f32;
    let (x1, y1) = (start.0 * scale, start.1 * scale);
    let (x2, y2) = (end.0 * scale, end.1 * scale);
    let dx = x2 - x1;
    let dy = y2 - y1;
    let length_squared = dx * dx + dy * dy;
    let radius_squared = (radius * scale).powi(2);
    for y in 0..MENU_ICON_PIXELS {
        for x in 0..MENU_ICON_PIXELS {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let projection = (((px - x1) * dx + (py - y1) * dy) / length_squared).clamp(0.0, 1.0);
            let nearest_x = x1 + projection * dx;
            let nearest_y = y1 + projection * dy;
            let distance = ((px - nearest_x).powi(2) + (py - nearest_y).powi(2)).sqrt();
            let coverage = edge_coverage(radius_squared.sqrt() - distance);
            if coverage > 0 {
                set_menu_pixel_with_alpha(rgba, x, y, rgb, coverage);
            }
        }
    }
}

#[cfg(test)]
mod animation_tests {
    use super::*;

    #[test]
    fn waiting_changes_every_second() {
        assert_eq!(
            animation_frame(AgentState::Waiting, Duration::from_millis(999)),
            0
        );
        assert_eq!(
            animation_frame(AgentState::Waiting, Duration::from_secs(1)),
            1
        );
        assert_eq!(
            animation_frame(AgentState::WaitingReply, Duration::from_secs(2)),
            0
        );
    }

    #[test]
    fn working_changes_every_two_seconds() {
        assert_eq!(
            animation_frame(AgentState::Working, Duration::from_millis(1999)),
            0
        );
        assert_eq!(
            animation_frame(AgentState::Working, Duration::from_secs(2)),
            1
        );
        assert_eq!(
            animation_frame(AgentState::Working, Duration::from_secs(4)),
            0
        );
    }

    #[test]
    fn ready_and_stopped_are_static() {
        assert_eq!(
            animation_frame(AgentState::Ready, Duration::from_secs(99)),
            0
        );
        assert_eq!(
            animation_frame(AgentState::Stopped, Duration::from_secs(99)),
            0
        );
    }

    #[test]
    fn status_icon_has_ring_and_center_dot() {
        let rgba = status_icon_rgba(AgentState::Working, 0);
        let alpha = |x: usize, y: usize| rgba[(y * 16 + x) * 4 + 3];
        assert_eq!(alpha(8, 8), 255);
        assert_eq!(alpha(8, 11), 0);
        assert_eq!(alpha(8, 14), 255);
    }

    #[test]
    fn ready_icon_is_a_solid_green_circle() {
        let rgba = status_icon_rgba(AgentState::Ready, 0);
        let pixel = |x: usize, y: usize| &rgba[(y * 16 + x) * 4..(y * 16 + x + 1) * 4];
        assert_eq!(pixel(8, 8), &[52, 199, 89, 255]);
        assert_eq!(pixel(8, 11), &[52, 199, 89, 255]);
        assert_eq!(pixel(8, 15)[3], 0);
    }
}

#[cfg(test)]
mod summary_tests {
    use super::*;

    fn instance(state: AgentState) -> AgentInstance {
        AgentInstance {
            key: "1".into(),
            kind: "Test".into(),
            label: "Test".into(),
            pid: 1,
            cwd: None,
            state,
            uptime: Duration::ZERO,
            model: None,
            context: None,
            open_url: None,
            automatic_confirmation_mode: false,
            informational: false,
            usage: None,
        }
    }

    #[test]
    fn stopped_items_are_excluded_from_summary() {
        assert_eq!(summary(&[instance(AgentState::Stopped)]), "无活动");
        assert_eq!(
            summary_state(&[instance(AgentState::Stopped)]),
            AgentState::Stopped
        );
    }

    #[test]
    fn informational_rows_stay_out_of_the_summary() {
        let mut instance = AgentInstance {
            key: "desktop:host:more".into(),
            kind: "DeepSeek Harness".into(),
            label: "… 3 more sessions".into(),
            pid: 43958,
            cwd: None,
            state: AgentState::Ready,
            uptime: Duration::ZERO,
            model: None,
            context: None,
            open_url: None,
            automatic_confirmation_mode: false,
            informational: true,
            usage: None,
        };
        assert_eq!(summary_state(&[instance.clone()]), AgentState::Stopped);
        assert_eq!(summary(&[instance.clone()]), i18n::no_activity());
        // A real row beside it still drives the summary.
        instance.informational = false;
        instance.state = AgentState::Working;
        assert_eq!(summary_state(&[instance]), AgentState::Working);
    }

    #[test]
    fn informational_rows_are_never_notified() {
        let config = Config::default();
        let mut instance = AgentInstance {
            key: "desktop:host:more".into(),
            kind: "DeepSeek Harness".into(),
            label: "… 3 more sessions".into(),
            pid: 43958,
            cwd: None,
            state: AgentState::Waiting,
            uptime: Duration::ZERO,
            model: None,
            context: None,
            open_url: None,
            automatic_confirmation_mode: false,
            informational: true,
            usage: None,
        };
        assert!(!notifications::should_notify(&instance, &config));
        instance.informational = false;
        assert!(notifications::should_notify(&instance, &config));
    }

    #[test]
    fn summary_includes_each_live_state_in_priority_order() {
        assert_eq!(
            summary(&[
                instance(AgentState::Ready),
                instance(AgentState::WaitingReply),
                instance(AgentState::Working),
                instance(AgentState::Stopped),
            ]),
            "1个等待回复 · 1个进行中 · 1个就绪"
        );
    }
}

#[cfg(test)]
mod stopped_visibility_tests {
    use super::*;

    fn instance(state: AgentState) -> AgentInstance {
        AgentInstance {
            key: "1".into(),
            kind: "Test".into(),
            label: "Test".into(),
            pid: 1,
            cwd: None,
            state,
            uptime: Duration::ZERO,
            model: None,
            context: None,
            open_url: None,
            automatic_confirmation_mode: false,
            informational: false,
            usage: None,
        }
    }

    fn config(show_stopped: bool) -> Config {
        let mut config = Config::default();
        config.show_stopped_agents = show_stopped;
        config
    }

    #[test]
    fn stopped_rows_are_visible_by_default() {
        let instances = [instance(AgentState::Stopped), instance(AgentState::Working)];
        let visible: Vec<_> = visible_instances(&instances, &config(true)).collect();
        assert_eq!(visible.len(), 2);
    }

    #[test]
    fn stopped_rows_can_be_hidden() {
        let instances = [instance(AgentState::Stopped), instance(AgentState::Working)];
        let visible: Vec<_> = visible_instances(&instances, &config(false)).collect();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].state, AgentState::Working);
    }

    #[test]
    fn display_settings_include_stopped_agents_toggle() {
        let keys = display_settings();
        assert_eq!(keys.len(), 10);
        assert!(keys.iter().any(|(key, _)| *key == "usage"));
        assert!(keys.iter().any(|(key, _)| *key == "stopped_agents"));
        assert!(keys.iter().any(|(key, _)| *key == "cpu"));
        assert!(keys.iter().any(|(key, _)| *key == "memory"));
        assert!(keys.iter().any(|(key, _)| *key == "resources"));
        let hidden = Config {
            show_stopped_agents: false,
            ..Config::default()
        };
        assert!(!config_value(&hidden, "stopped_agents"));
    }
}

#[cfg(test)]
mod usage_table_tests {
    use super::*;
    use crate::usage::{TokenCounts, UsageSnapshot};

    fn counts(input: u64, cached: u64, output: u64, cost: Option<f64>) -> TokenCounts {
        TokenCounts {
            input,
            input_cached: cached,
            output,
            cost,
        }
    }

    fn snapshot(agents: Vec<Option<TokenCounts>>) -> UsageSnapshot {
        let mut snapshot = UsageSnapshot::empty(usage::Period::Day);
        for (agent, counts) in snapshot.agents.iter_mut().zip(agents) {
            agent.counts = counts;
        }
        snapshot.has_data = snapshot.agents.iter().any(|agent| agent.counts.is_some());
        if snapshot.has_data {
            let mut total = TokenCounts::default();
            for agent in &snapshot.agents {
                if let Some(counts) = agent.counts {
                    total.input += counts.input;
                    total.input_cached += counts.input_cached;
                    total.output += counts.output;
                    total.cost = Some(total.cost.unwrap_or(0.0) + counts.cost.unwrap_or(0.0));
                }
            }
            snapshot.total = total;
        }
        snapshot
    }

    fn fill() -> UsageSnapshot {
        snapshot(vec![
            Some(counts(22_600_000, 21_800_000, 313_000, None)),
            Some(counts(1_200_000, 900_000, 40_000, None)),
            None,
            None,
            Some(counts(8_900_000, 8_845_696, 66_652, Some(0.148))),
        ])
    }

    #[test]
    fn the_table_names_the_agents_across_the_top_and_the_metrics_down_the_side() {
        let lines = usage_lines(&fill());
        // A header row naming the agents and the total, then input, output, cost.
        assert_eq!(lines.len(), 4);
        assert!(
            lines[1].starts_with(i18n::text("usage_input")),
            "{}",
            lines[1]
        );
        assert!(
            lines[2].starts_with(i18n::text("usage_output")),
            "{}",
            lines[2]
        );
        assert!(
            lines[3].starts_with(i18n::text("usage_cost")),
            "{}",
            lines[3]
        );
        assert!(lines[0].trim_start().starts_with("Claude"), "{}", lines[0]);
        // The agent names match the CPU/memory summary above, and the total
        // closes the row.
        for kind in crate::model::RESOURCE_AGENTS {
            assert!(lines[0].contains(kind), "{}", lines[0]);
        }
        assert!(
            lines[0].trim_end().ends_with(i18n::text("resource_total")),
            "{}",
            lines[0]
        );
    }

    #[test]
    fn every_row_carries_the_same_columns() {
        let lines = usage_lines(&fill());
        // macOS rows carry tab-separated cells and get their tab stops at draw
        // time, so every row must have one cell per column.
        #[cfg(target_os = "macos")]
        {
            let columns = crate::model::RESOURCE_AGENTS.len() + 2;
            for line in &lines {
                assert_eq!(line.split('\t').count(), columns, "{line}");
            }
        }
        // Elsewhere the rows are space-padded for a monospaced menu, so they have
        // to end on the same cell as well.
        #[cfg(not(target_os = "macos"))]
        {
            let widest = lines.iter().map(|line| display_width(line)).max().unwrap();
            for line in &lines {
                assert_eq!(display_width(line), widest, "{line}");
            }
        }
    }

    #[test]
    fn a_pair_cell_carries_the_total_and_its_cache_hits() {
        let lines = usage_lines(&fill());
        assert!(lines[1].contains("22.6m/21.8m"), "{}", lines[1]);
        assert!(lines[1].contains("8.9m/8.8m"), "{}", lines[1]);
        // Output has no cache to hit, so the cell carries the count alone.
        assert!(lines[2].contains("313k"), "{}", lines[2]);
        assert!(!lines[2].contains("313k/"), "{}", lines[2]);
        assert!(lines[3].contains("$0.15"), "{}", lines[3]);
        // An agent without records in the window reads as no data, not zeros:
        // OpenCode and DeepSeek are empty in every metric row, and Claude and
        // Codex report no cost so the cost row has two more. The output cell is
        // a count, so its placeholder is a single dash.
        assert_eq!(lines[1].matches(USAGE_EMPTY).count(), 2, "{}", lines[1]);
        assert_eq!(
            lines[2].matches(USAGE_EMPTY_OUTPUT).count(),
            2,
            "{}",
            lines[2]
        );
        assert_eq!(lines[3].matches(USAGE_EMPTY).count(), 4, "{}", lines[3]);
    }

    #[test]
    fn the_total_column_sums_the_agents_that_reported() {
        let lines = usage_lines(&fill());
        let total = &fill().total;
        assert_eq!(total.input, 22_600_000 + 1_200_000 + 8_900_000);
        assert_eq!(total.input_cached, 21_800_000 + 900_000 + 8_845_696);
        assert!(lines[1].trim_end().ends_with("32.7m/31.5m"), "{}", lines[1]);
        assert!(lines[2].trim_end().ends_with("419k"), "{}", lines[2]);
        assert!(lines[3].trim_end().ends_with("$0.15"), "{}", lines[3]);
    }

    #[test]
    fn an_empty_table_reports_no_data_in_every_cell() {
        let lines = usage_lines(&snapshot(vec![None; 5]));
        assert_eq!(lines.len(), 4);
        for line in [&lines[1], &lines[3]] {
            assert_eq!(line.matches(USAGE_EMPTY).count(), 6, "{line}");
        }
        assert_eq!(
            lines[2].matches(USAGE_EMPTY_OUTPUT).count(),
            6,
            "{}",
            lines[2]
        );
        #[cfg(not(target_os = "macos"))]
        {
            let widest = lines.iter().map(|line| display_width(line)).max().unwrap();
            for line in &lines {
                assert_eq!(display_width(line), widest, "{line}");
            }
        }
    }

    #[test]
    fn the_loading_label_keeps_the_period_and_cycles_the_spinner() {
        for period in usage::Period::ALL {
            let first = usage_period_row(period, true, Some(0));
            assert!(first.contains(period.label()), "{first}");
            // The frames wrap, so an ever-increasing counter keeps spinning.
            assert_eq!(
                usage_period_row(period, true, Some(1)),
                usage_period_row(period, true, Some(1 + USAGE_SPINNER.len()))
            );
            assert_ne!(
                usage_period_row(period, true, Some(0)),
                usage_period_row(period, true, Some(1))
            );
        }
    }

    #[test]
    fn the_period_rows_mark_exactly_one_period() {
        for period in usage::Period::ALL {
            let lines = usage_period_lines(period);
            assert_eq!(lines.len(), usage::Period::ALL.len());
            let joined = lines.join(" ");
            assert_eq!(joined.matches('●').count(), 1, "{joined}");
            assert_eq!(
                joined.matches('○').count(),
                usage::Period::ALL.len() - 1,
                "{joined}"
            );
            // The marked row is the selected period's own row.
            assert!(
                lines
                    .iter()
                    .any(|line| line.starts_with('●') && line.contains(period.label())),
                "{joined}"
            );
        }
    }
}

#[cfg(test)]
mod usage_format_tests {
    use super::*;
    use crate::model::ProcessUsage;

    fn instance(usage: ProcessUsage) -> AgentInstance {
        AgentInstance {
            key: "1".into(),
            kind: "Pi".into(),
            label: "Pi (project)".into(),
            pid: 1,
            cwd: None,
            state: AgentState::Working,
            uptime: Duration::ZERO,
            model: None,
            context: None,
            open_url: None,
            automatic_confirmation_mode: false,
            informational: false,
            usage: Some(usage),
        }
    }

    #[test]
    fn cpu_and_memory_render_for_a_session_private_process() {
        let config = Config::default();
        let row = instance(ProcessUsage {
            cpu: 12.5,
            memory_bytes: 300 * 1024 * 1024,
            shared: false,
        });
        let text = format_instance(&row, &config);
        assert!(text.contains("CPU 12.5%"), "{text}");
        assert!(text.contains("300M"), "{text}");
    }

    #[test]
    fn a_shared_host_row_hides_its_usage_and_carries_no_marker() {
        let config = Config::default();
        let row = instance(ProcessUsage {
            cpu: 1.0,
            memory_bytes: 1024 * 1024,
            shared: true,
        });
        // Host-level figures stay in the summary, so the row shows no numbers
        // and needs no marker to qualify them.
        let text = format_instance(&row, &config);
        assert!(!text.contains("CPU"), "{text}");
        assert!(!text.contains("1M"), "{text}");
        let (names, _) = resource_rows(&[row]);
        assert!(!names.contains("宿主级"), "{names}");
    }

    #[test]
    fn deepseek_rows_show_no_usage_even_without_a_shared_host() {
        let config = Config::default();
        let mut row = instance(ProcessUsage {
            cpu: 2.0,
            memory_bytes: 8 * 1024 * 1024,
            shared: false,
        });
        row.kind = "DeepSeek Harness (terminal)".into();
        // DeepSeek figures are only meaningful in the summary, so even a
        // session-private row leaves them out.
        let text = format_instance(&row, &config);
        assert!(!text.contains("CPU"), "{text}");
        assert!(!text.contains("8M"), "{text}");
    }

    #[test]
    fn hidden_resource_rows_render_no_numbers() {
        let config = Config {
            show_cpu: false,
            show_memory: false,
            ..Config::default()
        };
        let row = instance(ProcessUsage {
            cpu: 9.0,
            memory_bytes: 5 * 1024 * 1024,
            shared: true,
        });
        let text = format_instance(&row, &config);
        assert!(!text.contains("CPU"), "{text}");
        assert!(!text.contains("5M"), "{text}");
    }

    #[test]
    fn resource_rows_place_every_agent_with_placeholders_for_idle_ones() {
        let (names, values) = resource_rows(&[]);
        // One column per agent, then the total last.
        assert!(names.contains(i18n::text("resource_total")), "{names}");
        assert!(
            names.trim_end().ends_with(i18n::text("resource_total")),
            "{names}"
        );
        assert!(names.contains("Claude"), "{names}");
        assert!(names.contains("DeepSeek"), "{names}");
        // The value row leads with the CPU/memory stub describing its figures.
        assert!(values.starts_with(i18n::text("resource_stub")), "{values}");
        assert_eq!(
            values.matches("--/--").count(),
            model::RESOURCE_AGENTS.len() + 1
        );
    }

    #[test]
    fn resource_rows_show_a_used_agents_cpu_and_memory() {
        let row = instance(ProcessUsage {
            cpu: 12.5,
            memory_bytes: 300 * 1024 * 1024,
            shared: false,
        });
        let (names, values) = resource_rows(&[row]);
        // The total equals the only used agent, and the idle agents stay blank.
        assert_eq!(values.matches("12.5%/300M").count(), 2, "{values}");
        assert_eq!(
            values.matches("--/--").count(),
            model::RESOURCE_AGENTS.len() - 1
        );
        assert!(names.contains(i18n::text("resource_total")), "{names}");
    }
}
