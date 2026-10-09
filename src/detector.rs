use crate::codex_state::CodexTitles;
#[cfg(target_os = "macos")]
use crate::macos_process::{MacProcessSource, ProcessMetadata, ProcessRecord};
use crate::model::{AgentInstance, AgentState};
use crate::session::{SessionAnalyzer, SessionFacts};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};
#[cfg(not(target_os = "macos"))]
use sysinfo::ProcessesToUpdate;
#[cfg(not(target_os = "macos"))]
use sysinfo::{Pid, Process, System};

pub struct Detector {
    #[cfg(not(target_os = "macos"))]
    system: System,
    #[cfg(target_os = "macos")]
    macos_processes: MacProcessSource,
    sessions: SessionAnalyzer,
    deepseek: crate::deepseek::DeepSeekAnalyzer,
    deepseek_desktop: crate::deepseek_desktop::DeepSeekDesktopAnalyzer,
    opencode: crate::opencode::OpenCodeAnalyzer,
    pi: crate::pi::PiAnalyzer,
    terminal: crate::terminal::TerminalProbe,
    /// How long a finished hosted conversation keeps its row; `None` keeps it
    /// forever. Set from the config and updated when the menu changes it.
    conversation_window: Option<Duration>,
    #[cfg(target_os = "macos")]
    codex_titles: CodexTitles,
    #[cfg(target_os = "macos")]
    web_urls: crate::web::WebUrlDetector,
}

impl Detector {
    pub fn new() -> Self {
        Self {
            #[cfg(not(target_os = "macos"))]
            system: System::new_all(),
            #[cfg(target_os = "macos")]
            macos_processes: MacProcessSource::default(),
            sessions: SessionAnalyzer::default(),
            deepseek: crate::deepseek::DeepSeekAnalyzer::default(),
            deepseek_desktop: crate::deepseek_desktop::DeepSeekDesktopAnalyzer::default(),
            opencode: crate::opencode::OpenCodeAnalyzer::default(),
            pi: crate::pi::PiAnalyzer::default(),
            terminal: crate::terminal::TerminalProbe::default(),
            // Read the saved setting so a restart (and `--diagnose`) reports the
            // same window the menu shows.
            conversation_window: crate::config::Config::load().conversation_window_duration(),
            #[cfg(target_os = "macos")]
            codex_titles: CodexTitles::default(),
            #[cfg(target_os = "macos")]
            web_urls: crate::web::WebUrlDetector::default(),
        }
    }

    pub fn set_conversation_window(&mut self, window: Option<Duration>) {
        self.conversation_window = window;
    }

    /// The live conversations of the DeepSeek Harness desktop application, most
    /// actionable first, plus how many the row cap left out.
    ///
    /// `home` is the profile the running desktop host reported, so a
    /// non-default `DSH_HOME` reads the same profile the window shows.
    pub fn deepseek_desktop_overview(
        &mut self,
        home: Option<&Path>,
    ) -> (
        Vec<crate::deepseek_desktop::DesktopSession>,
        usize,
        Option<crate::deepseek_desktop::ProfileAlert>,
    ) {
        // An explicit `DSH_HOME` is a deliberate override and wins over a
        // running host's profile, so the tray and the diagnostic agree.
        let from_env = crate::deepseek_desktop::env_home();
        let home = from_env.as_deref().or(home);
        self.deepseek_desktop.refresh(home);
        let (sessions, hidden) = self.deepseek_desktop.overview(self.conversation_window);
        (sessions, hidden, self.deepseek_desktop.alert())
    }

    pub fn scan(&mut self) -> Vec<AgentInstance> {
        #[cfg(target_os = "macos")]
        {
            return self.scan_macos();
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.scan_sysinfo()
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn scan_sysinfo(&mut self) -> Vec<AgentInstance> {
        self.system.refresh_processes(ProcessesToUpdate::All, true);
        let roots: Vec<_> = self
            .system
            .processes()
            .iter()
            .filter_map(|(pid, process)| agent_kind(process).map(|kind| (*pid, process, kind)))
            .filter(|(_, process, kind)| !has_agent_parent(process, kind, &self.system))
            .collect();
        // Snapshot the desktop host's identity before `roots` is consumed by the
        // scan below: these rows are built afterwards, from the session cache.
        let desktop_hosts: Vec<(u32, Duration, Option<PathBuf>)> = roots
            .iter()
            .filter(|(_, process, kind)| *kind == "deepseek" && is_desktop_host(process))
            .map(|(_, process, _)| {
                let command = process
                    .cmd()
                    .iter()
                    .map(|value| value.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(" ");
                (
                    process.pid().as_u32(),
                    Duration::from_secs(process.run_time()),
                    crate::deepseek_desktop::home_from_command(&command),
                )
            })
            .collect();
        let mut instances: Vec<_> = roots
            .into_iter()
            .map(|(pid, process, kind)| {
                let cwd = process.cwd().map(PathBuf::from);
                let active = has_task_descendant(pid, kind, &self.system);
                let mut instance = AgentInstance {
                    key: pid.as_u32().to_string(),
                    kind: display_name(kind).into(),
                    label: cwd
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .and_then(|s| s.to_str())
                        .map(|p| format!("{} ({p})", display_name(kind)))
                        .unwrap_or_else(|| display_name(kind).into()),
                    pid: pid.as_u32(),
                    cwd,
                    state: if active {
                        AgentState::Working
                    } else {
                        AgentState::Ready
                    },
                    uptime: Duration::from_secs(process.run_time()),
                    model: None,
                    context: None,
                    open_url: None,
                    automatic_confirmation_mode: false,
                    informational: false,
                };
                if kind == "claude" {
                    enrich_claude(&mut instance, &mut self.sessions);
                } else if kind == "codex" {
                    enrich_codex(
                        &mut instance,
                        &mut self.sessions,
                        active,
                        codex_resume_session_id(process),
                        &mut self.terminal,
                    );
                } else if kind == "deepseek" {
                    enrich_deepseek(&mut instance, &mut self.deepseek);
                } else if kind == "opencode" {
                    enrich_opencode(&mut instance, &mut self.opencode);
                }
                instance
            })
            .collect();
        // The desktop application hosts every conversation it has open in one
        // process, so its rows come from the profile's session cache instead of
        // from the process tree.
        for (pid, uptime, home) in desktop_hosts {
            instances.retain(|instance| instance.pid != pid);
            let (sessions, hidden, alert) = self.deepseek_desktop_overview(home.as_deref());
            instances.extend(desktop_rows(pid, uptime, &sessions));
            instances.extend(desktop_overflow_row(pid, hidden));
            instances.extend(desktop_alert_row(pid, alert));
        }
        enrich_pi_instances(&mut instances, &mut self.pi);
        for kind in supported_kinds() {
            if !instances
                .iter()
                .any(|instance| instance.kind == display_name(kind))
            {
                instances.push(stopped_instance(kind));
            }
        }
        instances.sort_by_key(|instance| kind_order(&instance.kind));
        instances
    }

    #[cfg(target_os = "macos")]
    fn scan_macos(&mut self) -> Vec<AgentInstance> {
        let processes = self.macos_processes.processes();
        // The desktop application is one process tree that owns every
        // conversation it has open, so it is detected first and reported from
        // the profile's session cache rather than from the process tree: one
        // row per live conversation, all pointing at the application window.
        let desktop_hosts = desktop_roots(&processes);
        let desktop_groups: Vec<Vec<u32>> = desktop_hosts
            .iter()
            .map(|(process, _)| process_tree_pids(process.pid, &processes))
            .collect();
        let roots: Vec<_> = processes
            .iter()
            .filter_map(|process| {
                let kind = process_kind(process)?;
                let host = host_application_for(process, &processes);
                if host.is_none() && is_host_integration(process) {
                    return None;
                }
                Some((process, kind, host))
            })
            .filter(|(process, kind, _)| !has_process_agent_parent(process, kind, &processes))
            .filter(|(process, _, _)| {
                !desktop_groups
                    .iter()
                    .any(|group| group.contains(&process.pid))
            })
            .collect();
        let tracked_pids: Vec<_> = roots
            .iter()
            .flat_map(|(root, _, _)| process_tree_pids(root.pid, &processes))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let _ = self.macos_processes.metadata_for(&tracked_pids);
        let codex_pids: Vec<_> = roots
            .iter()
            .filter(|(_, kind, _)| *kind == "codex")
            .flat_map(|(root, _, _)| process_tree_pids(root.pid, &processes))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        self.macos_processes.refresh_codex_rollouts(&codex_pids);
        let metadata = self.macos_processes.metadata_for(&tracked_pids);
        let now = SystemTime::now();
        let window = self.conversation_window;
        let mut instances: Vec<_> = roots
            .into_iter()
            .flat_map(|(process, kind, host)| {
                let group_pids = process_tree_pids(process.pid, &processes);
                let group_metadata = group_pids
                    .iter()
                    .filter_map(|pid| metadata.get(pid))
                    .collect::<Vec<_>>();
                // A GUI application drives every conversation it has open
                // through one app-server process. Report the recently active
                // conversations instead of collapsing them into a single row.
                if kind == "codex" {
                    if let Some(host) = host {
                        let conversations = codex_conversations(
                            &mut self.sessions,
                            codex_rollouts_from_metadata(&group_metadata),
                            now,
                            window,
                        );
                        // With no conversation inside the configured range,
                        // report the application itself: reusing an old
                        // conversation would show the user a session they have
                        // moved on from as if it were live.
                        return if conversations.is_empty() {
                            vec![hosted_application_instance(process, host.display)]
                        } else {
                            hosted_codex_instances(
                                process,
                                host.display,
                                &mut self.codex_titles,
                                conversations,
                            )
                        };
                    }
                }
                let cwd = group_metadata.iter().find_map(|entry| entry.cwd.clone());
                let display = host.map_or_else(|| display_name(kind), |host| host.display);
                // A GUI host keeps its helper processes alive permanently, so
                // descendant activity says nothing about the embedded agent;
                // the rollout it writes is the source of truth instead.
                let active =
                    host.is_none() && has_active_process_descendant(process.pid, kind, &processes);
                let mut instance = AgentInstance {
                    key: process.pid.to_string(),
                    kind: display.into(),
                    label: cwd
                        .as_ref()
                        .and_then(|path| path.file_name())
                        .and_then(|name| name.to_str())
                        .map(|project| format!("{display} ({project})"))
                        .unwrap_or_else(|| display.into()),
                    pid: process.pid,
                    cwd,
                    state: if active {
                        AgentState::Working
                    } else {
                        AgentState::Ready
                    },
                    uptime: process.uptime,
                    model: None,
                    context: None,
                    open_url: if kind == "deepseek" {
                        self.web_urls.discover(process.pid, &group_pids)
                    } else {
                        None
                    },
                    automatic_confirmation_mode: false,
                    informational: false,
                };
                match kind {
                    "claude" => enrich_claude(&mut instance, &mut self.sessions),
                    "codex" => {
                        let resumed = codex_resume_session_id_from_command(&process.command);
                        let rollout = enrich_macos_codex(
                            &mut instance,
                            &mut self.sessions,
                            active,
                            codex_rollouts_from_metadata(&group_metadata),
                            &mut self.terminal,
                            host.is_none(),
                            resumed.as_deref(),
                        );
                        // A hosted session lives inside the application's own
                        // conversation view, so clicking must open that thread
                        // instead of only activating the application.
                        if host.is_some() {
                            instance.open_url = rollout.as_deref().and_then(codex_thread_url);
                        }
                    }
                    // The desktop application's whole process tree is excluded
                    // from `roots`, so a deepseek row here is always a terminal
                    // session bound to its project's session logs.
                    "deepseek" => enrich_deepseek(&mut instance, &mut self.deepseek),
                    "opencode" => enrich_opencode(&mut instance, &mut self.opencode),
                    _ => {}
                }
                vec![instance]
            })
            .collect();
        for (process, home) in desktop_hosts {
            let (sessions, hidden, alert) = self.deepseek_desktop_overview(home.as_deref());
            instances.extend(desktop_rows(process.pid, process.uptime, &sessions));
            instances.extend(desktop_overflow_row(process.pid, hidden));
            instances.extend(desktop_alert_row(process.pid, alert));
        }
        enrich_pi_instances(&mut instances, &mut self.pi);
        for kind in supported_kinds() {
            if !instances
                .iter()
                .any(|instance| instance.kind == display_name(kind))
            {
                instances.push(stopped_instance(kind));
            }
        }
        instances.sort_by_key(|instance| kind_order(&instance.kind));
        instances
    }
}

fn stopped_instance(kind: &str) -> AgentInstance {
    AgentInstance {
        key: format!("stopped:{}", display_name(kind)),
        kind: display_name(kind).into(),
        label: display_name(kind).into(),
        pid: 0,
        cwd: None,
        state: AgentState::Stopped,
        uptime: Duration::ZERO,
        model: None,
        context: None,
        open_url: None,
        automatic_confirmation_mode: false,
        informational: false,
    }
}

/// One row per conversation the DeepSeek Harness desktop application has open,
/// taken from the profile's session cache. Every row carries the host's pid, so
/// clicking it activates the application window; the conversation id keeps the
/// rows (and their notification state) apart.
///
/// With no conversation inside the configured range the application still gets
/// a row: the user has the window open, and "ready" is the truth about it.
fn desktop_rows(
    pid: u32,
    uptime: Duration,
    sessions: &[crate::deepseek_desktop::DesktopSession],
) -> Vec<AgentInstance> {
    let display = display_name("deepseek");
    if sessions.is_empty() {
        return vec![AgentInstance {
            key: format!("desktop:{pid}"),
            kind: display.into(),
            label: display.into(),
            pid,
            cwd: None,
            state: AgentState::Ready,
            uptime,
            model: None,
            context: None,
            open_url: None,
            automatic_confirmation_mode: false,
            informational: false,
        }];
    }
    sessions
        .iter()
        .map(|session| AgentInstance {
            key: format!("desktop:{}", session.id),
            kind: display.into(),
            label: desktop_label(session),
            pid,
            cwd: session.cwd.clone(),
            state: session.state,
            uptime: session
                .created
                .and_then(|created| SystemTime::now().duration_since(created).ok())
                .unwrap_or(uptime),
            model: session.model.clone(),
            context: session.context.clone(),
            // The desktop app has no per-conversation deep link yet, so clicking
            // a row brings its window forward.
            open_url: None,
            automatic_confirmation_mode: session.automatic_confirmation_mode,
            informational: false,
        })
        .collect()
}

/// A plain-text row reporting the conversations the row cap left out.
///
/// Without it the cap would silently hide work: the list ends at
/// [`crate::deepseek_desktop::MAX_DESKTOP_ROWS`] with no hint that more
/// conversations are live, so a user watching a session that fell off the end
/// would see nothing. It is not clickable and carries no session, which is why
/// it is marked informational and skipped by the summary and notifications.
fn desktop_overflow_row(pid: u32, hidden: usize) -> Option<AgentInstance> {
    (hidden > 0).then(|| AgentInstance {
        key: format!("desktop:{pid}:more"),
        kind: display_name("deepseek").into(),
        label: crate::i18n::hidden_sessions(hidden),
        pid,
        cwd: None,
        state: AgentState::Ready,
        uptime: Duration::ZERO,
        model: None,
        context: None,
        open_url: None,
        automatic_confirmation_mode: false,
        informational: true,
    })
}

/// A row stating that the desktop profile no longer matches this build's format.
///
/// Without it a format change is indistinguishable from an idle app: the
/// conversations simply stop appearing. The row is informational, so it is not
/// clickable, does not enter the summary, and never notifies.
fn desktop_alert_row(
    pid: u32,
    alert: Option<crate::deepseek_desktop::ProfileAlert>,
) -> Option<AgentInstance> {
    use crate::deepseek_desktop::AlertKind;
    let alert = alert?;
    let label = match alert.kind {
        AlertKind::ProfileMissing => crate::i18n::profile_missing().to_owned(),
        AlertKind::FormatChanged | AlertKind::Unreadable => {
            crate::i18n::profile_format_changed(alert.affected)
        }
        AlertKind::FailureUndecidable => crate::i18n::failure_undecidable().to_owned(),
    };
    Some(AgentInstance {
        key: format!("desktop:{pid}:alert"),
        kind: display_name("deepseek").into(),
        label,
        pid,
        cwd: None,
        state: AgentState::Ready,
        uptime: Duration::ZERO,
        model: None,
        context: None,
        open_url: None,
        automatic_confirmation_mode: false,
        informational: true,
    })
}

/// `DeepSeek Harness · <title>`, falling back to the project name and then to
/// the application name. The title already carries the project in practice, so
/// repeating both would only shorten the useful part of the row.
fn desktop_label(session: &crate::deepseek_desktop::DesktopSession) -> String {
    let display = display_name("deepseek");
    let detail = session
        .title
        .as_deref()
        .or_else(|| session.cwd.as_deref().and_then(Path::file_name)?.to_str());
    match detail {
        Some(detail) => format!("{display} · {detail}"),
        None => display.into(),
    }
}

/// Whether a process is the DeepSeek Harness desktop host, whose rows come from
/// the profile's session cache rather than from a per-conversation process.
#[cfg(target_os = "macos")]
fn is_desktop_host(process: &ProcessRecord) -> bool {
    desktop_host_from_parts(&process.command, &process.executable)
}

/// The DeepSeek Harness desktop application's root processes: the Electron
/// application (pid, uptime) and the `DSH_HOME` its host runs with.
///
/// The app is one tree — the Electron main process, its helpers, and the
/// Node-mode host that runs the `dsh` runtime — so only the topmost process of
/// the tree is reported, and the whole tree is excluded from the generic scan
/// to avoid reporting the same application once per helper.
#[cfg(target_os = "macos")]
fn desktop_roots(processes: &[ProcessRecord]) -> Vec<(&ProcessRecord, Option<PathBuf>)> {
    let by_pid: HashMap<_, _> = processes
        .iter()
        .map(|process| (process.pid, process))
        .collect();
    processes
        .iter()
        .filter(|process| {
            crate::deepseek_desktop::is_desktop_process(&process.executable, &process.command)
        })
        .filter(|process| {
            // The Node-mode host runs the same executable with the runtime
            // bootstrap on its command line; it is a child of the application,
            // which is the process the tray reports.
            !process.command.contains("dsh-desktop-host")
        })
        .filter(|process| {
            let mut parent = process.ppid;
            let mut seen = HashSet::new();
            while parent != 0 && seen.insert(parent) {
                let Some(candidate) = by_pid.get(&parent) else {
                    break;
                };
                if crate::deepseek_desktop::is_desktop_process(
                    &candidate.executable,
                    &candidate.command,
                ) {
                    return false;
                }
                parent = candidate.ppid;
            }
            true
        })
        .map(|process| {
            let tree = process_tree_pids(process.pid, processes);
            let home = processes
                .iter()
                .filter(|candidate| {
                    tree.contains(&candidate.pid) && candidate.command.contains("dsh-desktop-host")
                })
                .find_map(|candidate| {
                    crate::deepseek_desktop::home_from_command(&candidate.command)
                });
            (process, home)
        })
        .collect()
}

/// The platform-independent half of desktop-host detection, compiled and tested
/// on every platform.
///
/// Each platform adapter only has to produce a command line and an executable
/// path. The non-macOS adapter used to inline the whole judgement, which put
/// that branch's logic out of reach of an Apple Silicon host's compiler — the
/// same reason a borrow error in it reached CI before it was caught here.
pub(crate) fn desktop_host_from_parts(command: &str, path: &str) -> bool {
    crate::deepseek_desktop::is_desktop_process(path, command)
}

#[cfg(not(target_os = "macos"))]
fn is_desktop_host(process: &Process) -> bool {
    let command = process
        .cmd()
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    let path = process
        .exe()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| executable(process));
    desktop_host_from_parts(&command, &path)
}

fn supported_kinds() -> [&'static str; 5] {
    ["claude", "codex", "opencode", "deepseek", "pi"]
}

#[cfg(target_os = "macos")]
fn process_kind(process: &ProcessRecord) -> Option<&'static str> {
    agent_kind_from_executable(&process.executable)
}

/// Subcommands that turn an agent binary into a headless server driven by a
/// host application (the ChatGPT desktop app, IDE extensions) instead of an
/// interactive session the user is driving.
const HOST_SERVER_SUBCOMMANDS: [&str; 2] = ["app-server", "mcp-server"];

fn is_host_server_argument(argument: &str) -> bool {
    HOST_SERVER_SUBCOMMANDS.contains(&argument)
}

/// A macOS application that embeds an agent binary and drives it itself:
/// ChatGPT.app runs its bundled Codex as a headless app-server for the GUI.
/// Sessions it hosts are reported under the application's name, and activating
/// the application is the correct focus action for them.
#[cfg(target_os = "macos")]
#[derive(Debug)]
struct HostApplication {
    bundle: &'static str,
    display: &'static str,
}

#[cfg(target_os = "macos")]
const HOST_APPLICATIONS: [HostApplication; 1] = [HostApplication {
    bundle: "/ChatGPT.app/",
    display: "ChatGPT",
}];

#[cfg(target_os = "macos")]
fn host_application(executable: &str) -> Option<&'static HostApplication> {
    HOST_APPLICATIONS
        .iter()
        .find(|host| executable.contains(host.bundle))
}

/// The host application owning `process`, walking its ancestry. Ancestry rather
/// than only the executable path, because a host can drive a user-installed
/// agent binary (ChatGPT can point its Codex tools at a `codex` outside the
/// bundle).
#[cfg(target_os = "macos")]
fn host_application_for(
    process: &ProcessRecord,
    processes: &[ProcessRecord],
) -> Option<&'static HostApplication> {
    let by_pid: HashMap<_, _> = processes
        .iter()
        .map(|record| (record.pid, record))
        .collect();
    let mut current = Some(process);
    let mut seen = HashSet::new();
    while let Some(record) = current {
        if let Some(host) = host_application(&record.executable) {
            return Some(host);
        }
        if !seen.insert(record.pid) || record.ppid == 0 {
            break;
        }
        current = by_pid.get(&record.ppid).copied();
    }
    None
}

/// The executed program's basename, matched against the known agent binaries.
/// Only the executable is considered: command lines routinely mention agent
/// names (`which pi`) without being that agent.
///
/// The DeepSeek Harness desktop application is matched by its bundle directory
/// instead: its executable is the product name with a space, which no CLI
/// binary is ever called.
#[cfg(target_os = "macos")]
fn agent_kind_from_executable(executable: &str) -> Option<&'static str> {
    if executable.contains(crate::deepseek_desktop::APP_BUNDLE) {
        return Some("deepseek");
    }
    let name = Path::new(executable)
        .file_name()?
        .to_str()?
        .trim_matches('"')
        .to_ascii_lowercase();
    match name.as_str() {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        "opencode" => Some("opencode"),
        "pi" => Some("pi"),
        "dsh" | "deepseek-harness" => Some("deepseek"),
        _ => None,
    }
}

/// Whether a matched agent binary belongs to an application rather than to the
/// user: either a private copy inside a macOS `.app` bundle, or a Codex running
/// as a host server. Only consulted when no known [`HOST_APPLICATIONS`] owner
/// was found, so a host we recognize keeps its own row while unknown helpers
/// stay hidden instead of masquerading as a session.
#[cfg(target_os = "macos")]
fn is_host_integration(process: &ProcessRecord) -> bool {
    if is_bundled_in_application(Path::new(&process.executable)) {
        return true;
    }
    is_codex_executable(&process.executable)
        && process
            .command
            .split_whitespace()
            .any(is_host_server_argument)
}

#[cfg(target_os = "macos")]
fn is_codex_executable(executable: &str) -> bool {
    agent_kind_from_executable(executable) == Some("codex")
}

/// A binary inside an application bundle belongs to that GUI app, not to an
/// agent session the user is driving.
#[cfg(target_os = "macos")]
fn is_bundled_in_application(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| name.ends_with(".app"))
    })
}

#[cfg(target_os = "macos")]
fn has_process_agent_parent(
    process: &ProcessRecord,
    kind: &str,
    processes: &[ProcessRecord],
) -> bool {
    let by_pid: HashMap<_, _> = processes
        .iter()
        .map(|process| (process.pid, process))
        .collect();
    let mut parent = process.ppid;
    let mut seen = HashSet::new();
    while parent != 0 && seen.insert(parent) {
        let Some(candidate) = by_pid.get(&parent) else {
            break;
        };
        if process_kind(candidate) == Some(kind) {
            return true;
        }
        parent = candidate.ppid;
    }
    false
}

#[cfg(target_os = "macos")]
fn process_tree_pids(root: u32, processes: &[ProcessRecord]) -> Vec<u32> {
    let parents: HashMap<_, _> = processes
        .iter()
        .map(|process| (process.pid, process.ppid))
        .collect();
    processes
        .iter()
        .filter(|process| {
            let mut current = process.pid;
            let mut seen = HashSet::new();
            loop {
                if current == root {
                    return true;
                }
                if !seen.insert(current) {
                    return false;
                }
                let Some(parent) = parents.get(&current).copied() else {
                    return false;
                };
                if parent == 0 {
                    return false;
                }
                current = parent;
            }
        })
        .map(|process| process.pid)
        .collect()
}

#[cfg(target_os = "macos")]
fn has_active_process_descendant(root: u32, kind: &str, processes: &[ProcessRecord]) -> bool {
    process_tree_pids(root, processes).into_iter().any(|pid| {
        pid != root
            && processes
                .iter()
                .find(|process| process.pid == pid)
                .is_some_and(|process| {
                    process_kind(process).is_none()
                        && !(kind == "codex" && process.command.contains("codex-code-mode-host"))
                })
    })
}

#[cfg(target_os = "macos")]
fn codex_rollouts_from_metadata(metadata: &[&ProcessMetadata]) -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir().map(|home| home.join(".codex/sessions")) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut rollouts: Vec<_> = metadata
        .iter()
        .flat_map(|entry| entry.files.iter())
        .filter(|path| path.starts_with(&home))
        .filter(|path| crate::session::primary_codex_rollout_cwd(path).is_some())
        .filter(|path| seen.insert((*path).clone()))
        .cloned()
        .collect();
    rollouts.sort_by_key(|path| {
        std::cmp::Reverse(path.metadata().ok().and_then(|entry| entry.modified().ok()))
    });
    rollouts
}

/// The conversations of a GUI host that deserve a row, newest activity first.
#[cfg(target_os = "macos")]
fn codex_conversations(
    analyzer: &mut SessionAnalyzer,
    rollouts: Vec<PathBuf>,
    now: SystemTime,
    window: Option<Duration>,
) -> Vec<(PathBuf, SessionFacts)> {
    let conversations = rollouts
        .into_iter()
        .filter_map(|path| Some((path.clone(), analyzer.analyze_codex_rollout(&path)?)))
        .collect();
    select_conversations(conversations, now, window)
}

/// Keeps the conversations that deserve a row, newest activity first. A
/// conversation with no unfinished turn only earns a row while it has been
/// touched within the configured window: ChatGPT keeps a tab for every
/// historical thread, and listing all of them would bury the menu. An empty
/// result is meaningful: the caller then reports the application itself instead
/// of one of its older conversations.
#[cfg(target_os = "macos")]
fn select_conversations(
    mut conversations: Vec<(PathBuf, SessionFacts)>,
    now: SystemTime,
    window: Option<Duration>,
) -> Vec<(PathBuf, SessionFacts)> {
    conversations.sort_by_key(|(_, facts)| std::cmp::Reverse(facts.activity));

    let active: Vec<_> = conversations
        .iter()
        .filter(|(_, facts)| needs_attention(facts) || touched_recently(facts, now, window))
        .cloned()
        .collect();
    active
}

/// The row for a GUI application with no conversation inside the configured
/// range (or none open at all). It has no thread to open, so clicking only
/// activates the application.
#[cfg(target_os = "macos")]
fn hosted_application_instance(process: &ProcessRecord, display: &str) -> AgentInstance {
    AgentInstance {
        key: process.pid.to_string(),
        kind: display.into(),
        label: display.into(),
        pid: process.pid,
        cwd: None,
        state: AgentState::Ready,
        uptime: process.uptime,
        model: None,
        context: None,
        open_url: None,
        automatic_confirmation_mode: false,
        informational: false,
    }
}

/// A conversation that is still mid-turn or otherwise demands the user's
/// attention: it is working, waits for the user, or its last turn failed.
/// Those stay listed however long they have been idle, because the tray is how
/// the user notices them. A failed turn must not age out either: dropping it
/// would make the application fall back to a plain "ready" row.
#[cfg(target_os = "macos")]
fn needs_attention(facts: &SessionFacts) -> bool {
    matches!(
        facts.state,
        Some(
            AgentState::Working
                | AgentState::Waiting
                | AgentState::WaitingReply
                | AgentState::Error
        )
    )
}

#[cfg(target_os = "macos")]
fn touched_recently(facts: &SessionFacts, now: SystemTime, window: Option<Duration>) -> bool {
    // "All conversations" keeps every finished conversation listed.
    let Some(window) = window else { return true };
    facts
        .activity
        .and_then(|activity| now.duration_since(activity).ok())
        .is_some_and(|age| age < window)
}

/// One row per active conversation of a GUI host. Each row carries its own key
/// and deep link, so several working conversations appear side by side and
/// clicking one opens exactly that thread.
#[cfg(target_os = "macos")]
fn hosted_codex_instances(
    process: &ProcessRecord,
    display: &str,
    titles: &mut CodexTitles,
    conversations: Vec<(PathBuf, SessionFacts)>,
) -> Vec<AgentInstance> {
    conversations
        .into_iter()
        .map(|(rollout, facts)| {
            let thread = crate::session::codex_rollout_thread_id(&rollout).map(str::to_owned);
            let title = thread.as_deref().and_then(|thread| titles.title(thread));
            AgentInstance {
                key: thread.as_deref().map_or_else(
                    || process.pid.to_string(),
                    |thread| format!("{}:{thread}", process.pid),
                ),
                kind: display.into(),
                label: conversation_label(display, facts.cwd.as_deref(), title.as_deref()),
                pid: process.pid,
                cwd: facts.cwd.clone(),
                state: facts.state.unwrap_or(AgentState::Ready),
                uptime: rollout_uptime(&rollout).unwrap_or(process.uptime),
                model: facts.model.clone(),
                context: facts.context.clone(),
                open_url: codex_thread_url(&rollout),
                automatic_confirmation_mode: facts.automatic_confirmation_mode,
                informational: false,
            }
        })
        .collect()
}

/// `Kind (project) · title`, omitting whatever is unknown. The title is what
/// keeps several conversations of the same project apart.
#[cfg(target_os = "macos")]
fn conversation_label(display: &str, cwd: Option<&Path>, title: Option<&str>) -> String {
    let mut label = match cwd.and_then(Path::file_name).and_then(|name| name.to_str()) {
        Some(project) => format!("{display} ({project})"),
        None => display.to_owned(),
    };
    if let Some(title) = title {
        label.push_str(" · ");
        label.push_str(&title.chars().take(24).collect::<String>());
    }
    label
}

/// How long ago the conversation started, from the rollout file's birth time.
#[cfg(target_os = "macos")]
fn rollout_uptime(rollout: &Path) -> Option<Duration> {
    rollout.metadata().ok()?.created().ok()?.elapsed().ok()
}

/// A host application keeps one rollout open per conversation (the ChatGPT
/// app-server holds several at once), so reporting the most recently written
/// one picks whichever thread happened to be touched last. Report the most
/// actionable instead: a session waiting for the user outranks one that is
/// merely working, and ties go to the most recently active rollout.
#[cfg(target_os = "macos")]
fn most_actionable<T>(
    items: impl Iterator<Item = T>,
    key: impl Fn(&T) -> (AgentState, Option<std::time::SystemTime>),
) -> Option<T> {
    items.max_by_key(|item| key(item))
}

fn kind_order(kind: &str) -> usize {
    // Display order of every row the tray can show. Hosted sessions (ChatGPT)
    // sort next to the agent they embed. Kept in sync with `display_name` by
    // `kind_order_covers_every_agent_kind`.
    const ORDER: [&str; 6] = [
        "Claude",
        "Codex",
        "ChatGPT",
        "OpenCode",
        "DeepSeek Harness",
        "Pi",
    ];
    ORDER
        .iter()
        .position(|candidate| *candidate == kind)
        .unwrap_or(usize::MAX)
}

fn enrich_opencode(instance: &mut AgentInstance, analyzer: &mut crate::opencode::OpenCodeAnalyzer) {
    let Some(cwd) = instance.cwd.as_deref() else {
        return;
    };
    let Some(facts) = analyzer.analyze(cwd) else {
        return;
    };
    instance.model = facts.model;
    instance.context = facts.context;
    if let Some(state) = facts.state {
        instance.state = state;
    }
}

fn enrich_deepseek(instance: &mut AgentInstance, analyzer: &mut crate::deepseek::DeepSeekAnalyzer) {
    let Some(cwd) = instance.cwd.as_deref() else {
        return;
    };
    let Some(facts) = analyzer.analyze(cwd) else {
        return;
    };
    instance.model = facts.model;
    instance.context = facts.context;
    if let Some(state) = facts.state {
        instance.state = state;
    }
}

/// Bind each live pi process to its own session file and enrich rows with the
/// per-process facts. Pi sessions are keyed by project directory, so without
/// per-process selection two pi processes in one directory would both read the
/// most recently written session and mirror the active one's state.
fn enrich_pi_instances(instances: &mut [AgentInstance], analyzer: &mut crate::pi::PiAnalyzer) {
    use crate::pi::LiveSession;
    use std::time::SystemTime;

    let mut by_cwd: HashMap<PathBuf, Vec<(usize, u32, Duration)>> = HashMap::new();
    for (index, instance) in instances.iter().enumerate() {
        if instance.kind != display_name("pi") || instance.state == AgentState::Stopped {
            continue;
        }
        let Some(cwd) = instance.cwd.clone() else {
            continue;
        };
        by_cwd
            .entry(cwd)
            .or_default()
            .push((index, instance.pid, instance.uptime));
    }
    if by_cwd.is_empty() {
        return;
    }
    let now = SystemTime::now();
    for (cwd, mut rows) in by_cwd {
        rows.sort_by_key(|(_, pid, _)| *pid);
        let live: Vec<LiveSession> = rows
            .iter()
            .map(|(_, _, uptime)| LiveSession {
                started: now.checked_sub(*uptime).unwrap_or(now),
            })
            .collect();
        for ((index, _, _), facts) in rows.into_iter().zip(analyzer.analyze(&cwd, &live)) {
            let Some(facts) = facts else {
                continue;
            };
            let instance = &mut instances[index];
            instance.model = facts.model;
            instance.context = facts.context;
            if let Some(state) = facts.state {
                instance.state = state;
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn enrich_codex(
    instance: &mut AgentInstance,
    analyzer: &mut SessionAnalyzer,
    has_active_child: bool,
    resumed_session_id: Option<String>,
    terminal: &mut crate::terminal::TerminalProbe,
) {
    let facts = match (instance.cwd.as_deref(), resumed_session_id.as_deref()) {
        (Some(cwd), session_id) => analyzer.analyze_codex(cwd, session_id),
        // sysinfo can briefly return no cwd for a newly resumed process. The
        // thread ID still identifies the rollout exactly, so do not fall back to
        // the default Ready state while the process metadata catches up.
        (None, Some(session_id)) => analyzer.analyze_codex(Path::new(""), Some(session_id)),
        (None, None) => None,
    };
    let Some(facts) = facts else {
        return;
    };
    if instance.cwd.is_none() {
        if let Some(cwd) = facts.cwd.as_ref() {
            instance.cwd = Some(cwd.clone());
            if let Some(project) = cwd.file_name().and_then(|name| name.to_str()) {
                instance.label = format!("Codex ({project})");
            }
        }
    }
    let requires_terminal_probe = facts.requires_terminal_probe;
    let activity = facts.activity;
    instance.model = facts.model;
    instance.context = facts.context;
    instance.automatic_confirmation_mode = facts.automatic_confirmation_mode;
    if let Some(state) = facts.state {
        instance.state = state;
    }
    if requires_terminal_probe {
        match terminal.request(instance.pid, activity) {
            Some(AgentState::Waiting) if instance.state == AgentState::Working => {
                instance.state = AgentState::Waiting
            }
            Some(AgentState::Working)
                if instance.state == AgentState::Waiting && has_active_child =>
            {
                instance.state = AgentState::Working
            }
            _ => {}
        }
    }
}

#[cfg(target_os = "macos")]
fn enrich_macos_codex(
    instance: &mut AgentInstance,
    analyzer: &mut SessionAnalyzer,
    has_active_child: bool,
    rollouts: Vec<PathBuf>,
    terminal: &mut crate::terminal::TerminalProbe,
    probe_terminal: bool,
    resumed_session_id: Option<&str>,
) -> Option<PathBuf> {
    // Returns the rollout the reported facts came from, so a hosted session can
    // link to that exact conversation.
    let (rollout, facts) = if rollouts.is_empty() {
        // Newer Codex versions write conversations through a shared app-server
        // daemon, so the terminal process no longer holds its own rollout open.
        // Resolve the conversation from the session directory instead: a
        // resumed session names its thread on the command line, and a fresh
        // session owns the newest rollout created after the process started.
        let facts = match instance.cwd.as_deref() {
            Some(cwd) => match resumed_session_id {
                Some(session_id) => analyzer.analyze_codex_session(session_id),
                None => {
                    // `ps` reports uptime to the second, so allow a few extra
                    // seconds when deriving the process's start instant.
                    let started =
                        SystemTime::now().checked_sub(instance.uptime + Duration::from_secs(5));
                    analyzer.analyze_codex_for_cwd_since(cwd, started)
                }
            },
            None => None,
        };
        match facts {
            Some(facts) => (None, facts),
            // A newly opened terminal Codex may not have created its rollout
            // yet. Stay ready instead of mirroring another process's session.
            None => {
                instance.state = AgentState::Ready;
                return None;
            }
        }
    } else {
        let chosen = most_actionable(
            rollouts
                .iter()
                .filter_map(|path| Some((path.clone(), analyzer.analyze_codex_rollout(path)?))),
            |(_, facts)| (facts.state.unwrap_or(AgentState::Stopped), facts.activity),
        );
        let Some((path, facts)) = chosen else {
            // Leave the process-derived state untouched, matching the lsof path
            // that a version still holding its own rollout would take.
            return None;
        };
        (Some(path), facts)
    };
    if let Some(cwd) = facts.cwd {
        instance.cwd = Some(cwd.clone());
        if let Some(project) = cwd.file_name().and_then(|name| name.to_str()) {
            instance.label = format!("{} ({project})", instance.kind);
        }
    }
    let requires_terminal_probe = facts.requires_terminal_probe;
    let activity = facts.activity;
    instance.model = facts.model;
    instance.context = facts.context;
    instance.automatic_confirmation_mode = facts.automatic_confirmation_mode;
    if let Some(state) = facts.state {
        instance.state = state;
    }
    // A session hosted by a GUI application has no terminal of its own, so
    // there is no approval prompt to probe for.
    if requires_terminal_probe && probe_terminal {
        match terminal.request(instance.pid, activity) {
            Some(AgentState::Waiting) if instance.state == AgentState::Working => {
                instance.state = AgentState::Waiting
            }
            Some(AgentState::Working)
                if instance.state == AgentState::Waiting && has_active_child =>
            {
                instance.state = AgentState::Working
            }
            _ => {}
        }
    }
    rollout
}

/// Codex's app-server exposes every thread at `codex://threads/<id>`; that is
/// also the link the ChatGPT desktop app itself opens for a conversation.
#[cfg(target_os = "macos")]
fn codex_thread_url(rollout: &Path) -> Option<String> {
    crate::session::codex_rollout_thread_id(rollout).map(|id| format!("codex://threads/{id}"))
}

#[cfg(not(target_os = "macos"))]
fn codex_resume_session_id(process: &Process) -> Option<String> {
    let args = process
        .cmd()
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    codex_resume_session_id_from_args(&args).or_else(|| {
        args.is_empty()
            .then(|| codex_resume_session_id_from_process_command(process.pid().as_u32()))?
    })
}

#[cfg(not(target_os = "macos"))]
fn codex_resume_session_id_from_process_command(pid: u32) -> Option<String> {
    #[cfg(not(target_os = "windows"))]
    {
        // A long-lived sysinfo snapshot can expose a new Codex process before its
        // command-line metadata is populated. macOS `ps` provides the missing
        // resume thread ID without waiting for the next process metadata refresh.
        let output = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
            .ok()?;
        let command = String::from_utf8(output.stdout).ok()?;
        let args = command
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        codex_resume_session_id_from_args(&args)
    }
    #[cfg(target_os = "windows")]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "macos")]
fn codex_resume_session_id_from_command(command: &str) -> Option<String> {
    let args = command
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    codex_resume_session_id_from_args(&args)
}

fn codex_resume_session_id_from_args(args: &[String]) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == "resume")
        .map(|pair| pair[1].clone())
        .filter(|id| id.len() >= 16 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
}

fn enrich_claude(instance: &mut AgentInstance, analyzer: &mut SessionAnalyzer) {
    let Some(home) = dirs::home_dir() else { return };
    let session = read_json(
        &home
            .join(".claude/sessions")
            .join(format!("{}.json", instance.pid)),
    );
    let Some(session_id) = session.as_ref().and_then(|v| v["sessionId"].as_str()) else {
        return;
    };
    if let Some(cwd) = session.as_ref().and_then(|v| v["cwd"].as_str()) {
        instance.cwd = Some(PathBuf::from(cwd));
        if let Some(project) = Path::new(cwd).file_name().and_then(|v| v.to_str()) {
            instance.label = format!("Claude ({project})");
        }
    }
    let native_state = session
        .as_ref()
        .and_then(|v| v["status"].as_str())
        .and_then(|status| match status.to_ascii_lowercase().as_str() {
            "waiting" => Some(AgentState::Waiting),
            "busy" | "working" | "running" => Some(AgentState::Working),
            "idle" | "ready" => Some(AgentState::Ready),
            "error" | "failed" | "aborted" | "disconnected" | "offline" => Some(AgentState::Error),
            _ => None,
        });
    let snapshot = read_json(
        &PathBuf::from("/tmp/agent-statusbar-claude-context").join(format!("{session_id}.json")),
    );
    if let Some(value) = snapshot.as_ref().and_then(|v| v["model"].as_str()) {
        instance.model = Some(value.into());
    }
    if let Some(context) = snapshot.as_ref().and_then(|v| v.get("context_usage")) {
        if let (Some(used_tokens), Some(window_tokens)) = (
            context["used_tokens"].as_u64(),
            context["window_tokens"].as_u64(),
        ) {
            instance.context = Some(crate::model::ContextUsage {
                used_tokens,
                window_tokens,
            });
        }
    }
    if let Some(transcript) = snapshot
        .as_ref()
        .and_then(|v| v["transcript_path"].as_str())
    {
        if let Some(facts) = analyzer.analyze_jsonl(Path::new(transcript), "claude") {
            instance.model = facts.model.or(instance.model.take());
            instance.context = facts.context.or(instance.context.take());
            // Explicit human-intervention signals outrank the native busy/idle flag.
            instance.state = match facts.state {
                Some(AgentState::Waiting | AgentState::WaitingReply) => facts.state.unwrap(),
                _ => native_state.or(facts.state).unwrap_or(instance.state),
            };
            return;
        }
    }
    if let Some(state) = native_state {
        instance.state = state;
    }
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_reader(std::fs::File::open(path).ok()?).ok()
}

/// Why the DeepSeek Harness desktop application is or is not being reported.
/// Meant for `--diagnose-deepseek-desktop`: it names the process the application
/// was recognized as, the profile it points at, the conversations read from that
/// profile's projection cache, and what one scan of that cache costs.
pub fn diagnose_deepseek_desktop() -> Value {
    let mut result = serde_json::json!({
        "sessionCacheRoot": crate::deepseek_desktop::session_cache_root()
            .map(|path| path.display().to_string()),
        "hosts": [],
        "processes": [],
        "sessions": [],
    });
    let mut home: Option<PathBuf> = None;
    #[cfg(target_os = "macos")]
    {
        let source = crate::macos_process::MacProcessSource::default();
        let processes = source.processes();
        result["processes"] = Value::Array(
            processes
                .iter()
                .filter(|process| {
                    process.executable.to_ascii_lowercase().contains("deepseek")
                        || process.command.to_ascii_lowercase().contains("deepseek")
                })
                .map(|process| {
                    serde_json::json!({
                        "pid": process.pid,
                        "kind": agent_kind_from_executable(&process.executable),
                        "executable": process.executable,
                        "host": is_desktop_host(process),
                    })
                })
                .collect(),
        );
        result["hosts"] = Value::Array(
            desktop_roots(&processes)
                .into_iter()
                .map(|(process, detected)| {
                    home = crate::deepseek_desktop::env_home().or_else(|| detected.clone());
                    serde_json::json!({
                        "pid": process.pid,
                        "ppid": process.ppid,
                        "executable": process.executable,
                        "home": detected.map(|path| path.display().to_string()),
                    })
                })
                .collect(),
        );
    }
    // A host that is not running still has a profile worth reading: without this
    // the diagnostic reported nothing at all whenever the app was closed, which
    // is exactly when it is consulted for a state mismatch.
    if result["hosts"]
        .as_array()
        .is_some_and(|hosts| hosts.is_empty())
    {
        // The analyzer falls back to `DSH_HOME` (or `~/.dsh`) on its own; only
        // an explicitly discovered home is passed through here.
        home = None;
    } else if crate::deepseek_desktop::env_home().is_none() && home.is_none() {
        home = None;
    }
    // The same work the 2-second scan does, timed. `cold` reads every session
    // document, `warm` is the steady state where nothing moved since the last
    // scan, and `sessions` builds the rows the menu consumes.
    let mut analyzer = crate::deepseek_desktop::DeepSeekDesktopAnalyzer::default();
    let cold = Instant::now();
    let cold_parsed = analyzer.refresh(home.as_deref());
    let cold_ms = cold.elapsed().as_secs_f64() * 1_000.0;
    let warm = Instant::now();
    let warm_parsed = analyzer.refresh(home.as_deref());
    let warm_ms = warm.elapsed().as_secs_f64() * 1_000.0;
    // A second steady-state scan is where an approval prompt is re-read (the
    // pass is rate-limited), so this is the worst case of the 2-second cycle.
    let repeat = Instant::now();
    analyzer.refresh(home.as_deref());
    let repeat_ms = repeat.elapsed().as_secs_f64() * 1_000.0;
    let read = Instant::now();
    let sessions = analyzer.sessions(None);
    let sessions_ms = read.elapsed().as_secs_f64() * 1_000.0;
    result["health"] = serde_json::json!({
        "rootPresent": analyzer.health().root_present,
        "files": analyzer.health().files,
        "parsed": analyzer.health().parsed,
        "versionMismatch": analyzer.health().version_mismatch,
        "unreadable": analyzer.health().unreadable,
        "incomplete": analyzer.health().incomplete,
        "retryUnjudged": analyzer.health().retry_unjudged,
        "intact": analyzer.health().is_intact(),
    });
    result["timing"] = serde_json::json!({
        "refreshColdMs": cold_ms,
        "refreshColdParsed": cold_parsed,
        "refreshWarmMs": warm_ms,
        "refreshWarmParsed": warm_parsed,
        "refreshRepeatMs": repeat_ms,
        "sessionsMs": sessions_ms,
    });
    result["sessions"] = serde_json::json!(sessions
        .into_iter()
        .map(|session| {
            let signals = analyzer.signal_report(&session.id);
            serde_json::json!({
                "id": session.id,
                "cwd": session.cwd.map(|path| path.display().to_string()),
                "title": session.title,
                "state": session.state,
                "model": session.model,
                "signals": signals,
            })
        })
        .collect::<Vec<_>>());
    result
}

#[cfg(not(target_os = "macos"))]
fn executable(process: &Process) -> String {
    process
        .exe()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or_else(|| process.name().to_str().unwrap_or_default())
        .to_ascii_lowercase()
}

#[cfg(not(target_os = "macos"))]
fn agent_kind(process: &Process) -> Option<&'static str> {
    let name = executable(process);
    // The ChatGPT desktop app ships the same headless Codex app-server on
    // Windows, driven by the GUI instead of a terminal session.
    if name == "codex"
        && process
            .cmd()
            .iter()
            .any(|argument| argument.to_str().is_some_and(is_host_server_argument))
    {
        return None;
    }
    match name.as_str() {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        "opencode" => Some("opencode"),
        "pi" => Some("pi"),
        "dsh" | "deepseek-harness" => Some("deepseek"),
        _ => None,
    }
}

fn display_name(kind: &str) -> &str {
    match kind {
        "claude" => "Claude",
        "codex" => "Codex",
        "opencode" => "OpenCode",
        "pi" => "Pi",
        _ => "DeepSeek Harness",
    }
}

#[cfg(not(target_os = "macos"))]
fn has_agent_parent(process: &Process, kind: &str, system: &System) -> bool {
    let mut parent = process.parent();
    let mut seen = HashSet::new();
    while let Some(pid) = parent {
        if !seen.insert(pid) {
            break;
        }
        let Some(candidate) = system.process(pid) else {
            break;
        };
        if agent_kind(candidate) == Some(kind) {
            return true;
        }
        parent = candidate.parent();
    }
    false
}

#[cfg(not(target_os = "macos"))]
fn has_task_descendant(root: Pid, kind: &str, system: &System) -> bool {
    let parents: HashMap<_, _> = system
        .processes()
        .iter()
        .map(|(pid, p)| (*pid, p.parent()))
        .collect();
    system.processes().iter().any(|(pid, process)| {
        let name = executable(process);
        if *pid == root
            || agent_kind(process).is_some()
            || (kind == "codex" && name == "codex-code-mode-host")
        {
            return false;
        }
        let mut cursor = Some(*pid);
        let mut seen = HashSet::new();
        while let Some(current) = cursor {
            if current == root {
                return true;
            }
            if !seen.insert(current) {
                break;
            }
            cursor = parents.get(&current).copied().flatten();
        }
        false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    fn record(pid: u32, ppid: u32, executable: &str, args: &str) -> ProcessRecord {
        ProcessRecord {
            pid,
            ppid,
            uptime: Duration::ZERO,
            executable: executable.into(),
            command: format!("{executable} {args}").trim_end().to_owned(),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn chatgpt_bundle_is_recognized_as_a_host_application() {
        assert_eq!(
            host_application("/Applications/ChatGPT.app/Contents/Resources/codex")
                .map(|host| host.display),
            Some("ChatGPT")
        );
        assert!(host_application("/opt/homebrew/bin/codex").is_none());
        // A path that merely contains "Codex" as a directory name is not the
        // application bundle itself.
        assert!(host_application("/Users/me/.codex/computer-use/Codex").is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn host_application_is_found_through_process_ancestry() {
        // ChatGPT.app runs its bundled Codex app-server as a child process, so
        // that session belongs to the app rather than to a standalone Codex.
        let host = [
            record(1, 0, "/sbin/launchd", ""),
            record(
                100,
                1,
                "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT",
                "",
            ),
            record(
                101,
                100,
                "/Applications/ChatGPT.app/Contents/Resources/codex",
                "-c features.code_mode_host=true app-server",
            ),
        ];
        assert_eq!(
            host_application_for(&host[2], &host).map(|host| host.display),
            Some("ChatGPT")
        );

        // A terminal Codex has no GUI host in its ancestry.
        let terminal = [
            record(1, 0, "/sbin/launchd", ""),
            record(200, 1, "/Applications/iTerm.app/Contents/MacOS/iTerm2", ""),
            record(201, 200, "/opt/homebrew/bin/codex", ""),
        ];
        assert!(host_application_for(&terminal[2], &terminal).is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unknown_app_bundled_and_host_server_codex_stay_hidden() {
        // A bundled helper with no known host owner must not appear at all.
        assert!(is_host_integration(&record(
            1,
            0,
            "/Applications/SomeOther.app/Contents/Resources/codex",
            "app-server"
        )));
        // A user-installed codex driven as a host server (IDE extension).
        assert!(is_host_integration(&record(
            2,
            0,
            "/Users/me/.nvm/versions/node/v24/bin/codex",
            "app-server"
        )));
        // A user-driven codex in a terminal remains a real session.
        assert!(!is_host_integration(&record(
            3,
            0,
            "/Users/me/.nvm/versions/node/v24/bin/codex",
            "resume 01abc"
        )));
        assert!(!is_host_integration(&record(
            4,
            0,
            "/opt/homebrew/bin/opencode",
            ""
        )));
    }

    #[cfg(target_os = "macos")]
    fn conversation(
        thread: &str,
        state: Option<AgentState>,
        age_secs: u64,
    ) -> (PathBuf, SessionFacts) {
        let mut facts = SessionFacts::default();
        facts.state = state;
        facts.activity = Some(SystemTime::now() - Duration::from_secs(age_secs));
        (
            PathBuf::from(format!("rollout-2026-09-10T18-28-56-{thread}.jsonl")),
            facts,
        )
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn only_unfinished_or_recent_conversations_get_rows() {
        let now = SystemTime::now();
        let conversations = vec![
            conversation("working", Some(AgentState::Working), 3 * 3600),
            conversation("waiting", Some(AgentState::Waiting), 4 * 3600),
            conversation("failed", Some(AgentState::Error), 5 * 3600),
            conversation("recent", Some(AgentState::Ready), 60),
            conversation("stale", Some(AgentState::Ready), 3 * 3600),
        ];
        let selected = select_conversations(conversations, now, Some(Duration::from_secs(15 * 60)));
        let threads: Vec<_> = selected
            .iter()
            .map(|(path, _)| crate::session::codex_rollout_thread_id(path).unwrap())
            .collect();
        // Newest activity first; the long-idle finished conversation is dropped
        // so history tabs cannot bury the menu. A failed turn stays listed: the
        // user still has to notice it. Distinct ages keep the expected order
        // independent of the clock's resolution.
        assert_eq!(threads, ["recent", "working", "waiting", "failed"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_configured_window_decides_how_long_finished_conversations_stay() {
        let now = SystemTime::now();
        let conversations = || {
            vec![
                conversation("recent", Some(AgentState::Ready), 5 * 60),
                conversation("fresh", Some(AgentState::Ready), 30 * 60),
                conversation("day", Some(AgentState::Ready), 20 * 3600),
            ]
        };
        let at = |seconds: u64| {
            let selected =
                select_conversations(conversations(), now, Some(Duration::from_secs(seconds)));
            selected
                .iter()
                .map(|(path, _)| {
                    crate::session::codex_rollout_thread_id(path)
                        .unwrap()
                        .to_owned()
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(at(15 * 60), ["recent"]);
        assert_eq!(at(3600), ["recent", "fresh"]);
        assert_eq!(at(12 * 3600), ["recent", "fresh"]);
        assert_eq!(at(24 * 3600), ["recent", "fresh", "day"]);
        let all = select_conversations(conversations(), now, None);
        assert_eq!(all.len(), 3, "\"all\" keeps every conversation");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn no_conversation_in_range_selects_nothing() {
        // An empty selection is what makes the caller fall back to the
        // application row; keeping an old conversation here would present a
        // session the user has moved on from as if it were live.
        let now = SystemTime::now();
        let selected = select_conversations(
            vec![
                conversation("older", Some(AgentState::Ready), 3 * 3600),
                conversation("newer", Some(AgentState::Ready), 2 * 3600),
            ],
            now,
            Some(Duration::from_secs(15 * 60)),
        );
        assert!(selected.is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_application_row_has_no_conversation_to_open() {
        let process = record(
            233,
            1,
            "/Applications/ChatGPT.app/Contents/Resources/codex",
            "",
        );
        let row = hosted_application_instance(&process, "ChatGPT");
        assert_eq!(row.kind, "ChatGPT");
        assert_eq!(row.label, "ChatGPT");
        assert_eq!(row.key, "233");
        assert_eq!(row.state, AgentState::Ready);
        assert!(row.open_url.is_none(), "nothing to deep link to");
        assert!(row.cwd.is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn hosted_rows_are_distinguished_by_their_conversation() {
        let process = record(
            233,
            1,
            "/Applications/ChatGPT.app/Contents/Resources/codex",
            "",
        );
        let mut titles = CodexTitles::default();
        let rows = hosted_codex_instances(
            &process,
            "ChatGPT",
            &mut titles,
            vec![
                conversation(
                    "01a08a9c-8b7b-7530-be66-8da1fee75728",
                    Some(AgentState::Working),
                    1,
                ),
                conversation(
                    "01a08add-08dc-7362-b1ec-9006b7b625b4",
                    Some(AgentState::Working),
                    2,
                ),
            ],
        );
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].key, rows[1].key, "each conversation owns its row");
        assert_eq!(
            rows[0].open_url.as_deref(),
            Some("codex://threads/01a08a9c-8b7b-7530-be66-8da1fee75728")
        );
        assert_eq!(rows[0].pid, 233);
        assert_eq!(rows[0].kind, "ChatGPT");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn conversation_labels_add_the_thread_title() {
        let cwd = Path::new("/Users/me/code/nita");
        assert_eq!(
            conversation_label("ChatGPT", Some(cwd), Some("看看git状态")),
            "ChatGPT (nita) · 看看git状态"
        );
        assert_eq!(
            conversation_label("ChatGPT", Some(cwd), None),
            "ChatGPT (nita)"
        );
        assert_eq!(
            conversation_label("ChatGPT", None, Some("在吗")),
            "ChatGPT · 在吗"
        );
        assert_eq!(
            conversation_label("ChatGPT", Some(cwd), Some(&"标".repeat(40))),
            format!("ChatGPT (nita) · {}", "标".repeat(24))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn hosted_session_links_to_its_own_conversation() {
        assert_eq!(
            codex_thread_url(Path::new(
                "/home/u/.codex/sessions/2026/09/10/rollout-2026-09-10T17-18-30-01a08a9c-8b7b-7530-be66-8da1fee75728.jsonl"
            )),
            Some("codex://threads/01a08a9c-8b7b-7530-be66-8da1fee75728".to_owned())
        );
        assert_eq!(codex_thread_url(Path::new("session.jsonl")), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn most_actionable_prefers_a_session_waiting_for_the_user() {
        use std::time::SystemTime;
        let at = |secs: u64| Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
        let key = |(state, secs): &(AgentState, u64)| (*state, at(*secs));
        // The waiting conversation is older than the working one: exactly the
        // shape of the ChatGPT app-server holding two threads open, where
        // picking the most recently written rollout reports the wrong one.
        let chosen = most_actionable(
            [
                (AgentState::Working, 300),
                (AgentState::Waiting, 100),
                (AgentState::Ready, 400),
            ]
            .into_iter(),
            key,
        )
        .expect("a session");
        assert_eq!(chosen.0, AgentState::Waiting);

        // Ties fall back to the most recently active rollout.
        let chosen = most_actionable(
            [(AgentState::Working, 300), (AgentState::Working, 500)].into_iter(),
            key,
        )
        .expect("a session");
        assert_eq!(chosen.1, 500);
    }

    #[test]
    fn kind_order_places_hosted_sessions_beside_their_agent() {
        for kind in supported_kinds() {
            assert!(
                kind_order(display_name(kind)) < usize::MAX,
                "{} missing from the display order",
                display_name(kind)
            );
        }
        assert!(kind_order("Codex") < kind_order("ChatGPT"));
        assert!(kind_order("ChatGPT") < kind_order("OpenCode"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn agent_kind_matches_only_the_executed_program() {
        assert_eq!(agent_kind_from_executable("pi"), Some("pi"));
        assert_eq!(
            agent_kind_from_executable("/Users/me/.local/bin/pi"),
            Some("pi")
        );
        // Shells are not agents even though their command lines mention one.
        assert_eq!(agent_kind_from_executable("/bin/zsh"), None);
        assert_eq!(agent_kind_from_executable("/bin/bash"), None);
        assert_eq!(agent_kind_from_executable("claude"), Some("claude"));
        assert_eq!(agent_kind_from_executable("dsh"), Some("deepseek"));
        // A GUI helper whose bundled path contains "Codex" is not the CLI: the
        // basename must be exactly `codex`.
        assert_eq!(
            agent_kind_from_executable("/Applications/ChatGPT.app/Contents/Resources/codex"),
            Some("codex")
        );
        assert_eq!(
            agent_kind_from_executable(
                "/Applications/ChatGPT.app/Contents/Frameworks/Codex Framework.framework/Versions/151.0/Helpers/Codex (Renderer).app/Contents/MacOS/Codex (Renderer)"
            ),
            None
        );
        assert_eq!(
            agent_kind_from_executable(
                "/Users/me/.codex/computer-use/Codex Computer Use.app/Contents/MacOS/SkyComputerUseService"
            ),
            None
        );
    }

    #[test]
    fn known_display_names() {
        assert_eq!(display_name("codex"), "Codex");
        assert_eq!(display_name("deepseek"), "DeepSeek Harness");
    }

    #[test]
    fn extracts_resumed_codex_thread_id() {
        let args = vec![
            "codex".into(),
            "resume".into(),
            "01a0103b-98d7-7581-b338-6407764039a9".into(),
        ];
        assert_eq!(
            codex_resume_session_id_from_args(&args).as_deref(),
            Some("01a0103b-98d7-7581-b338-6407764039a9")
        );
    }

    #[test]
    fn extracts_resumed_codex_thread_id_from_process_command() {
        let args = "/opt/homebrew/bin/codex resume 01a0103b-98d7-7581-b338-6407764039a9"
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(
            codex_resume_session_id_from_args(&args).as_deref(),
            Some("01a0103b-98d7-7581-b338-6407764039a9")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn extracts_resumed_codex_thread_id_from_the_command_line() {
        assert_eq!(
            codex_resume_session_id_from_command(
                "/opt/homebrew/bin/codex resume 01a0103b-98d7-7581-b338-6407764039a9"
            )
            .as_deref(),
            Some("01a0103b-98d7-7581-b338-6407764039a9")
        );
        // A fresh session carries no thread id to resolve.
        assert!(codex_resume_session_id_from_command("/opt/homebrew/bin/codex").is_none());
        // Switches such as `--last` are not thread ids.
        assert!(
            codex_resume_session_id_from_command("/opt/homebrew/bin/codex resume --last").is_none()
        );
    }

    #[test]
    fn stopped_instances_have_no_focus_pid() {
        let instance = stopped_instance("opencode");
        assert_eq!(instance.kind, "OpenCode");
        assert_eq!(instance.state, AgentState::Stopped);
        assert_eq!(instance.pid, 0);
    }

    fn desktop_session(id: &str, state: AgentState) -> crate::deepseek_desktop::DesktopSession {
        crate::deepseek_desktop::DesktopSession {
            id: id.into(),
            cwd: Some(PathBuf::from("/Users/me/code/nita")),
            title: Some("同步代码".into()),
            state,
            model: Some("deepseek-flash".into()),
            context: Some(crate::model::ContextUsage {
                used_tokens: 152_082,
                window_tokens: 1_000_000,
            }),
            activity: SystemTime::now(),
            created: Some(SystemTime::now() - Duration::from_secs(600)),
            turn_open: true,
            automatic_confirmation_mode: false,
            missing_rows: 0,
        }
    }

    #[test]
    fn desktop_conversations_own_their_rows() {
        let created = SystemTime::now() - Duration::from_secs(600);
        let mut session = desktop_session("session-0c3f162a", AgentState::Working);
        session.created = Some(created);
        let mut waiting = desktop_session("session-106ec104", AgentState::WaitingReply);
        waiting.created = Some(created);
        let rows = desktop_rows(43958, Duration::from_secs(417), &[session, waiting]);
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].key, rows[1].key, "each conversation owns its row");
        assert_eq!(rows[0].kind, "DeepSeek Harness");
        assert_eq!(rows[0].label, "DeepSeek Harness · 同步代码");
        assert_eq!(rows[0].pid, 43958, "clicking brings the app forward");
        assert_eq!(rows[0].state, AgentState::Working);
        assert_eq!(rows[1].state, AgentState::WaitingReply);
        assert_eq!(rows[0].context.as_ref().unwrap().used_tokens, 152_082);
        let uptime = rows[0].uptime.as_secs();
        assert!(
            (599..=601).contains(&uptime),
            "the row ages from the conversation's creation, not the app's: {uptime}s"
        );
    }

    #[test]
    fn descriptor_rows_carry_the_auto_confirmation_mode() {
        let mut session = desktop_session("session-auto", AgentState::Ready);
        session.automatic_confirmation_mode = true;
        let rows = desktop_rows(43958, Duration::from_secs(30), &[session]);
        assert!(rows[0].automatic_confirmation_mode);
        assert!(!rows[0].informational);
    }

    #[test]
    fn the_desktop_host_is_recognized_from_a_real_command_line() {
        // Captured from a running app: the predicate is platform-independent, so
        // it is compiled everywhere and this is the one place the *judgement*
        // (rather than a platform adapter) is exercised on an Apple Silicon
        // host. A borrow error in the non-macOS branch reached CI because that
        // branch is not compiled here; keeping the judgement shared is what
        // makes it reachable.
        let command = "/Applications/DeepSeek Harness.app/Contents/MacOS/DeepSeek Harness \
             --expose-internals /Applications/DeepSeek Harness.app/Contents/Resources/app.asar\
/dsh/node_modules/@deepseek-ai/dsh-desktop-host/lib/index.js";
        assert!(desktop_host_from_parts(
            command,
            "/Applications/DeepSeek Harness.app/Contents/MacOS/DeepSeek Harness"
        ));
        // An ordinary CLI session in the same project is not the desktop host.
        assert!(!desktop_host_from_parts(
            "node /usr/local/lib/node_modules/@deepseek-ai/dsh/bin/dsh.js",
            "/usr/local/bin/node"
        ));
        // Nor is an unrelated Electron application.
        assert!(!desktop_host_from_parts(
            "/Applications/Other App.app/Contents/MacOS/Other App",
            "/Applications/Other App.app/Contents/MacOS/Other App"
        ));
    }

    #[test]
    fn a_format_change_is_stated_in_the_menu() {
        use crate::deepseek_desktop::{AlertKind, ProfileAlert};
        // Nothing wrong: no extra row.
        assert!(desktop_alert_row(43958, None).is_none());
        let row = desktop_alert_row(
            43958,
            Some(ProfileAlert {
                kind: AlertKind::FormatChanged,
                affected: 4,
            }),
        )
        .expect("an alert row");
        assert!(row.informational, "it reports, it is not a session");
        assert_eq!(row.pid, 43958);
        assert_eq!(row.key, "desktop:43958:alert");
        assert!(
            row.label.contains('4'),
            "the affected count must be in the label: {}",
            row.label
        );
        // An absent profile says so instead of showing an empty app.
        let missing = desktop_alert_row(
            43958,
            Some(ProfileAlert {
                kind: AlertKind::ProfileMissing,
                affected: 0,
            }),
        )
        .expect("an alert row");
        assert_eq!(missing.label, crate::i18n::profile_missing());
    }

    #[test]
    fn the_overflow_row_reports_hidden_conversations() {
        // Nothing hidden: no extra row at all.
        assert!(desktop_overflow_row(43958, 0).is_none());
        let row = desktop_overflow_row(43958, 3).expect("an overflow row");
        assert!(row.informational, "it reports, it is not a session");
        assert_eq!(row.pid, 43958);
        assert_eq!(row.key, "desktop:43958:more");
        assert!(
            row.label.contains('3'),
            "the count must be in the label: {}",
            row.label
        );
        // The key cannot collide with a conversation row.
        assert_ne!(row.key, "desktop:session-x");
    }

    #[test]
    fn a_desktop_app_without_conversations_still_gets_a_row() {
        let rows = desktop_rows(43958, Duration::from_secs(30), &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, "desktop:43958");
        assert_eq!(rows[0].label, "DeepSeek Harness");
        assert_eq!(rows[0].state, AgentState::Ready);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_desktop_app_is_not_a_terminal_deepseek_session() {
        let electron = record(
            43958,
            1,
            "/Applications/DeepSeek Harness.app/Contents/MacOS/DeepSeek Harness",
            "",
        );
        assert!(is_desktop_host(&electron));
        // Its Node-mode host runs the same executable, named only by the command.
        let host = record(
            44814,
            43958,
            "/Applications/DeepSeek Harness.app/Contents/MacOS/DeepSeek Harness",
            "--expose-internals /Applications/DeepSeek Harness.app/Contents/Resources/app.asar/dsh/node_modules/@deepseek-ai/dsh-desktop-host/lib/index.js /Applications/DeepSeek Harness.app/Contents/Resources/app.asar/dsh ~/.dsh/profiles/desktop",
        );
        assert!(is_desktop_host(&host));
        // A terminal session is enriched from the CLI session logs instead.
        assert!(!is_desktop_host(&record(
            500,
            1,
            "/opt/homebrew/bin/dsh",
            ""
        )));
    }
}
