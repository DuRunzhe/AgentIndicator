use crate::dialog;

pub fn configure(currently_enabled: bool) -> bool {
    if currently_enabled {
        return false;
    }
    let choice = dialog::choose(
        "接下来会发送一条测试通知并打开 macOS 通知设置。请为 AgentStatusIndicator 开启“允许通知”和横幅。",
        "开启通知", &["取消", "打开设置"], "打开设置", Some("取消"),
    );
    if choice.as_deref() != Some("打开设置") {
        return false;
    }
    let _ = notify_rust::Notification::new()
        .appname("AgentStatusIndicator")
        .summary("AgentStatusIndicator")
        .body("通知权限测试：如果看到了这条通知，请返回并确认。")
        .sound_name("default")
        .show();
    if !open_settings() {
        dialog::notice(
            "无法打开 macOS 通知设置，请手动进入“系统设置 → 通知”后重试。",
            "开启通知",
        );
        return false;
    }
    let verified = dialog::choose(
        "在系统设置中开启通知后，你是否看到了 AgentStatusIndicator 测试通知？",
        "验证通知",
        &["还没有", "已看到"],
        "已看到",
        Some("还没有"),
    );
    if verified.as_deref() != Some("已看到") {
        return false;
    }
    dialog::notice(
        "通知已开启。进入等待确认或等待回复时将发送提醒。",
        "开启通知",
    );
    true
}

pub fn open_settings() -> bool {
    #[cfg(target_os = "macos")]
    {
        return std::process::Command::new("/usr/bin/open")
            .arg("x-apple.systempreferences:com.apple.Notifications-Settings.extension")
            .status()
            .is_ok_and(|status| status.success());
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}
