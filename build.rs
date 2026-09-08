// Embed the application icon into the Windows executable so Explorer, the
// taskbar and desktop shortcuts show the traffic-light artwork instead of a
// generic binary icon. No-op on macOS and Linux.
fn main() {
    #[cfg(target_os = "windows")]
    {
        let mut resources = winresource::WindowsResource::new();
        resources.set_icon("icons/AgentStatusIndicator.ico");
        if let Err(error) = resources.compile() {
            eprintln!("failed to embed the Windows application icon: {error}");
            std::process::exit(1);
        }
    }
}
