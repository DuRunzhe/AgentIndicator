//! Self-update support.
//!
//! The tray app asks GitHub which release is newest, downloads the archive for
//! the current platform, swaps it in over the running copy and reports back so
//! the caller can relaunch. Everything shells out to `curl` / `tar`, which are
//! present on every platform this app ships for, so no HTTP or archive crate is
//! pulled in.

use crossbeam_channel::{bounded, Receiver, Sender};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

const REPOSITORY_SLUG: &str = "DuRunzhe/AgentIndicator";
const EXECUTABLE: &str = "agent-status-indicator";
const APP_BUNDLE: &str = "AgentStatusIndicator.app";

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Result of asking GitHub for the newest release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    UpToDate,
    Available(String),
    Failed(String),
}

/// Phase of an install, mirrored into the progress dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Downloading,
    Verifying,
    Extracting,
    Installing,
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub stage: Stage,
    pub received: u64,
    pub total: Option<u64>,
}

/// Messages emitted by [`spawn_install`] while it works.
pub enum Message {
    Progress(Progress),
    /// `Ok` carries the executable that should be relaunched.
    Finished(Result<PathBuf, String>),
}

/// Asks GitHub for the newest release on a background thread.
pub fn spawn_check() -> Receiver<CheckOutcome> {
    let (tx, rx) = bounded(1);
    thread::spawn(move || {
        let _ = tx.send(check());
    });
    rx
}

/// Downloads `version` for this platform and installs it over the running copy
/// on a background thread. `Ok` carries the executable to relaunch once the
/// caller has released its instance lock.
pub fn spawn_install(version: String) -> Receiver<Message> {
    let (tx, rx) = bounded(64);
    thread::spawn(move || {
        let result = install(&version, &tx);
        let _ = tx.send(Message::Finished(result));
    });
    rx
}

/// Relaunches `executable` as a detached process.
pub fn restart(executable: &Path) {
    let _ = Command::new(executable)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Asks GitHub for the newest release synchronously. Exposed for diagnostics.
pub fn check() -> CheckOutcome {
    let url = format!("https://api.github.com/repos/{REPOSITORY_SLUG}/releases/latest");
    let Some(body) = curl_text(&url, 15) else {
        return CheckOutcome::Failed("latest release is unreachable".into());
    };
    let version = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| {
            value["tag_name"]
                .as_str()
                .map(|tag| tag.trim_start_matches('v').to_owned())
        });
    match version {
        Some(version) if is_newer(&version, CURRENT_VERSION) => CheckOutcome::Available(version),
        Some(_) => CheckOutcome::UpToDate,
        None => CheckOutcome::Failed("latest release response was not understood".into()),
    }
}

fn install(version: &str, tx: &Sender<Message>) -> Result<PathBuf, String> {
    let target = target_triple();
    if target == "unknown" {
        return Err("this platform has no published update".into());
    }
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let current = current.canonicalize().unwrap_or(current);
    let bundle = app_bundle(&current);

    let directory = std::env::temp_dir().join(format!(
        "agent-status-indicator-update-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;

    let outcome = (|| {
        let asset = match &bundle {
            Some(_) => format!("{EXECUTABLE}-{target}.app.tar.gz"),
            None => asset_name(target),
        };
        let archive = directory.join(&asset);
        let url =
            format!("https://github.com/{REPOSITORY_SLUG}/releases/download/v{version}/{asset}");

        report(tx, Stage::Downloading, 0, None);
        download(&url, &archive, tx)?;
        verify(&url, &archive, tx)?;
        report(tx, Stage::Extracting, 0, None);
        extract(&archive, &directory)?;

        match bundle {
            Some(bundle) => install_bundle(&bundle, &directory, tx),
            None => install_binary(&current, &directory, tx),
        }
    })();

    let _ = fs::remove_dir_all(&directory);
    outcome
}

fn report(tx: &Sender<Message>, stage: Stage, received: u64, total: Option<u64>) {
    let _ = tx.try_send(Message::Progress(Progress {
        stage,
        received,
        total,
    }));
}

fn download(url: &str, destination: &Path, tx: &Sender<Message>) -> Result<(), String> {
    let total = remote_size(url);
    let mut child = Command::new("curl")
        .args(["-fL", "--retry", "3", "--retry-delay", "2", "-sS", "-o"])
        .arg(destination)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("curl is unavailable: {error}"))?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(format!("download failed ({status})"));
                }
                let received = file_size(destination);
                report(tx, Stage::Downloading, received, total);
                return Ok(());
            }
            Ok(None) => {
                report(tx, Stage::Downloading, file_size(destination), total);
                thread::sleep(Duration::from_millis(120));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn verify(url: &str, archive: &Path, tx: &Sender<Message>) -> Result<(), String> {
    report(tx, Stage::Verifying, 0, None);
    // Releases publish a bare-hex sidecar next to every asset. A missing file
    // is treated as "verification unavailable" rather than a hard failure, the
    // same way the install script behaves.
    let Some(expected) = curl_text(&format!("{url}.sha256"), 20) else {
        return Ok(());
    };
    let expected = expected
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    if expected.is_empty() {
        return Ok(());
    }
    let Some(actual) = sha256(archive) else {
        return Ok(());
    };
    if actual == expected {
        Ok(())
    } else {
        Err("SHA256 mismatch".into())
    }
}

fn extract(archive: &Path, directory: &Path) -> Result<(), String> {
    let status = Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(directory)
        .status()
        .map_err(|error| format!("tar is unavailable: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("extracting the update failed ({status})"))
    }
}

fn install_bundle(
    bundle: &Path,
    directory: &Path,
    tx: &Sender<Message>,
) -> Result<PathBuf, String> {
    report(tx, Stage::Installing, 0, None);
    let replacement = directory.join(APP_BUNDLE);
    if !replacement.is_dir() {
        return Err("the downloaded archive has no app bundle".into());
    }
    let name = bundle
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(APP_BUNDLE);
    let backup = bundle.with_file_name(format!("{name}.old-{}", std::process::id()));
    let _ = fs::remove_dir_all(&backup);
    fs::rename(bundle, &backup)
        .map_err(|error| format!("cannot replace the app bundle: {error}"))?;
    if let Err(error) = copy_tree(&replacement, bundle) {
        // Put the old bundle back so a failed update never leaves the user
        // without a working app.
        let _ = fs::rename(&backup, bundle);
        return Err(error);
    }
    let _ = fs::remove_dir_all(&backup);
    Ok(bundle.join("Contents/MacOS/AgentStatusIndicator"))
}

fn install_binary(
    current: &Path,
    directory: &Path,
    tx: &Sender<Message>,
) -> Result<PathBuf, String> {
    report(tx, Stage::Installing, 0, None);
    let name = executable_name();
    let source = find_file(directory, name, 2)
        .ok_or_else(|| format!("the downloaded archive has no {name}"))?;
    let parent = current
        .parent()
        .ok_or_else(|| "the executable has no parent directory".to_owned())?;
    let staging = parent.join(format!(".{name}.new-{}", std::process::id()));
    fs::copy(&source, &staging)
        .map_err(|error| format!("cannot write {}: {error}", staging.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&staging, fs::Permissions::from_mode(0o755));
    }
    #[cfg(target_os = "windows")]
    {
        // Windows refuses to overwrite a running executable, so move it aside
        // first; the newly written copy takes its place.
        let backup = current.with_extension("old");
        let _ = fs::remove_file(&backup);
        fs::rename(current, &backup)
            .map_err(|error| format!("cannot replace the running executable: {error}"))?;
        fs::rename(&staging, current).map_err(|error| error.to_string())?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        fs::rename(&staging, current)
            .map_err(|error| format!("cannot replace {}: {error}", current.display()))?;
    }
    Ok(current.to_path_buf())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    if let Ok(status) = Command::new("ditto").arg(source).arg(destination).status() {
        if status.success() {
            return Ok(());
        }
    }
    let status = Command::new("cp")
        .arg("-R")
        .arg(source)
        .arg(destination)
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("copying the app bundle failed ({status})"))
    }
}

fn find_file(directory: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let candidate = directory.join(name);
    if candidate.is_file() {
        return Some(candidate);
    }
    if depth == 0 {
        return None;
    }
    for entry in fs::read_dir(directory).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name, depth - 1) {
                return Some(found);
            }
        }
    }
    None
}

fn file_size(path: &Path) -> u64 {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn remote_size(url: &str) -> Option<u64> {
    let output = Command::new("curl")
        .args(["-fsIL", "--max-time", "20", url])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // A redirect chain prints one header block per hop; the final
    // Content-Length is the asset itself, not the empty redirect body.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .filter_map(|(_, value)| value.trim().parse::<u64>().ok())
        .next_back()
}

fn curl_text(url: &str, timeout: u64) -> Option<String> {
    let output = Command::new("curl")
        .args(["-fsSL", "--max-time", &timeout.to_string()])
        .arg(url)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        None
    }
}

#[cfg(target_os = "windows")]
fn sha256(path: &Path) -> Option<String> {
    let output = Command::new("certutil")
        .args(["-hashfile"])
        .arg(path)
        .arg("SHA256")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .map(str::trim)
        .find(|line| line.len() == 64 && line.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_lowercase)
}

#[cfg(not(target_os = "windows"))]
fn sha256(path: &Path) -> Option<String> {
    for (program, prefix) in [("shasum", ["-a", "256"].as_slice()), ("sha256sum", &[][..])] {
        let Ok(output) = Command::new(program).args(prefix).arg(path).output() else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        return text.split_whitespace().next().map(str::to_lowercase);
    }
    None
}

#[cfg(target_os = "macos")]
fn app_bundle(executable: &Path) -> Option<PathBuf> {
    // .../AgentStatusIndicator.app/Contents/MacOS/agent-status-indicator
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    if macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app"
    {
        return Some(bundle.to_path_buf());
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn app_bundle(_: &Path) -> Option<PathBuf> {
    None
}

pub fn target_triple() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unknown"
    }
}

fn asset_name(target: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{EXECUTABLE}-{target}.zip")
    } else {
        format!("{EXECUTABLE}-{target}.tar.gz")
    }
}

fn executable_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "agent-status-indicator.exe"
    } else {
        EXECUTABLE
    }
}

/// Numeric comparison good enough for `x.y.z` release tags. Pre-release
/// suffixes are ignored because `releases/latest` never returns them.
fn is_newer(candidate: &str, current: &str) -> bool {
    fn parts(value: &str) -> Vec<u64> {
        value
            .trim_start_matches('v')
            .split(['.', '-', '+'])
            .map_while(|part| part.parse::<u64>().ok())
            .collect()
    }
    let candidate = parts(candidate);
    let current = parts(current);
    let length = candidate.len().max(current.len());
    for index in 0..length {
        let left = candidate.get(index).copied().unwrap_or(0);
        let right = current.get(index).copied().unwrap_or(0);
        if left != right {
            return left > right;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_release_versions_numerically() {
        assert!(is_newer("0.2.22", "0.2.21"));
        assert!(is_newer("0.3.0", "0.2.99"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.2.21", "0.2.21.0"));
        assert!(!is_newer("0.2.21", "0.2.21"));
        assert!(!is_newer("0.2.20", "0.2.21"));
        assert!(!is_newer("0.2.9", "0.2.10"));
    }

    #[test]
    fn strips_the_version_prefix() {
        assert!(is_newer("v0.2.22", "v0.2.21"));
    }

    #[test]
    fn finds_nested_archived_binaries() {
        let root = std::env::temp_dir().join(format!("asi-find-{}", std::process::id()));
        let nested = root.join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("agent-status-indicator"), b"bin").unwrap();
        assert_eq!(
            find_file(&root, "agent-status-indicator", 2),
            Some(nested.join("agent-status-indicator"))
        );
        assert_eq!(find_file(&root, "missing", 2), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn swaps_in_a_downloaded_binary() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("asi-install-{}", std::process::id()));
        let extracted = root.join("extract");
        std::fs::create_dir_all(&extracted).unwrap();
        let name = executable_name();
        std::fs::write(extracted.join(name), b"new").unwrap();
        let current = root.join(name);
        std::fs::write(&current, b"old").unwrap();

        let (tx, _rx) = bounded(8);
        let installed = install_binary(&current, &extracted, &tx).unwrap();
        assert_eq!(installed, current);
        assert_eq!(std::fs::read(&current).unwrap(), b"new");
        assert_eq!(
            std::fs::metadata(&current).unwrap().permissions().mode() & 0o111,
            0o111
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn detects_the_macos_app_bundle() {
        assert_eq!(
            app_bundle(Path::new(
                "/Applications/AgentStatusIndicator.app/Contents/MacOS/AgentStatusIndicator"
            )),
            Some(PathBuf::from("/Applications/AgentStatusIndicator.app"))
        );
        assert_eq!(
            app_bundle(Path::new("/usr/local/bin/agent-status-indicator")),
            None
        );
    }
}
