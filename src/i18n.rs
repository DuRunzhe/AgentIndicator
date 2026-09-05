use std::sync::{OnceLock, RwLock};

static LOCALE: OnceLock<RwLock<String>> = OnceLock::new();

pub fn set_locale(value: &str) {
    let value = if value == "auto" {
        system_locale()
    } else {
        value.to_owned()
    };
    *LOCALE
        .get_or_init(|| RwLock::new("zh-Hans".into()))
        .write()
        .expect("locale lock") = value;
}

pub fn refresh_system_locale() -> bool {
    let value = system_locale();
    let mut locale = LOCALE
        .get_or_init(|| RwLock::new("zh-Hans".into()))
        .write()
        .expect("locale lock");
    if *locale == value {
        false
    } else {
        *locale = value;
        true
    }
}

fn locale() -> String {
    LOCALE
        .get_or_init(|| RwLock::new("zh-Hans".into()))
        .read()
        .expect("locale lock")
        .clone()
}
fn system_locale() -> String {
    #[cfg(target_os = "macos")]
    if let Some(value) = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).to_lowercase())
    {
        if value.contains("zh-hant") || value.contains("zh_tw") || value.contains("zh_hk") {
            return "zh-Hant".into();
        }
        if value.contains("zh") {
            return "zh-Hans".into();
        }
        if value.contains("en") {
            return "en".into();
        }
    }
    let value = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default()
        .to_lowercase();
    if value.contains("zh_tw") || value.contains("zh_hk") || value.contains("hant") {
        "zh-Hant".into()
    } else if value.contains("zh") {
        "zh-Hans".into()
    } else {
        "en".into()
    }
}

pub fn state(key: &str) -> &'static str {
    match (locale().as_str(), key) {
        ("en", "waiting") => "Needs confirmation",
        ("en", "waiting_reply") => "Waiting for reply",
        ("en", "working") => "Working",
        ("en", "ready") => "Ready",
        ("en", "stopped") => "Stopped",
        ("zh-Hant", "waiting") => "等待確認",
        ("zh-Hant", "waiting_reply") => "等待回覆",
        ("zh-Hant", "working") => "進行中",
        ("zh-Hant", "ready") => "就緒",
        ("zh-Hant", "stopped") => "已停止",
        (_, "waiting") => "等待确认",
        (_, "waiting_reply") => "等待回复",
        (_, "working") => "进行中",
        (_, "ready") => "就绪",
        _ => "已停止",
    }
}
pub fn no_activity() -> &'static str {
    match locale().as_str() {
        "en" => "No activity",
        "zh-Hant" => "無活動",
        _ => "无活动",
    }
}

pub fn state_count(count: usize, state: &str) -> String {
    match locale().as_str() {
        "en" => format!("{count} {state}"),
        "zh-Hant" => format!("{count}個{state}"),
        _ => format!("{count}个{state}"),
    }
}

pub fn notification_message(state: &str, stage: usize) -> &'static str {
    let reply = state == "waiting_reply";
    match (locale().as_str(), reply, stage) {
        ("en", false, 0) => "Your confirmation is needed.",
        ("en", false, 1) => "Still waiting for confirmation (1 minute).",
        ("en", false, _) => "Still waiting for confirmation (3 minutes).",
        ("en", true, 0) => "Your reply is needed.",
        ("en", true, 1) => "Still waiting for your reply (1 minute).",
        ("en", true, _) => "Still waiting for your reply (3 minutes).",
        ("zh-Hant", false, 0) => "需要你的確認。",
        ("zh-Hant", false, 1) => "仍在等待確認（1 分鐘）。",
        ("zh-Hant", false, _) => "仍在等待確認（3 分鐘）。",
        ("zh-Hant", true, 0) => "需要你的回覆。",
        ("zh-Hant", true, 1) => "仍在等待你的回覆（1 分鐘）。",
        ("zh-Hant", true, _) => "仍在等待你的回覆（3 分鐘）。",
        (_, false, 0) => "需要你的确认。",
        (_, false, 1) => "仍在等待确认（1 分钟）。",
        (_, false, _) => "仍在等待确认（3 分钟）。",
        (_, true, 0) => "需要你的回复。",
        (_, true, 1) => "仍在等待你的回复（1 分钟）。",
        (_, true, _) => "仍在等待你的回复（3 分钟）。",
    }
}
pub fn language_name(value: &str) -> &'static str {
    match value {
        "auto" => "跟随系统 / System",
        "en" => "English",
        "zh-Hant" => "繁體中文",
        _ => "简体中文",
    }
}
pub fn menu(key: &str) -> &'static str {
    match (locale().as_str(), key) {
        ("en", "settings") => "Settings",
        ("en", "startup") => "Start at login",
        ("en", "notifications") => "Notifications",
        ("en", "test_notification") => "Send test notification",
        ("en", "browser") => "Browser tabs",
        ("en", "display") => "Display options",
        ("en", "refresh") => "Refresh now",
        ("en", "quit") => "Quit",
        ("en", "install_claude") => "Install Claude context collector",
        ("en", "language") => "Language",
        ("en", "login") => "Open Login Items Settings",
        ("en", "automation") => "Open Automation Settings",
        ("en", "permission") => "Requires browser Automation permission",
        ("zh-Hant", "settings") => "設定",
        ("zh-Hant", "startup") => "登入時啟動",
        ("zh-Hant", "notifications") => "通知",
        ("zh-Hant", "test_notification") => "發送測試通知",
        ("zh-Hant", "browser") => "瀏覽器分頁",
        ("zh-Hant", "display") => "顯示設定",
        ("zh-Hant", "refresh") => "立即重新整理",
        ("zh-Hant", "quit") => "結束",
        ("zh-Hant", "install_claude") => "安裝 Claude 上下文收集器",
        ("zh-Hant", "language") => "語言",
        ("zh-Hant", "login") => "開啟登入項目設定",
        ("zh-Hant", "automation") => "開啟自動化設定",
        ("zh-Hant", "permission") => "需要瀏覽器自動化權限",
        (_, "settings") => "设置",
        (_, "startup") => "开机自启",
        (_, "notifications") => "通知",
        (_, "test_notification") => "发送测试通知",
        (_, "browser") => "浏览器标签页",
        (_, "display") => "显示配置",
        (_, "refresh") => "立即刷新",
        (_, "quit") => "退出",
        (_, "install_claude") => "安装 Claude 上下文采集",
        (_, "language") => "语言 / Language",
        (_, "login") => "打开登录项设置",
        (_, "automation") => "打开系统自动化设置",
        _ => "需要浏览器自动化权限",
    }
}

/// Dynamic actions and small status labels that are not submenu titles. Keeping
/// them here prevents a locale switch from leaving mixed-language menu rows.
pub fn text(key: &str) -> &'static str {
    match (locale().as_str(), key) {
        ("en", "disable_notifications") => "Click to disable notifications",
        ("en", "enable_notifications") => "Click to enable notifications",
        ("en", "disable_startup") => "Click to disable start at login",
        ("en", "enable_startup") => "Click to enable start at login",
        ("en", "disable_browser_tabs") => "Click to disable browser tab reuse",
        ("en", "enable_browser_tabs") => "Click to enable browser tab reuse",
        ("en", "open_notifications") => "Open System Notification Settings",
        ("en", "notification_app") => "System notification app: AgentStatusIndicator",
        ("en", "restart_detector") => "Restart detector",
        ("en", "last_updated") => "Last updated",
        ("en", "test_notification_title") => "AgentStatusIndicator · Test notification",
        ("en", "test_notification_body") => "Notifications are enabled. Click to verify native delivery.",
        ("en", "notification_denied") => "macOS did not grant notification permission. Allow this app in System Settings → Notifications and try again.",
        ("en", "notifications_disabled") => "Notifications disabled",
        ("en", "show_duration") => "Duration",
        ("en", "show_model") => "Model",
        ("en", "show_context_percent") => "Context usage percentage",
        ("en", "show_context_used") => "Context used",
        ("en", "show_context_total") => "Total context",
        ("en", "notify_waiting_confirmation") => "Notify: needs confirmation",
        ("en", "notify_waiting_reply") => "Notify: waiting for reply",
        ("en", "notify_auto_confirm") => "Notify in auto-confirmation mode",
        ("en", "cancel") => "Cancel",
        ("en", "enable") => "Enable",
        ("en", "disable") => "Disable",
        ("en", "continue") => "Continue",
        ("en", "not_yet") => "Not yet",
        ("en", "authorized") => "Authorized",
        ("en", "startup_enable_prompt") => "AgentStatusIndicator will install a user startup item and show tray status when you sign in.",
        ("en", "startup_disable_prompt") => "Turn off AgentStatusIndicator start at login?",
        ("en", "startup_enabled") => "Start at login is enabled. Confirm it in Login Items settings.",
        ("en", "startup_disabled") => "Start at login is disabled.",
        ("en", "startup_failed") => "Unable to change start-at-login settings. Check the installation path and try again.",
        ("en", "browser_prompt") => "AgentStatusIndicator can reuse an existing DeepSeek Harness browser tab. Chrome, Edge, Brave, or Safari must permit Automation access.",
        ("en", "browser_verify") => "If macOS asked for Automation permission, allow browser access. Have you completed authorization?",
        ("en", "browser_enabled") => "Browser tab reuse is enabled.",
        ("en", "browser_verify_title") => "Verify Automation permission",
        ("en", "claude_install_prompt") => "Configure Claude Code context collection while preserving and forwarding your current statusLine command.",
        ("en", "claude_installed") => "Claude context collection is installed. Start or resume a Claude session to write data.",
        ("en", "claude_install_failed") => "Claude context collection was not installed",
        ("en", "install") => "Install",
        ("en", "no_agents") => "⚪ No running agents found",
        ("en", "context_used") => "Used",
        ("en", "context_total") => "Total",
        ("zh-Hant", "disable_notifications") => "點擊關閉通知",
        ("zh-Hant", "enable_notifications") => "點擊開啟通知",
        ("zh-Hant", "disable_startup") => "點擊關閉登入時啟動",
        ("zh-Hant", "enable_startup") => "點擊開啟登入時啟動",
        ("zh-Hant", "disable_browser_tabs") => "點擊關閉瀏覽器分頁重用",
        ("zh-Hant", "enable_browser_tabs") => "點擊開啟瀏覽器分頁重用",
        ("zh-Hant", "open_notifications") => "開啟系統通知設定",
        ("zh-Hant", "notification_app") => "系統通知應用程式：AgentStatusIndicator",
        ("zh-Hant", "restart_detector") => "重新啟動偵測器",
        ("zh-Hant", "last_updated") => "上次更新",
        ("zh-Hant", "test_notification_title") => "AgentStatusIndicator · 測試通知",
        ("zh-Hant", "test_notification_body") => "通知已啟用。點擊以驗證原生通知投遞。",
        ("zh-Hant", "notification_denied") => "macOS 未授予通知權限。請在系統設定 → 通知中允許此應用程式後重試。",
        ("zh-Hant", "notifications_disabled") => "通知未開啟",
        ("zh-Hant", "show_duration") => "時長",
        ("zh-Hant", "show_model") => "模型",
        ("zh-Hant", "show_context_percent") => "上下文使用百分比",
        ("zh-Hant", "show_context_used") => "已用上下文",
        ("zh-Hant", "show_context_total") => "總上下文",
        ("zh-Hant", "notify_waiting_confirmation") => "通知：等待確認",
        ("zh-Hant", "notify_waiting_reply") => "通知：等待回覆",
        ("zh-Hant", "notify_auto_confirm") => "自動確認模式仍通知",
        ("zh-Hant", "cancel") => "取消",
        ("zh-Hant", "enable") => "開啟",
        ("zh-Hant", "disable") => "關閉",
        ("zh-Hant", "continue") => "繼續",
        ("zh-Hant", "not_yet") => "尚未完成",
        ("zh-Hant", "authorized") => "已授權",
        ("zh-Hant", "startup_enable_prompt") => "AgentStatusIndicator 將安裝使用者登入項目，登入後自動顯示選單列狀態。",
        ("zh-Hant", "startup_disable_prompt") => "確定關閉 AgentStatusIndicator 的登入時啟動嗎？",
        ("zh-Hant", "startup_enabled") => "登入時啟動已開啟，請在登入項目設定中確認。",
        ("zh-Hant", "startup_disabled") => "登入時啟動已關閉。",
        ("zh-Hant", "startup_failed") => "無法修改登入時啟動設定，請檢查安裝路徑後重試。",
        ("zh-Hant", "browser_prompt") => "AgentStatusIndicator 可重用既有的 DeepSeek Harness 瀏覽器分頁。Chrome、Edge、Brave 或 Safari 需要允許自動化存取。",
        ("zh-Hant", "browser_verify") => "如果 macOS 彈出自動化權限要求，請允許瀏覽器存取。你是否已完成授權？",
        ("zh-Hant", "browser_enabled") => "瀏覽器分頁重用已開啟。",
        ("zh-Hant", "browser_verify_title") => "驗證自動化權限",
        ("zh-Hant", "claude_install_prompt") => "將為 Claude Code 設定上下文收集，並保留、轉送你目前的 statusLine 命令。",
        ("zh-Hant", "claude_installed") => "Claude 上下文收集已安裝。請在 Claude 中開始或繼續一次會話以寫入資料。",
        ("zh-Hant", "claude_install_failed") => "Claude 上下文收集未安裝",
        ("zh-Hant", "install") => "安裝",
        ("zh-Hant", "no_agents") => "⚪ 未發現執行中的 Agent",
        ("zh-Hant", "context_used") => "已用",
        ("zh-Hant", "context_total") => "總計",
        (_, "disable_notifications") => "点击关闭通知",
        (_, "enable_notifications") => "点击开启通知",
        (_, "disable_startup") => "点击关闭开机自启",
        (_, "enable_startup") => "点击开启开机自启",
        (_, "disable_browser_tabs") => "点击关闭浏览器标签页复用",
        (_, "enable_browser_tabs") => "点击开启浏览器标签页复用",
        (_, "open_notifications") => "打开系统通知设置",
        (_, "notification_app") => "系统通知应用：AgentStatusIndicator",
        (_, "restart_detector") => "重启检测器",
        (_, "last_updated") => "上次刷新",
        (_, "test_notification_title") => "AgentStatusIndicator · 测试通知",
        (_, "test_notification_body") => "通知已启用。点击此通知可验证原生投递。",
        (_, "notification_denied") => "macOS 未授予 AgentStatusIndicator 通知权限。请在系统设置的“通知”中允许此应用后重试。",
        (_, "show_duration") => "时长",
        (_, "show_model") => "模型",
        (_, "show_context_percent") => "上下文占比",
        (_, "show_context_used") => "已用上下文",
        (_, "show_context_total") => "总上下文",
        (_, "notify_waiting_confirmation") => "通知：等待确认",
        (_, "notify_waiting_reply") => "通知：等待回复",
        (_, "notify_auto_confirm") => "自动确认模式仍通知",
        (_, "cancel") => "取消",
        (_, "enable") => "开启",
        (_, "disable") => "关闭",
        (_, "continue") => "继续",
        (_, "not_yet") => "还没有",
        (_, "authorized") => "已授权",
        (_, "startup_enable_prompt") => "AgentStatusIndicator 将安装用户级启动项，在你登录 Mac 后自动显示菜单栏状态。",
        (_, "startup_disable_prompt") => "确定关闭 AgentStatusIndicator 的开机自启吗？",
        (_, "startup_enabled") => "开机自启已开启，请在系统登录项设置中确认。",
        (_, "startup_disabled") => "开机自启已关闭。",
        (_, "startup_failed") => "无法修改开机自启设置，请检查安装路径后重试。",
        (_, "browser_prompt") => "AgentStatusIndicator 可复用已有的 DeepSeek Harness 浏览器标签页。Chrome、Edge、Brave 或 Safari 需要允许自动化访问。",
        (_, "browser_verify") => "如果 macOS 弹出了自动化权限请求，请允许访问浏览器。你是否已完成授权？",
        (_, "browser_enabled") => "浏览器标签页复用已开启。",
        (_, "browser_verify_title") => "验证自动化权限",
        (_, "claude_install_prompt") => "将为 Claude Code 配置上下文采集，并保留、转发你当前的 statusLine 命令。",
        (_, "claude_installed") => "Claude 上下文采集已安装。请在 Claude 中开始或继续一次会话以写入数据。",
        (_, "claude_install_failed") => "Claude 上下文采集未安装",
        (_, "install") => "安装",
        (_, "no_agents") => "⚪ 未发现运行中的 Agent",
        (_, "context_used") => "已用",
        (_, "context_total") => "总计",
        _ => "通知未开启",
    }
}
