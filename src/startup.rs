use crate::i18n;
use std::{fs, path::PathBuf};

const LABEL: &str = "com.agentstatusindicator.app";

pub fn is_enabled() -> bool {
    startup_path().is_some_and(|path| path.is_file())
}

pub fn toggle() -> bool {
    if is_enabled() {
        let choice = crate::dialog::choose(
            i18n::text("startup_disable_prompt"),
            i18n::menu("startup"),
            &[i18n::text("cancel"), i18n::text("disable")],
            i18n::text("disable"),
            Some(i18n::text("cancel")),
        );
        if choice.as_deref() != Some(i18n::text("disable")) {
            return true;
        }
        if let Some(path) = startup_path() {
            let _ = unload(&path);
            let _ = fs::remove_file(path);
        }
        crate::dialog::notice(i18n::text("startup_disabled"), i18n::menu("startup"));
        return false;
    }
    let choice = crate::dialog::choose(
        i18n::text("startup_enable_prompt"),
        i18n::menu("startup"),
        &[i18n::text("cancel"), i18n::text("enable")],
        i18n::text("enable"),
        Some(i18n::text("cancel")),
    );
    if choice.as_deref() != Some(i18n::text("enable")) {
        return false;
    }
    if install().is_ok() {
        if let Some(path) = startup_path() {
            let _ = load(&path);
        }
        let _ = open_settings();
        crate::dialog::notice(i18n::text("startup_enabled"), i18n::menu("startup"));
        true
    } else {
        crate::dialog::notice(i18n::text("startup_failed"), i18n::menu("startup"));
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
    let path = startup_path().ok_or_else(|| std::io::Error::other("startup unsupported"))?;
    let executable = std::env::current_exe()?;
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("invalid launch agent path"))?;
    fs::create_dir_all(parent)?;
    #[cfg(target_os = "macos")]
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
    #[cfg(target_os = "windows")]
    let content = format!(
        "@echo off\r\nstart \"\" \"{}\"\r\n",
        executable.to_string_lossy().replace('"', "\"\"")
    );
    #[cfg(target_os = "linux")]
    let content = format!(
        "[Desktop Entry]\nType=Application\nName=AgentStatusIndicator\nExec={}\nX-GNOME-Autostart-enabled=true\n",
        desktop_escape(&executable.to_string_lossy())
    );
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let content = return Err(std::io::Error::other("startup unsupported"));
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

fn startup_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir().map(|home| home.join(format!("Library/LaunchAgents/{LABEL}.plist")))
    }
    #[cfg(not(target_os = "macos"))]
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        None
    }
    #[cfg(target_os = "windows")]
    {
        dirs::data_dir().map(|root| {
            root.join("Microsoft/Windows/Start Menu/Programs/Startup/AgentStatusIndicator.cmd")
        })
    }
    #[cfg(target_os = "linux")]
    {
        dirs::config_dir().map(|root| root.join("autostart/agent-status-indicator.desktop"))
    }
}

pub fn is_supported() -> bool {
    cfg!(any(
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    ))
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(target_os = "linux")]
fn desktop_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace(' ', "\\s")
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
