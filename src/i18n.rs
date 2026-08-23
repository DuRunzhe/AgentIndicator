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

fn locale() -> String {
    LOCALE
        .get_or_init(|| RwLock::new("zh-Hans".into()))
        .read()
        .expect("locale lock")
        .clone()
}
fn system_locale() -> String {
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
