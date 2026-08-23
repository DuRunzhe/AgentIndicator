use crate::dialog;
use std::process::Command;

pub fn configure(currently_enabled: bool) -> bool {
    if currently_enabled {
        return false;
    }
    let choice = dialog::choose(
        "AgentStatusIndicator 可复用已有的 DeepSeek Harness 浏览器标签页。Chrome、Edge、Brave 或 Safari 需要允许自动化访问。",
        "复用浏览器标签页", &["取消", "继续"], "继续", Some("取消"),
    );
    if choice.as_deref() != Some("继续") {
        return false;
    }
    if !probe_automation() {
        let _ = open_automation_settings();
        let verified = dialog::choose(
            "如果 macOS 弹出了自动化权限请求，请允许访问浏览器。你是否已完成授权？",
            "验证自动化权限",
            &["还没有", "已授权"],
            "已授权",
            Some("还没有"),
        );
        if verified.as_deref() != Some("已授权") {
            return false;
        }
    }
    dialog::notice("浏览器标签页复用已开启。", "复用浏览器标签页");
    true
}

pub fn open_automation_settings() -> bool {
    #[cfg(target_os = "macos")]
    return Command::new("/usr/bin/open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Automation")
        .status()
        .is_ok_and(|status| status.success());
    #[cfg(not(target_os = "macos"))]
    false
}

fn probe_automation() -> bool {
    #[cfg(target_os = "macos")]
    return Command::new("/usr/bin/osascript")
        .args(["-e", "tell application \"Google Chrome\" to if it is running then get URL of tabs of windows"])
        .output().is_ok_and(|output| output.status.success());
    #[cfg(not(target_os = "macos"))]
    false
}
