//! The one runtime dependency the Linux build cannot ship itself: 「打开会话」and
//! 「打开链接」end in `xdg-open`, and `xdg-mime` decides which browser a link
//! opens in. `scripts/install.sh` only prints the `apt install xdg-utils` hint
//! because an installer should not run sudo behind the caller's back; the tray
//! does have a user in front of it, so the same check becomes a menu row that
//! installs the package (and uninstalls it again) through the distro's own
//! package manager.

// The menu row only exists in the Linux build, but the module compiles
// everywhere so its command building and simulation parsing stay under test on
// every CI host.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use crate::i18n;
use std::{
    env,
    path::PathBuf,
    process::{Command, Stdio},
};

/// The two commands the app actually calls. Both come from `xdg-utils`, so the
/// row is only satisfied when both resolve; a half-installed package would let
/// the hint disappear while session opening still silently degrades.
const COMMANDS: [&str; 2] = ["xdg-open", "xdg-mime"];
const PACKAGE: &str = "xdg-utils";

/// The distros whose package manager this row knows how to drive. Detection is
/// by binary, not by `/etc/os-release`: a Fedora container on a Debian host is
/// still dnf's job.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Distro {
    Debian,
    Fedora,
    Arch,
    Suse,
}

impl Distro {
    fn program(self) -> &'static str {
        match self {
            Distro::Debian => "apt-get",
            Distro::Fedora => "dnf",
            Distro::Arch => "pacman",
            Distro::Suse => "zypper",
        }
    }

    /// `-y` / `--noconfirm` everywhere: the command runs detached from a menu
    /// click, so nothing may wait for a terminal that is not there.
    fn args(self, install: bool) -> Vec<&'static str> {
        match (self, install) {
            (Distro::Debian, true) => vec!["install", "-y", PACKAGE],
            (Distro::Debian, false) => vec!["remove", "-y", PACKAGE],
            (Distro::Fedora, true) => vec!["install", "-y", PACKAGE],
            (Distro::Fedora, false) => vec!["remove", "-y", PACKAGE],
            (Distro::Arch, true) => vec!["-S", "--noconfirm", PACKAGE],
            (Distro::Arch, false) => vec!["-R", "--noconfirm", PACKAGE],
            (Distro::Suse, true) => vec!["--non-interactive", "install", PACKAGE],
            (Distro::Suse, false) => vec!["--non-interactive", "remove", PACKAGE],
        }
    }

    /// The removal is simulated first where the simulator is trustworthy, so a
    /// click never takes the desktop down with the dependency: on Debian
    /// `apt-get remove xdg-utils` also removes everything that depends on it
    /// (xfce4, xdg-desktop-portal-gtk, several file managers), and `-y` would
    /// not ask.
    fn simulate_removal(self) -> Option<Vec<&'static str>> {
        match self {
            Distro::Debian => Some(vec!["--simulate", "remove", "-y", PACKAGE]),
            Distro::Arch => Some(vec!["-R", "--print", "--print-format", "%n", PACKAGE]),
            Distro::Fedora | Distro::Suse => None,
        }
    }
}

/// Why the row could not do its job. Every arm carries the text the notice box
/// shows, because the caller has no terminal to fall back on.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// No apt/dnf/pacman/zypper on this system.
    NoPackageManager,
    /// Found a package manager, but no way to become root without a prompt
    /// this process cannot answer.
    NoElevation { command: String },
    /// The simulated removal would take other packages down with it.
    RemovesOthers {
        command: String,
        packages: Vec<String>,
    },
    /// The package manager itself refused or failed.
    Command { detail: String },
}

/// True when both commands the app calls resolve on `PATH`.
pub fn is_installed() -> bool {
    COMMANDS.iter().all(|command| which(command).is_some())
}

pub fn action_label(installed: bool) -> &'static str {
    if installed {
        i18n::text("uninstall_xdg_utils")
    } else {
        i18n::text("install_xdg_utils")
    }
}

pub fn install() -> Result<(), Failure> {
    let distro = detect().ok_or(Failure::NoPackageManager)?;
    run(distro, true)
}

pub fn uninstall() -> Result<(), Failure> {
    let distro = detect().ok_or(Failure::NoPackageManager)?;
    if let Some(packages) = spills_over(distro)? {
        return Err(Failure::RemovesOthers {
            command: describe(distro, false),
            packages,
        });
    }
    run(distro, false)
}

/// The package manager this machine has, in the order people install them.
fn detect() -> Option<Distro> {
    [Distro::Debian, Distro::Fedora, Distro::Arch, Distro::Suse]
        .into_iter()
        .find(|distro| which(distro.program()).is_some())
}

/// The packages a removal would take down with it, when the distro can be
/// asked cheaply. Empty means "only xdg-utils", `None` means "not asked".
fn spills_over(distro: Distro) -> Result<Option<Vec<String>>, Failure> {
    if distro.simulate_removal().is_none() {
        return Ok(None);
    }
    let output = Command::new(distro.program())
        .args(distro.simulate_removal().unwrap_or_default())
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    let Ok(output) = output else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }
    let packages = match distro {
        // apt prints one `Remv <name> [<version>]` line per removal.
        Distro::Debian => removed_by_apt(&String::from_utf8_lossy(&output.stdout)),
        // pacman --print --print-format %n prints bare names.
        Distro::Arch => String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
        Distro::Fedora | Distro::Suse => Vec::new(),
    };
    let others: Vec<String> = packages
        .into_iter()
        .filter(|name| name != PACKAGE)
        .collect();
    Ok((!others.is_empty()).then_some(others))
}

fn removed_by_apt(simulation: &str) -> Vec<String> {
    simulation
        .lines()
        .filter_map(|line| line.strip_prefix("Remv "))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(|name| name.split(':').next().unwrap_or(name).to_owned())
        .collect()
}

fn run(distro: Distro, install: bool) -> Result<(), Failure> {
    let mut last_detail = String::new();
    for attempt in elevators() {
        let mut command = Command::new(&attempt.program);
        command
            .args(&attempt.prefix)
            .arg(distro.program())
            .args(distro.args(install))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let Ok(output) = command.output() else {
            continue;
        };
        if output.status.success() {
            return Ok(());
        }
        let detail = tail(&String::from_utf8_lossy(&output.stderr));
        // A refused authorisation is not a package manager failure: try the
        // next elevator before telling the user to open a terminal.
        if detail.contains("password is required")
            || detail.contains("not authorized")
            || detail.contains("Error executing command as another user")
        {
            last_detail = detail;
            continue;
        }
        return Err(Failure::Command { detail });
    }
    if last_detail.is_empty() {
        return Err(Failure::NoElevation {
            command: describe(distro, install),
        });
    }
    Err(Failure::Command {
        detail: last_detail,
    })
}

struct Elevator {
    program: String,
    prefix: Vec<&'static str>,
}

/// pkexec first: a GUI session has a polkit agent that can ask for the
/// password. `sudo -n` is the fallback for machines where the user is already
/// allowed to run the package manager without a prompt.
fn elevators() -> Vec<Elevator> {
    let mut elevators = Vec::new();
    if let Some(pkexec) = which("pkexec") {
        elevators.push(Elevator {
            program: pkexec.to_string_lossy().into_owned(),
            prefix: vec![],
        });
    }
    if which("sudo").is_some() {
        elevators.push(Elevator {
            program: "sudo".into(),
            prefix: vec!["-n"],
        });
    }
    elevators
}

/// The line a user can paste into a terminal when the row cannot do it alone.
fn describe(distro: Distro, install: bool) -> String {
    let mut parts = vec![String::from("sudo"), distro.program().to_owned()];
    parts.extend(distro.args(install).into_iter().map(str::to_owned));
    parts.join(" ")
}

/// The body of the notice box a [`Failure`] earns. Every arm ends with the
/// line the user can paste, so a failed click still leaves them a way out.
pub fn failure_text(failure: &Failure) -> String {
    const LISTED: usize = 5;
    match failure {
        Failure::NoPackageManager => i18n::text("xdg_no_package_manager").to_owned(),
        Failure::NoElevation { command } => {
            format!("{}\n\n{command}", i18n::text("xdg_needs_admin"))
        }
        Failure::RemovesOthers { command, packages } => {
            let mut listed: Vec<&str> = packages.iter().take(LISTED).map(String::as_str).collect();
            if packages.len() > LISTED {
                listed.push("…");
            }
            format!(
                "{}\n\n{command}\n\n{}",
                i18n::text("xdg_removes_others"),
                listed.join(" ")
            )
        }
        Failure::Command { detail } => {
            format!("{}\n\n{detail}", i18n::text("xdg_command_failed"))
        }
    }
}

fn tail(text: &str) -> String {
    let trimmed = text.trim();
    let mut lines: Vec<&str> = trimmed.lines().filter(|line| !line.is_empty()).collect();
    const KEEP: usize = 4;
    if lines.len() > KEEP {
        lines = lines.split_off(lines.len() - KEEP);
    }
    lines.join("\n")
}

fn which(program: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_distro_names_both_directions_of_the_package() {
        for distro in [Distro::Debian, Distro::Fedora, Distro::Arch, Distro::Suse] {
            for install in [true, false] {
                let args = distro.args(install);
                assert!(args.contains(&PACKAGE), "{distro:?} {install}: {args:?}");
                assert!(
                    args.iter().any(|arg| arg.starts_with('-')),
                    "{distro:?} {install} must not stop for a prompt"
                );
            }
        }
    }

    #[test]
    fn apt_simulation_lines_yield_bare_package_names() {
        let simulation = "NOTE: This is only a simulation!\n\
Remv xdg-utils [1.1.3-4.1ubuntu3]\n\
Remv xfce4 [4.18.1]\n\
Inst foo (1.0 bar [amd64])\n";
        assert_eq!(removed_by_apt(simulation), vec!["xdg-utils", "xfce4"]);
    }

    #[test]
    fn apt_architecture_qualified_names_are_reduced() {
        assert_eq!(
            removed_by_apt("Remv xdg-utils:all [1.1.3]\n"),
            vec!["xdg-utils"]
        );
    }

    #[test]
    fn the_manual_command_is_pasteable() {
        let command = describe(Distro::Debian, true);
        assert_eq!(command, "sudo apt-get install -y xdg-utils");
        assert_eq!(
            describe(Distro::Arch, false),
            "sudo pacman -R --noconfirm xdg-utils"
        );
    }

    #[test]
    fn failures_keep_only_the_tail_of_the_output() {
        let text = "one\ntwo\nthree\nfour\nfive\nsix\n";
        assert_eq!(tail(text), "three\nfour\nfive\nsix");
        assert_eq!(tail("  \n only \n"), "only");
    }

    #[test]
    fn only_apt_and_pacman_are_simulated() {
        assert!(Distro::Debian.simulate_removal().is_some());
        assert!(Distro::Arch.simulate_removal().is_some());
        assert!(Distro::Fedora.simulate_removal().is_none());
        assert!(Distro::Suse.simulate_removal().is_none());
    }
}
