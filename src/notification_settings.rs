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
