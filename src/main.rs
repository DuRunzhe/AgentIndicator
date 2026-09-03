mod browser_tabs;
mod claude_statusline;
mod config;
mod deepseek;
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
mod web;

use anyhow::Result;
use config::Config;
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
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
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
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
        println!("{}", serde_json::to_string_pretty(&detector.scan())?);
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
                Ok(WorkerCommand::Restart) => detector = Detector::new(),
                Ok(WorkerCommand::Refresh) | Err(_) => {}
            }
        }
    });
    let mut app = App::new(refresh_tx, latest_snapshot, debug_ui, instance_lock);
    event_loop.run_app(&mut app)?;
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
    config: Config,
    menu: Option<MenuView>,
    #[cfg(target_os = "macos")]
    macos_menu_symbols: macos_menu_symbols::SymbolCache,
    animation: Animation,
    last_updated: Option<std::time::SystemTime>,
    last_locale_check: Instant,
}

enum UserEvent {
    Scan,
}
enum WorkerCommand {
    Refresh,
    Restart,
}

enum ActionResult {
    BrowserTabs(bool),
    ClaudeStatusLine(Result<(), String>),
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
    notifications: IconMenuItem,
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
    ) -> Self {
        let config = Config::load();
        i18n::set_locale(&config.locale);
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
            config,
            menu: None,
            #[cfg(target_os = "macos")]
            macos_menu_symbols: macos_menu_symbols::SymbolCache::default(),
            animation: Animation::default(),
            last_updated: None,
            last_locale_check: Instant::now(),
        }
    }
    fn rebuild(&mut self) {
        let signature: Vec<_> = self
            .current
            .iter()
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
            return;
        }
        if let Some(menu) = &mut self.menu {
            for (item, instance) in menu.instances.iter().zip(&self.current) {
                item.set_text(format_instance(instance, &self.config));
                item.set_enabled(instance.state != AgentState::Stopped);
            }
            menu.notifications
                .set_text(notification_action_label(self.config.notifications_enabled));
            #[cfg(not(target_os = "macos"))]
            menu.notifications
                .set_icon(Some(toggle_menu_icon(self.config.notifications_enabled)));
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
    }

    fn create_menu(&self, signature: Vec<(u32, AgentState)>) -> MenuView {
        let menu = Menu::new();
        let mut instance_items = Vec::new();
        for instance in &self.current {
            let id = format!("focus:{}", instance.pid);
            let item = MenuItem::with_id(
                id,
                format_instance(instance, &self.config),
                instance.state != AgentState::Stopped,
                None,
            );
            let _ = menu.append(&item);
            instance_items.push(item);
        }
        if self.current.is_empty() {
            let item = MenuItem::new(i18n::text("no_agents"), false, None);
            let _ = menu.append(&item);
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
        for (key, label, enabled) in notification_preferences(&self.config) {
            let _ = notification_menu.append(&IconMenuItem::with_id(
                format!("notification_preference:{key}"),
                toggle_label(enabled, label),
                true,
                menu_toggle_icon(enabled),
                None,
            ));
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
        let claude_statusline = IconMenuItem::with_id(
            "install_claude_statusline",
            i18n::menu("install_claude"),
            true,
            menu_symbol_menu_icon([142, 142, 147]),
            None,
        );
        let _ = settings.append(&claude_statusline);
        let language_menu = Submenu::new(i18n::menu("language"), true);
        for value in ["auto", "zh-Hans", "zh-Hant", "en"] {
            let label = format!(
                "{} {}",
                if self.config.locale == value {
                    "✓"
                } else {
                    ""
                },
                i18n::language_name(value)
            );
            let _ = language_menu.append(&MenuItem::with_id(
                format!("locale:{value}"),
                label,
                true,
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
        let _ = menu.append(&MenuItem::with_id("quit", i18n::menu("quit"), true, None));
        MenuView {
            menu,
            signature,
            instances: instance_items,
            notifications: notification_item,
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

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, _: &ActiveEventLoop) {
        self.rebuild();
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.config.locale == "auto"
            && self.last_locale_check.elapsed() >= Duration::from_secs(60)
        {
            self.last_locale_check = Instant::now();
            if i18n::refresh_system_locale() {
                self.menu = None;
                self.rebuild();
            }
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
            if let ActionResult::ClaudeStatusLine(result) = result {
                match result {
                    Ok(()) => {
                        dialog::notice(i18n::text("claude_installed"), i18n::menu("install_claude"))
                    }
                    Err(error) => dialog::notice(&error, i18n::text("claude_install_failed")),
                }
            }
            self.action_busy = false;
            self.rebuild();
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            let id = event.id.0.as_str();
            if id == "quit" {
                event_loop.exit();
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
            } else if id == "install_claude_statusline" {
                if self.action_busy {
                    continue;
                }
                let choice = dialog::choose(
                    i18n::text("claude_install_prompt"),
                    i18n::menu("install_claude"),
                    &[i18n::text("cancel"), i18n::text("install")],
                    i18n::text("install"),
                    Some(i18n::text("cancel")),
                );
                if choice.as_deref() != Some(i18n::text("install")) {
                    continue;
                }
                self.action_busy = true;
                self.rebuild();
                let tx = self.action_tx.clone();
                thread::spawn(move || {
                    let _ = tx.send(ActionResult::ClaudeStatusLine(claude_statusline::install()));
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
            } else if let Some(key) = id.strip_prefix("display:") {
                toggle_config(&mut self.config, key);
                self.config.save();
                self.menu = None;
                self.rebuild();
            } else if let Some(locale) = id.strip_prefix("locale:") {
                self.config.locale = locale.into();
                self.config.save();
                i18n::set_locale(locale);
                self.menu = None;
                self.rebuild();
            } else if let Some(pid) = id
                .strip_prefix("focus:")
                .and_then(|v| v.parse::<u32>().ok())
            {
                let web_url = self
                    .current
                    .iter()
                    .find(|instance| instance.pid == pid)
                    .and_then(|instance| instance.open_url.clone());
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
        self.update_tray_status(false);
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            std::time::Instant::now() + Duration::from_millis(250),
        ));
    }
    fn user_event(&mut self, _: &ActiveEventLoop, event: UserEvent) {
        let UserEvent::Scan = event;
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
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

impl App {
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

fn toggle_label(enabled: bool, label: &str) -> String {
    if enabled {
        format!("✓ {label}")
    } else {
        label.into()
    }
}

fn display_settings() -> [(&'static str, &'static str); 5] {
    [
        ("duration", i18n::text("show_duration")),
        ("model", i18n::text("show_model")),
        ("context_percent", i18n::text("show_context_percent")),
        ("context_used", i18n::text("show_context_used")),
        ("context_total", i18n::text("show_context_total")),
    ]
}

fn notification_preferences(config: &Config) -> [(&'static str, &'static str, bool); 3] {
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
        "auto_confirm" => {
            config.show_waiting_notifications_in_auto_confirm_mode =
                !config.show_waiting_notifications_in_auto_confirm_mode
        }
        _ => {}
    }
}

fn config_value(config: &Config, key: &str) -> bool {
    match key {
        "duration" => config.show_duration,
        "model" => config.show_model,
        "context_percent" => config.show_context_percent,
        "context_used" => config.show_context_used,
        "context_total" => config.show_context_total,
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
    text
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
        .filter(|item| item.state != AgentState::Stopped)
        .map(|i| i.state)
        .max()
        .unwrap_or(AgentState::Stopped)
}
fn summary(items: &[AgentInstance]) -> String {
    let active: Vec<_> = items
        .iter()
        .filter(|item| item.state != AgentState::Stopped)
        .collect();
    if active.is_empty() {
        i18n::no_activity().into()
    } else {
        [
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
        AgentState::Ready | AgentState::Stopped => 0,
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
                AgentState::Ready | AgentState::Stopped => edge_alpha(outer_radius - distance),
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
