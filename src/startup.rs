use std::{fs, path::PathBuf};

const LABEL: &str = "com.agentstatusindicator.app";

pub fn is_enabled() -> bool {
    plist_path().is_some_and(|path| path.is_file())
}

pub fn toggle() -> bool {
    if is_enabled() {
        let choice = crate::dialog::choose(
            "确定关闭 AgentStatusIndicator 的开机自启吗？",
            "开机自启",
            &["取消", "关闭"],
            "关闭",
            Some("取消"),
        );
        if choice.as_deref() != Some("关闭") {
            return true;
        }
        if let Some(path) = plist_path() {
            let _ = unload(&path);
            let _ = fs::remove_file(path);
        }
        crate::dialog::notice("开机自启已关闭。", "开机自启");
        return false;
    }
    let choice = crate::dialog::choose(
        "AgentStatusIndicator 将安装用户级启动项，在你登录 Mac 后自动显示菜单栏状态。",
        "开机自启",
        &["取消", "开启"],
        "开启",
        Some("取消"),
    );
    if choice.as_deref() != Some("开启") {
        return false;
    }
    if install().is_ok() {
        if let Some(path) = plist_path() {
            let _ = load(&path);
        }
        let _ = open_settings();
        crate::dialog::notice("开机自启已开启，请在系统登录项设置中确认。", "开机自启");
        true
    } else {
        crate::dialog::notice("无法修改开机自启设置，请检查安装路径后重试。", "开机自启");
        false
    }
}

pub fn open_settings() -> bool {
    #[cfg(target_os = "macos")]
    {
        return std::process::Command::new("/usr/bin/open")
            .arg("x-apple.systempreferences:com.apple.LoginItems-Settings.extension")
            .status()
            .is_ok_and(|status| status.success());
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

fn install() -> std::io::Result<()> {
    let path = plist_path().ok_or_else(|| std::io::Error::other("home unavailable"))?;
    let executable = std::env::current_exe()?;
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("invalid launch agent path"))?;
    fs::create_dir_all(parent)?;
    let content = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\"><dict>\n\
<key>Label</key><string>{LABEL}</string>\n\
<key>ProgramArguments</key><array><string>{}</string></array>\n\
<key>RunAtLoad</key><true/>\n\
<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n\
<key>ProcessType</key><string>Interactive</string>\n\
</dict></plist>\n",
        xml_escape(&executable.to_string_lossy())
    );
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temporary, content)?;
    fs::rename(temporary, path)
}

fn load(path: &std::path::Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        return std::process::Command::new("/bin/launchctl")
            .args(["bootstrap", &format!("gui/{}", unsafe { libc::geteuid() })])
            .arg(path)
            .status()
            .is_ok_and(|status| status.success());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        false
    }
}

fn unload(path: &std::path::Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        return std::process::Command::new("/bin/launchctl")
            .args(["bootout", &format!("gui/{}", unsafe { libc::geteuid() })])
            .arg(path)
            .status()
            .is_ok_and(|status| status.success());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        false
    }
}

fn plist_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir().map(|home| home.join(format!("Library/LaunchAgents/{LABEL}.plist")))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_paths_are_xml_escaped() {
        assert_eq!(xml_escape("/A&B/<app>"), "/A&amp;B/&lt;app&gt;");
    }

    #[test]
    fn launch_agent_restarts_only_after_an_unsuccessful_exit() {
        let value = "<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>";
        assert!(value.contains("SuccessfulExit"));
    }
}
