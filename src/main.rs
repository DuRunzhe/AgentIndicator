mod browser_tabs;
mod claude_statusline;
mod config;
mod deepseek;
mod detector;
mod dialog;
mod focus;
mod i18n;
mod instance_lock;
mod macos_process;
mod model;
mod native_notifications;
mod notification_settings;
mod notifications;
mod opencode;
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
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
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
    config: Config,
    menu: Option<MenuView>,
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
    notifications: MenuItem,
    startup: MenuItem,
    display: Vec<MenuItem>,
    browser_tabs: MenuItem,
    claude_statusline: MenuItem,
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
            config,
            menu: None,
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
            menu.browser_tabs
                .set_text(browser_tab_action_label(self.config.browser_tab_reuse));
            menu.startup
                .set_text(startup_action_label(startup::is_enabled()));
            menu.notifications.set_enabled(!self.action_busy);
            menu.browser_tabs.set_enabled(!self.action_busy);
            menu.claude_statusline.set_enabled(!self.action_busy);
            menu.startup.set_enabled(!self.action_busy);
            for (item, (key, label)) in menu.display.iter().zip(display_settings()) {
                item.set_text(toggle_label(config_value(&self.config, key), label));
            }
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
            let item = MenuItem::new("⚪ 未发现运行中的 Agent", false, None);
            let _ = menu.append(&item);
        }
        let _ = menu.append(&PredefinedMenuItem::separator());
        let settings = Submenu::new(i18n::menu("settings"), true);
        let startup_menu = Submenu::new(i18n::menu("startup"), startup::is_supported());
        let startup_item = MenuItem::with_id(
            "startup",
            startup_action_label(startup::is_enabled()),
            startup::is_supported(),
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
        let notification_item = MenuItem::with_id(
            "notifications",
            notification_action_label(self.config.notifications_enabled),
            true,
            None,
        );
        let _ = notification_menu.append(&notification_item);
        let _ = notification_menu.append(&MenuItem::with_id(
            "open_notification_settings",
            "打开系统通知设置",
            cfg!(target_os = "macos"),
            None,
        ));
        let _ = notification_menu.append(&MenuItem::new(
            "系统通知应用：AgentStatusIndicator",
            false,
            None,
        ));
        let _ = settings.append(&notification_menu);
        let browser_menu = Submenu::new(i18n::menu("browser"), cfg!(target_os = "macos"));
        let browser_tabs = MenuItem::with_id(
            "browser_tabs",
            browser_tab_action_label(self.config.browser_tab_reuse),
            cfg!(target_os = "macos"),
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
        let claude_statusline = MenuItem::with_id(
            "install_claude_statusline",
            i18n::menu("install_claude"),
            true,
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
            let item = MenuItem::with_id(
                format!("display:{key}"),
                toggle_label(config_value(&self.config, key), label),
                true,
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
            "重启检测器",
            true,
            None,
        ));
        let _ = menu.append(&MenuItem::with_id("quit", i18n::menu("quit"), true, None));
        MenuView {
            menu,
            signature,
            instances: instance_items,
            notifications: notification_item,
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
        "✓ 点击关闭通知"
    } else {
        "点击开启通知"
    }
}

fn startup_action_label(enabled: bool) -> &'static str {
    if enabled {
        "✓ 点击关闭开机自启"
    } else {
        "点击开启开机自启"
    }
}

fn browser_tab_action_label(enabled: bool) -> &'static str {
    if enabled {
        "✓ 点击关闭浏览器标签页复用"
    } else {
        "点击开启浏览器标签页复用"
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
            self.rebuild();
        }
        while let Ok(result) = self.action_rx.try_recv() {
            if let ActionResult::BrowserTabs(enabled) = result {
                self.config.browser_tab_reuse = enabled;
                self.config.save();
            }
            if let ActionResult::ClaudeStatusLine(result) = result {
                match result {
                    Ok(()) => dialog::notice(
                        "Claude 上下文采集已安装。请在 Claude 中开始或继续一次会话以写入数据。",
                        "Claude 上下文采集",
                    ),
                    Err(error) => dialog::notice(&error, "Claude 上下文采集未安装"),
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
                    "将为 Claude Code 配置上下文采集，并保留、转发你当前的 statusLine 命令。",
                    "安装 Claude 上下文采集",
                    &["取消", "安装"],
                    "安装",
                    Some("取消"),
                );
                if choice.as_deref() != Some("安装") {
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
        for request in self
            .notifications
            .update(&latest, self.config.notifications_enabled)
        {
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

fn last_updated_label(value: Option<std::time::SystemTime>) -> String {
    let seconds = value
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_secs() % 86_400);
    match seconds {
        Some(value) => format!("上次刷新: {:02}:{:02}", value / 3600, value % 3600 / 60),
        None => "上次刷新: --:--".into(),
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
        ("duration", "时长"),
        ("model", "模型"),
        ("context_percent", "上下文占比"),
        ("context_used", "已用上下文"),
        ("context_total", "总上下文"),
    ]
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
            (true, false) => text.push_str(&format!(" · 已用 {}", short_tokens(ctx.used_tokens))),
            (false, true) => text.push_str(&format!(" · 总计 {}", short_tokens(ctx.window_tokens))),
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
            (count > 0).then(|| format!("{count}个{}", state.label()))
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
    Icon::from_rgba(status_icon_rgba(state, frame), 16, 16).expect("valid icon")
}

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
