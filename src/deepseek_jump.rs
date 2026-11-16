//! Session-addressed navigation into the DeepSeek Harness desktop application.
//!
//! The desktop app is one process serving every conversation, so a tray row
//! cannot be focused by activating the app: it has to name the conversation it
//! wants. Three rungs of capability are attempted in order, and the click always
//! ends somewhere useful:
//!
//! 1. **Native route** — `dsh://session/<id>`. Today's shell answers only
//!    `dsh://open` and silently ignores everything else, so this rung is gated on
//!    a released version that answers the route. When upstream ships it, setting
//!    [`NATIVE_ROUTE_MIN_VERSION`] is the whole change: rows already carry the
//!    URL, so the click starts travelling through it with no user-visible
//!    transition.
//! 2. **Accessibility** — drive the app's own sidebar through the macOS
//!    accessibility API: find the conversation row by title and press it.
//! 3. **Window** — the historical behaviour: `dsh://open`, which restores a
//!    minimized window and recreates a closed one.
//!
//! The target travels as the URL it will eventually be, so no layer has to
//! change shape when a rung starts working: a row stores
//! `dsh://session/<id>?title=…&workspace=…`, the native rung hands it to the OS
//! verbatim, and the accessibility rung reads the same fields back out.

/// The accessibility rung: driving the app's own sidebar.
///
/// Only macOS exposes a tree this project can drive without shipping native
/// automation per platform, so every other target reports the rung as
/// unavailable and the ladder goes straight to the window. Windows and Linux
/// keep working exactly as before, and gain the session rung once their own
/// bridge exists.
#[cfg(target_os = "macos")]
use crate::deepseek_ax as accessibility;
#[cfg(not(target_os = "macos"))]
mod accessibility {
    use super::SessionTarget;

    /// Whether this platform can drive the sidebar at all.
    pub fn authorized() -> bool {
        false
    }

    /// Whether the conversation was brought up. Never on this platform.
    pub fn press_session(_: &SessionTarget) -> bool {
        false
    }
}

/// The prefix a session route uses. Everything after it is the session id.
pub const SESSION_PREFIX: &str = "dsh://session/";

/// The one route today's desktop shell answers: bring the primary window
/// forward, restoring a minimized window and recreating a closed one.
pub const OPEN_URL: &str = "dsh://open";

/// The first DeepSeek Harness release whose shell answers `dsh://session/<id>`.
///
/// `None` until upstream ships the route: no released build answers it, so the
/// ladder starts at the accessibility rung instead of sending an URL the shell
/// would drop on the floor. Setting this to the releasing version is the entire
/// migration — rows already carry the URL, and the accessibility rung stays as
/// the fallback for older installs.
pub const NATIVE_ROUTE_MIN_VERSION: Option<&str> = None;

/// The bundle identifier the desktop application registers, used to find its
/// process without scanning the process table.
pub const BUNDLE_ID: &str = "com.deepseek.dsh";

/// A conversation the desktop window can be asked to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTarget {
    /// The session id from the profile's projection cache.
    pub id: String,
    /// The title the app's sidebar shows for the conversation.
    pub title: Option<String>,
    /// The workspace the sidebar groups the conversation under, which is the
    /// project directory's name. Titles are not unique across workspaces, so a
    /// row that carries one is matched inside its own group instead of whichever
    /// same-titled conversation the list happens to hold first.
    pub workspace: Option<String>,
}

impl SessionTarget {
    /// The route a row carries to this conversation.
    ///
    /// The shape is the eventual native one, with the two sidebar facts as query
    /// parameters: an upstream shell that answers the route ignores unknown
    /// parameters, and the accessibility rung reads them back. Values are
    /// percent-encoded so a title may contain `&`, `=` or a space.
    pub fn url(&self) -> String {
        let mut url = format!("{SESSION_PREFIX}{}", encode(&self.id));
        let mut query: Vec<String> = Vec::new();
        if let Some(title) = &self.title {
            query.push(format!("title={}", encode(title)));
        }
        if let Some(workspace) = &self.workspace {
            query.push(format!("workspace={}", encode(workspace)));
        }
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query.join("&"));
        }
        url
    }

    /// The route a tray row carries for a desktop conversation.
    ///
    /// The sidebar groups conversations by workspace, whose name is the project
    /// directory's name, so that is what the route carries alongside the title:
    /// it is what lets two conversations with the same title be told apart.
    pub fn route(session: &crate::deepseek_desktop::DesktopSession) -> String {
        Self {
            id: session.id.clone(),
            title: session.title.clone(),
            workspace: session
                .cwd
                .as_deref()
                .and_then(std::path::Path::file_name)
                .and_then(std::ffi::OsStr::to_str)
                .map(str::to_owned),
        }
        .url()
    }

    /// Read a session route back out of an URL, or `None` for any other address.
    pub fn parse(url: &str) -> Option<Self> {
        let rest = url.strip_prefix(SESSION_PREFIX)?;
        let (id, query) = match rest.split_once('?') {
            Some((id, query)) => (id, Some(query)),
            None => (rest, None),
        };
        let id = decode(id);
        if id.is_empty() {
            return None;
        }
        let mut title = None;
        let mut workspace = None;
        for pair in query.unwrap_or_default().split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            match key {
                "title" => title = Some(decode(value)),
                "workspace" => workspace = Some(decode(value)),
                _ => {}
            }
        }
        Some(Self {
            id,
            title: title.filter(|value| !value.is_empty()),
            workspace: workspace.filter(|value| !value.is_empty()),
        })
    }
}

/// Whether a row's address is a conversation rather than a page.
pub fn is_session_url(url: &str) -> bool {
    url.starts_with(SESSION_PREFIX)
}

/// The rung a click used, deepest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Handed to the app as `dsh://session/<id>`.
    Native,
    /// Pressed in the app's own sidebar through the accessibility API.
    Accessibility,
    /// Only the window, `dsh://open`.
    Window,
}

impl Route {
    pub fn key(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Accessibility => "accessibility",
            Self::Window => "window",
        }
    }
}

/// What a click actually achieved, and whether it reached anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub route: Route,
    pub delivered: bool,
}

/// What the user asked for: the setting is a switch, not a mode. Turning it off
/// keeps the row clickable and stops it past anything but the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Whether the session itself may be targeted.
    pub session_level: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            session_level: true,
        }
    }
}

impl Options {
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self {
            session_level: config.deepseek_session_jump,
        }
    }
}

/// What this machine can do right now, for the diagnostic and the settings row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// The installed app answers `dsh://session/<id>` itself.
    pub native_route: bool,
    /// macOS granted this application the accessibility permission.
    pub accessibility: bool,
}

impl Capabilities {
    pub fn probe() -> Self {
        let capabilities = Self {
            native_route: native_route_available(),
            accessibility: accessibility::authorized(),
        };
        // Once per process: the menu probes this on every rebuild, so a changing
        // value is what gets recorded, not the value itself.
        static LAST: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(u16::MAX);
        let encoded =
            u16::from(capabilities.native_route) | (u16::from(capabilities.accessibility) << 1);
        if LAST.swap(encoded, std::sync::atomic::Ordering::Relaxed) != encoded {
            log(&format!(
                "capabilities nativeRoute={} accessibility={} pid={}",
                capabilities.native_route,
                capabilities.accessibility,
                std::process::id()
            ));
        }
        capabilities
    }
}

/// The rungs a click may use for `url`, deepest first.
///
/// Pure so the degradation order is testable without an app, a permission or a
/// desktop: `open` walks the list, and a rung that fails hands the click to the
/// next one.
pub fn ladder(url: &str, options: Options, capabilities: Capabilities) -> Vec<Route> {
    let addressable =
        options.session_level && is_session_url(url) && SessionTarget::parse(url).is_some();
    let mut rungs = Vec::new();
    if addressable && capabilities.native_route {
        rungs.push(Route::Native);
    }
    if addressable && capabilities.accessibility {
        rungs.push(Route::Accessibility);
    }
    // The window is always last, and always present: a click that cannot reach
    // the conversation must still reach the application.
    rungs.push(Route::Window);
    rungs
}

/// Navigate a click to the target an URL names.
///
/// The ladder only ever degrades: a rung that is unavailable, fails, or cannot
/// be confirmed hands the click to the next one, so a denied permission or an
/// app update that moves the sidebar costs the user the conversation, not the
/// window.
pub fn open(url: &str, options: Options) -> Outcome {
    let started = std::time::Instant::now();
    let target = SessionTarget::parse(url);
    let capabilities = Capabilities::probe();
    let rungs = ladder(url, options, capabilities);
    for rung in rungs {
        let delivered = match (rung, target.as_ref()) {
            (Route::Native, Some(target)) => hand_to_os(&target.url()),
            (Route::Accessibility, Some(target)) => accessibility::press_session(target),
            _ => hand_to_os(OPEN_URL),
        };
        if delivered {
            log(&format!(
                "jump route={} delivered=true ms={} session={} title={:?}",
                rung.key(),
                started.elapsed().as_millis(),
                target.as_ref().map_or("-", |target| target.id.as_str()),
                target.as_ref().and_then(|target| target.title.as_deref())
            ));
            return Outcome {
                route: rung,
                delivered,
            };
        }
    }
    log(&format!(
        "jump route=window delivered=false ms={} session={} title={:?}",
        started.elapsed().as_millis(),
        target.as_ref().map_or("-", |target| target.id.as_str()),
        target.as_ref().and_then(|target| target.title.as_deref())
    ));
    Outcome {
        route: Route::Window,
        delivered: false,
    }
}

/// Open the window only, the historical behaviour, kept for rows without a
/// conversation and for the setting being off.
pub fn open_window() -> bool {
    hand_to_os(OPEN_URL)
}

/// Hand an URL to the OS.
///
/// The scheme is registered by the desktop application on macOS, which is the
/// only platform where the tray can address it; elsewhere the handler is
/// reported missing so the caller keeps its pre-existing behaviour instead of
/// shelling out to a command that does not exist.
fn hand_to_os(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        return std::process::Command::new("open")
            .arg(url)
            .status()
            .is_ok_and(|status| status.success());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = url;
        false
    }
}

/// Append one line to the session-jump log.
///
/// A jump is invisible when it degrades: the window still comes forward, so the
/// only evidence that the conversation rung failed is the reason inside the
/// process. The log keeps that reason, capped so a long-lived tray cannot grow
/// it without bound.
pub fn log(event: &str) {
    use std::io::Write;
    let Some(path) =
        dirs::data_dir().map(|root| root.join("agent-status-indicator/session-jump.log"))
    else {
        return;
    };
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > 256 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "{event}");
    }
}

/// Whether the installed desktop application answers session routes itself.
///
/// A version comparison rather than a probe: sending the URL to a build that
/// ignores it looks exactly like sending it to one that understands it (the
/// shell drops unknown routes silently and `open` still exits 0), so there is
/// nothing to observe. The override exists for the transition window, when a
/// user runs a build that answers the route before this constant moved.
pub fn native_route_available() -> bool {
    if let Some(value) = std::env::var_os("AGENT_STATUS_INDICATOR_DSH_SESSION_URL") {
        return value != "0";
    }
    let Some(minimum) = NATIVE_ROUTE_MIN_VERSION else {
        return false;
    };
    crate::deepseek_desktop::installed_version()
        .is_some_and(|installed| version_at_least(&installed, minimum))
}

/// `installed >= required`, comparing dotted numeric components.
///
/// A pre-release suffix on the installed build (`0.2.2-rc.3`) counts as the
/// numeric version it is based on: the route either exists in that build or it
/// does not, and the constant is set from releases, so erring towards attempting
/// the route costs nothing — the next rung still runs when the shell ignores it.
fn version_at_least(installed: &str, required: &str) -> bool {
    fn components(value: &str) -> Vec<u64> {
        value
            .split(['.', '-', '+'])
            .take_while(|part| part.chars().all(|ch| ch.is_ascii_digit()))
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    }
    let installed = components(installed);
    let required = components(required);
    for index in 0..installed.len().max(required.len()) {
        let left = installed.get(index).copied().unwrap_or(0);
        let right = required.get(index).copied().unwrap_or(0);
        if left != right {
            return left > right;
        }
    }
    true
}

/// Percent-encode everything outside the unreserved set, which is what keeps a
/// title with spaces, `&` or a non-ASCII character readable in a URL.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Inverse of [`encode`]. Invalid sequences are kept verbatim rather than
/// dropped: a title is a display hint, and losing a character must not lose the
/// session id next to it.
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(byte) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> SessionTarget {
        SessionTarget {
            id: "session-0c3f162a".into(),
            title: Some("搭建局域网Git协议仓库".into()),
            workspace: Some("manisLugo".into()),
        }
    }

    #[test]
    fn a_row_carries_the_route_it_will_eventually_use() {
        let url = target().url();
        assert!(url.starts_with("dsh://session/session-0c3f162a?"));
        // The shape is the native one, so nothing has to change when the shell
        // starts answering it: only the id is in the path, the sidebar facts are
        // parameters the shell is free to ignore.
        assert_eq!(
            url.split('?').next(),
            Some("dsh://session/session-0c3f162a")
        );
        assert!(url.contains("title=%E6%90%AD%E5%BB%BA"), "{url}");
        assert!(url.contains("workspace=manisLugo"), "{url}");
        assert!(is_session_url(&url));
    }

    #[test]
    fn a_route_round_trips_through_its_url() {
        let url = target().url();
        assert_eq!(SessionTarget::parse(&url), Some(target()));
        // A title with the separators the URL itself uses must survive intact,
        // or the row would press the wrong conversation.
        let awkward = SessionTarget {
            id: "s1".into(),
            title: Some("a & b = c / d ? e # f 100%".into()),
            workspace: Some("w s".into()),
        };
        let url = awkward.url();
        assert!(!url.contains(" = "), "the value must be encoded: {url}");
        assert_eq!(SessionTarget::parse(&url), Some(awkward));
    }

    #[test]
    fn a_route_without_sidebar_hints_still_parses() {
        let url = "dsh://session/abc";
        assert_eq!(
            SessionTarget::parse(url),
            Some(SessionTarget {
                id: "abc".into(),
                title: None,
                workspace: None,
            })
        );
        // Other addresses are not sessions: `dsh://open` keeps its own meaning,
        // and an empty id cannot address anything.
        assert_eq!(SessionTarget::parse("dsh://open"), None);
        assert_eq!(SessionTarget::parse("dsh://session/"), None);
        assert_eq!(SessionTarget::parse("https://example.com"), None);
        assert!(!is_session_url(OPEN_URL));
    }

    #[test]
    fn the_native_rung_is_gated_on_a_release_that_answers_it() {
        // No released build answers `dsh://session/<id>`: its shell handles only
        // `dsh://open`. Sending the route anyway would look successful (`open`
        // exits 0) while doing nothing, so the ladder must not start there.
        assert!(
            NATIVE_ROUTE_MIN_VERSION.is_none(),
            "revisit the ladder once an upstream release answers the route"
        );
        // Without the override the probe agrees with the constant.
        if std::env::var_os("AGENT_STATUS_INDICATOR_DSH_SESSION_URL").is_none() {
            assert!(!native_route_available());
        }
    }

    #[test]
    fn version_comparison_orders_dotted_releases() {
        assert!(version_at_least("0.2.2", "0.2.2"));
        assert!(version_at_least("0.2.10", "0.2.9"));
        assert!(version_at_least("0.3.0", "0.2.99"));
        assert!(version_at_least("1.0", "0.244.1"));
        assert!(!version_at_least("0.2.1", "0.2.2"));
        assert!(!version_at_least("0.1.9", "0.2"));
        // A pre-release counts as the numeric release it is based on.
        assert!(version_at_least("0.2.2-rc.3", "0.2.2"));
        assert!(version_at_least("0.3.0-alpha.1", "0.2.9"));
        assert!(!version_at_least("0.2.2-rc.3", "0.3.0"));
        // Nonsense cannot claim support.
        assert!(!version_at_least("nightly", "0.2.0"));
    }

    fn all() -> Capabilities {
        Capabilities {
            native_route: true,
            accessibility: true,
        }
    }

    #[test]
    fn the_ladder_degrades_one_rung_at_a_time() {
        let url = target().url();
        let options = Options::default();
        // Everything available: the native route is tried first.
        assert_eq!(
            ladder(&url, options, all()),
            vec![Route::Native, Route::Accessibility, Route::Window]
        );
        // No released build answers the route yet, which is the state of the
        // world today: the accessibility rung leads.
        let today = Capabilities {
            native_route: false,
            accessibility: true,
        };
        assert_eq!(
            ladder(&url, options, today),
            vec![Route::Accessibility, Route::Window]
        );
        // Permission denied: the window is all that is left.
        let denied = Capabilities {
            native_route: false,
            accessibility: false,
        };
        assert_eq!(ladder(&url, options, denied), vec![Route::Window]);
        // A future release keeps the accessibility rung as the fallback for
        // installs that predate it.
        let future = Capabilities {
            native_route: true,
            accessibility: false,
        };
        assert_eq!(
            ladder(&url, options, future),
            vec![Route::Native, Route::Window]
        );
    }

    #[test]
    #[ignore = "drives the running desktop application"]
    fn smoke_open_lands_on_the_accessibility_rung() {
        // The whole point of the ladder: with no native route in any released
        // build, a click still reaches the conversation through the sidebar.
        let url = target().url();
        let outcome = open(&url, Options::default());
        assert_eq!(
            outcome,
            Outcome {
                route: Route::Accessibility,
                delivered: true,
            }
        );
        // A conversation the sidebar does not hold cannot be pressed, so the
        // click degrades to the window instead of claiming success.
        let missing = SessionTarget {
            id: "session-missing".into(),
            title: Some("这个会话不存在 99天".into()),
            workspace: None,
        };
        let outcome = open(&missing.url(), Options::default());
        assert_eq!(outcome.route, Route::Window);
    }

    #[test]
    fn the_setting_and_the_address_both_gate_the_session_rungs() {
        let off = Options {
            session_level: false,
        };
        assert_eq!(ladder(&target().url(), off, all()), vec![Route::Window]);
        // A row that is not a conversation cannot take a session rung either:
        // the plain window route and a subagent's web page both stay put.
        assert_eq!(
            ladder(OPEN_URL, Options::default(), all()),
            vec![Route::Window]
        );
        assert_eq!(
            ladder("http://127.0.0.1:3000/", Options::default(), all()),
            vec![Route::Window]
        );
        assert_eq!(
            ladder("dsh://session/", Options::default(), all()),
            vec![Route::Window]
        );
    }
}
