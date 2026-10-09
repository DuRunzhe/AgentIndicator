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
    /// How long a finished hosted (ChatGPT) conversation keeps its row; `None`
    /// keeps it forever. Set from the config and updated when the menu changes
    /// it.
    conversation_window: Option<Duration>,
    /// How long a finished DeepSeek Harness desktop conversation keeps its row.
    /// A separate setting: a desktop conversation and a hosted one have
    /// different lifetimes, so the two ranges are configured independently.
    deepseek_desktop_window: Option<Duration>,
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
            deepseek_desktop_window: crate::config::Config::load()
                .deepseek_desktop_window_duration(),
            #[cfg(target_os = "macos")]
            codex_titles: CodexTitles::default(),
            #[cfg(target_os = "macos")]
            web_urls: crate::web::WebUrlDetector::default(),
        }
    }

    pub fn set_conversation_window(&mut self, window: Option<Duration>) {
        self.conversation_window = window;
    }

    /// Set the DeepSeek Harness desktop range independently of the hosted one.
    pub fn set_deepseek_desktop_window(&mut self, window: Option<Duration>) {
        self.deepseek_desktop_window = window;
    }

    /// The hosted (ChatGPT) range currently in effect. Read by the tests that
    /// pin the two ranges as independent *data*, not merely two menu groups.
    #[cfg(test)]
    pub fn conversation_window(&self) -> Option<Duration> {
        self.conversation_window
    }

    /// The DeepSeek Harness desktop range currently in effect.
    #[cfg(test)]
    pub fn deepseek_desktop_window(&self) -> Option<Duration> {
        self.deepseek_desktop_window
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
        // The desktop range, not the hosted one: see `deepseek_desktop_window`.
        let (sessions, hidden) = self.deepseek_desktop.overview(self.deepseek_desktop_window);
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
                    // The terminal form carries its own kind so that its absence
                    // can be reported while the desktop form is present.
                    kind: if kind == "deepseek" {
                        DEEPSEEK_TERMINAL_KIND.into()
                    } else {
                        display_name(kind).into()
                    },
                    label: terminal_label(kind, display_name(kind), cwd.as_deref()),
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
        // from the process tree. The conversations are listed whether or not the
        // application is running, because the CLI shares the profile they live in.
        let (sessions, hidden, alert) = self.deepseek_desktop_overview(None);
        instances.extend(desktop_rows(None, &sessions, &[]));
        instances.extend(desktop_overflow_row(0, hidden));
        instances.extend(desktop_alert_row(0, alert));
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
        // A `dsh` CLI is not a row of its own: it drives one of its project's
        // conversations, and each conversation row asks who is driving it to
        // decide where it navigates.
        let mut drivers: Vec<DeepSeekDriver> = Vec::new();
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
                // Record what a DeepSeek CLI serves and which conversation it is
                // writing, then emit no row for the process itself.
                if kind == "deepseek" && host.is_none() {
                    let web_url = self.web_urls.discover(process.pid, &group_pids);
                    // A CLI that serves a web UI serves the whole shared profile:
                    // its UI lists every workspace, not the directory the process
                    // happens to run in, so it carries no single project. Only a
                    // terminal CLI, which has no listening address, is scoped to
                    // the directory it runs in.
                    let project = if web_url.is_some() { None } else { cwd.clone() };
                    let driven = if web_url.is_some() {
                        crate::deepseek::driven_session_in_profile()
                    } else {
                        cwd.as_deref().and_then(crate::deepseek::driven_session_for)
                    };
                    if web_url.is_some() || project.is_some() {
                        drivers.push(DeepSeekDriver {
                            project,
                            web_url,
                            driven,
                        });
                    }
                    return Vec::new();
                }
                let display = host.map_or_else(|| display_name(kind), |host| host.display);
                // A GUI host keeps its helper processes alive permanently, so
                // descendant activity says nothing about the embedded agent;
                // the rollout it writes is the source of truth instead.
                let active =
                    host.is_none() && has_active_process_descendant(process.pid, kind, &processes);
                let mut instance = AgentInstance {
                    key: process.pid.to_string(),
                    // The terminal form carries its own kind so its absence can be
                    // reported while the desktop form lists conversations.
                    kind: if kind == "deepseek" && host.is_none() {
                        DEEPSEEK_TERMINAL_KIND.into()
                    } else {
                        display.into()
                    },
                    label: terminal_label(kind, display, cwd.as_deref()),
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
        if desktop_hosts.is_empty() {
            // The conversations live in `$DSH_HOME`, which the CLI shares and
            // keeps writing, so they are listed whether or not the application is
            // running. The rows carry no pid: with the window closed there is
            // nothing to activate, and a `dsh web` CLI's conversations open its UI.
            let (sessions, hidden, alert) = self.deepseek_desktop_overview(None);
            instances.extend(desktop_rows(None, &sessions, &drivers));
            instances.extend(desktop_overflow_row(0, hidden));
            instances.extend(desktop_alert_row(0, alert));
        } else {
            for (process, home) in desktop_hosts {
                let (sessions, hidden, alert) = self.deepseek_desktop_overview(home.as_deref());
                instances.extend(desktop_rows(
                    Some((process.pid, process.uptime)),
                    &sessions,
                    &drivers,
                ));
                instances.extend(desktop_overflow_row(process.pid, hidden));
                instances.extend(desktop_alert_row(process.pid, alert));
            }
        }
        if let Ok(mut snapshot) = DRIVERS_SNAPSHOT.lock() {
            *snapshot = drivers.clone();
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

/// Key of the DeepSeek Harness placeholder row. The desktop form's placeholder
/// and its stopped row are the same row, so they share this key and the scan
/// never lists both.
pub(crate) const DEEPSEEK_PLACEHOLDER_KIND: &str = "DeepSeek Harness";

/// Key and kind of the DeepSeek Harness terminal (`dsh` CLI) rows and of its
/// placeholder.
///
/// The kind is distinct from the desktop form's on purpose: the two forms are
/// independently present, so "no terminal running" must be able to produce its
/// own row while the desktop application keeps listing its conversations. The
/// kind is never rendered — the menu shows the label and the state — so it only
/// has to sort and group.
pub(crate) const DEEPSEEK_TERMINAL_KIND: &str = "DeepSeek Harness (terminal)";


/// The drivers found by the most recent scan, as JSON: the project a `dsh` CLI
/// serves, the conversation it drives, and the address it serves.
pub fn drivers_json() -> Value {
    let drivers = DRIVERS_SNAPSHOT
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    Value::Array(
        drivers
            .iter()
            .map(|driver| {
                serde_json::json!({
                    "project": driver.project.as_ref().map(|path| path.to_string_lossy()),
                    "webUrl": driver.web_url,
                    "driven": driver.driven,
                })
            })
            .collect(),
    )
}

/// The drivers found by the most recent scan, for the diagnostic. A process-wide
/// snapshot keeps the diagnostic independent of which `Detector` instance scanned.
static DRIVERS_SNAPSHOT: std::sync::Mutex<Vec<DeepSeekDriver>> = std::sync::Mutex::new(Vec::new());

/// A running `dsh` CLI process: the project it serves, where its web UI listens,
/// and the conversation it is driving.
///
/// The terminal and the desktop application share one profile, so a conversation
/// is one row whichever of them drives it. This is what tells that row where to
/// navigate. A terminal CLI is scoped to the project it runs in; the `dsh web`
/// server has no project, because its UI lists every workspace in the profile.
#[derive(Clone)]
struct DeepSeekDriver {
    project: Option<PathBuf>,
    web_url: Option<String>,
    driven: Option<String>,
}

/// One row per conversation the DeepSeek Harness desktop application has open,
/// taken from the profile's session cache. Every row carries the host's pid, so
/// clicking it activates the application window; the conversation id keeps the
/// rows (and their notification state) apart.
///
/// With no conversation inside the configured range the application still gets
/// a row: the user has the window open, and "ready" is the truth about it.
fn desktop_rows(
    host: Option<(u32, Duration)>,
    sessions: &[crate::deepseek_desktop::DesktopSession],
    drivers: &[DeepSeekDriver],
) -> Vec<AgentInstance> {
    let display = display_name("deepseek");
    let pid = host.map_or(0, |(pid, _)| pid);
    // Whether a window exists to bring forward, which decides where a
    // conversation the CLI is not currently writing navigates.
    let host_running = host.is_some();
    if sessions.is_empty() {
        // With no conversations in range the application still gets a row, but
        // only while it is running: "ready" describes a window that is open, and
        // a row with no process behind it would be a row for nothing.
        let Some((pid, uptime)) = host else {
            return Vec::new();
        };
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
            // How long this run has lasted, not how old the conversation is: a
            // conversation created months ago and used today is minutes old.
            //
            // No measured run means *no duration*, not the application's uptime:
            // the app runs for weeks, so borrowing its uptime is what produced
            // absurd values like "1337h" on conversations the tray had not read.
            // The value is display-only (ordering uses the state), so silence is
            // better than a wrong number.
            uptime: session.run.unwrap_or(Duration::ZERO),
            model: session.model.clone(),
            context: session.context.clone(),
            // Where a row navigates follows whoever is driving the conversation:
            // the `dsh web` UI when a CLI owns it, otherwise the desktop window,
            // which has no per-conversation deep link yet.
            open_url: session_target(session, drivers, host_running),
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

/// `DeepSeek Harness · <form> · <title>`, falling back to the project name and then to
/// the application name. The title already carries the project in practice, so
/// repeating both would only shorten the useful part of the row.
fn desktop_label(session: &crate::deepseek_desktop::DesktopSession) -> String {
    let display = display_name("deepseek");
    // One row per conversation, whichever process drives it: the terminal and the
    // desktop application share a profile, so naming a form would report the same
    // conversation twice and the row's destination is decided by the click.
    let detail = session
        .title
        .as_deref()
        .or_else(|| session.cwd.as_deref().and_then(Path::file_name)?.to_str());
    match detail {
        Some(detail) => format!("{display} · {detail}"),
        None => display.into(),
    }
}

/// Where a conversation row navigates.
///
/// A `dsh` CLI process serves conversations over its own web UI; the desktop
/// application shows them in its window. Two rules, in order:
///
/// 1. The conversation the CLI is writing is the one it is showing, so it opens
///    the web UI. This holds whether or not the window is open.
/// 2. Everything else a CLI serves opens the web UI too when the window is *not*
///    running: the application cannot be activated, so its web UI is the only
///    destination the conversation has. While the window is open the same
///    conversations belong to it and bring it forward.
///
/// A driver reaches a conversation when it serves the whole profile — the `dsh
/// web` server, which lists every workspace — or the conversation's own project.
/// Only a driver with a listening address has a destination, so a terminal CLI
/// that serves no URL never wins the lookup.
///
/// The web UI has no per-conversation route, so a destination is the UI, not one
/// conversation inside it.
fn session_target(
    session: &crate::deepseek_desktop::DesktopSession,
    drivers: &[DeepSeekDriver],
    host_running: bool,
) -> Option<String> {
    let cwd = session.cwd.as_deref();
    let driver = drivers.iter().find(|driver| {
        driver.web_url.is_some() && (driver.project.is_none() || driver.project.as_deref() == cwd)
    })?;
    let url = driver.web_url.clone()?;
    if driver.driven.as_deref() == Some(session.id.as_str()) || !host_running {
        return Some(url);
    }
    None
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
    agent_kind_from_executable(&process.executable, &process.command)
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

/// Whether a process is the `dsh` CLI, however it was installed.
///
/// The CLI ships as an npm package, so there is no binary named `dsh` to match
/// on: npm, pnpm, bun and npx all run it through a JavaScript runtime, and a
/// standalone or globally linked install runs the same entry file directly. What
/// is stable across every one of those is the package the entry file lives in —
/// `@deepseek-ai/dsh` under a `node_modules` directory — so that is what is
/// matched, in the executable path and in every command-line token.
///
/// Deliberately *not* keyed on the runtime's name or on `lib/bin.js`: either can
/// change without a new installation method appearing, and pinning them would
/// reproduce exactly the bug this replaced (a basename-only match that missed
/// `node …/dsh/lib/bin.js`).
fn is_dsh_cli(executable: &str, command: &str) -> bool {
    is_dsh_package_path(executable) || reaches_dsh_through_runtime(executable, command)
}

/// Whether a command line, or an executable path, names the `dsh` CLI package
/// or the desktop host that embeds the same runtime.
///
/// The search runs over the *whole* string rather than one whitespace-separated
/// token: this application's own bundle path contains a space, so a token split
/// cut `…/DeepSeek Harness.app/…/@deepseek-ai/dsh/lib/bin.js` in half and the
/// package stopped being recognisable. Backslashes are normalised first, because
/// Windows separates paths with them.
///
/// The package must sit directly under a `node_modules` directory and be followed
/// by something that looks like a file, so a bare `…/@deepseek-ai/dsh` argument —
/// the shape `grep -r dsh …/dsh` has — is not an agent, while every real
/// invocation is.
fn is_dsh_package_path(text: &str) -> bool {
    const PACKAGE: &str = "node_modules/@deepseek-ai/dsh";
    let normalized = text.replace('\\', "/");
    let mut rest = normalized.as_str();
    while let Some(found) = rest.find(PACKAGE) {
        let after = &rest[found + PACKAGE.len()..];
        // The desktop host carries the same runtime under its own package name.
        if after
            .strip_prefix("desktop-host")
            .is_some_and(|tail| tail.starts_with('/'))
        {
            return true;
        }
        if let Some(tail) = after.strip_prefix('/') {
            // Any later segment looking like a file (`lib/bin.js`,
            // `package.json`, an entry file at the root) proves the token reaches
            // *inside* the package rather than naming its directory.
            if tail.split('/').any(|segment| segment.contains('.')) {
                return true;
            }
        }
        rest = &rest[found + 1..];
    }
    false
}

/// The runtime names a JavaScript entry file can be handed to. `dsh` is an npm
/// package, so every install runs it through one of these.
fn is_js_runtime(executable: &str) -> bool {
    let name = executable
        .rsplit('/')
        .next()
        .unwrap_or(executable)
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "node" | "node.exe" | "bun" | "bun.exe" | "deno" | "deno.exe"
    )
}

/// Whether a command line reaches the `dsh` CLI through a runtime, either by the
/// package's own entry file or by the shim npm links onto `PATH`.
fn reaches_dsh_through_runtime(executable: &str, command: &str) -> bool {
    is_js_runtime(executable)
        && (is_dsh_package_path(command) || command_tokens(command).any(is_dsh_entry))
}

/// Whether one command-line token is the `dsh` entry point the runtime was handed
/// — the shim npm links onto `PATH`, e.g.
/// `node /Users/…/nvm/versions/node/v24.19.0/bin/dsh web`.
///
/// That shape names the package nowhere, which is why matching the package path
/// alone reported a live CLI as stopped.
///
/// The token has to be an executable `dsh` inside a `bin` or `.bin` directory
/// and carry no extension. `bin` is the shape a global install links onto
/// `PATH` (`…/nvm/…/bin/dsh`); `.bin` is the shape a local or `npx` shim takes
/// (`…/node_modules/.bin/dsh`), which is how `npx @deepseek-ai/dsh` runs and was
/// previously missed. `dsh` as an argument value (`--name dsh`), an extensioned
/// file beside it (`dsh.js`), a bare directory (`…/dsh`) and an arbitrary file
/// that happens to share the name (`/tmp/probe/dsh`) are all still excluded.
fn is_dsh_entry(token: &str) -> bool {
    let token = token.trim_matches(|c| c == '"' || c == '\\');
    if token.starts_with('-') {
        return false;
    }
    let normalized = token.replace('\\', "/");
    let mut segments = normalized.rsplit('/');
    let Some(last) = segments.next() else {
        return false;
    };
    last == "dsh" && matches!(segments.next(), Some("bin" | ".bin"))
}

/// The tokens of a command line, split on whitespace and on the quoting a shell
/// or a runtime may have added. A backslash is deliberately not a separator: it
/// separates paths on Windows, so splitting on it cut such a path apart.
fn command_tokens(command: &str) -> impl Iterator<Item = &str> {
    command
        .split(|c: char| c.is_whitespace() || c == '"' || c == '(' || c == ')')
        .filter(|token| !token.is_empty())
}

/// The executed program's basename, matched against the known agent binaries.
/// Only the executable is considered for the CLI agents: command lines routinely
/// mention agent names (`which pi`) without being that agent. The one exception
/// is `dsh`, which ships as an npm package and is recognised from its command
/// line by [`is_dsh_cli`].
///
/// The DeepSeek Harness desktop application is matched by its bundle directory
/// instead: its executable is the product name with a space, which no CLI
/// binary is ever called.
#[cfg(target_os = "macos")]
fn agent_kind_from_executable(executable: &str, command: &str) -> Option<&'static str> {
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
        _ if is_dsh_cli(executable, command) => Some("deepseek"),
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
    agent_kind_from_executable(executable, "") == Some("codex")
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
    // The terminal form sorts with the agent it belongs to.
    let kind = if kind == DEEPSEEK_TERMINAL_KIND {
        DEEPSEEK_PLACEHOLDER_KIND
    } else {
        kind
    };
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
    // The DeepSeek CLI's process chain, to explain a missing driver. The macOS
    // process source is the only one that reports `comm` and the full command
    // line together, so the probe is macOS-only.
    #[cfg(target_os = "macos")]
    {
        let source = crate::macos_process::MacProcessSource::default();
        let processes = source.processes();
        let rows: Vec<Value> = processes
            .iter()
            .filter(|process| {
                process.command.contains("dsh") || process.executable.contains("dsh")
            })
            .map(|process| {
                serde_json::json!({
                    "pid": process.pid,
                    "ppid": process.ppid,
                    "executable": process.executable,
                    "command": process.command,
                    "kind": agent_kind_from_executable(&process.executable, &process.command),
                    "host": host_application_for(process, &processes).map(|host| host.display),
                    "hasAgentParent": match agent_kind_from_executable(&process.executable, &process.command) {
                        Some(kind) => has_process_agent_parent(process, kind, &processes),
                        None => false,
                    },
                })
            })
            .collect();
        result["cliProbe"] = Value::Array(rows);
    }
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
                        "kind": agent_kind_from_executable(
                            &process.executable,
                            &process.command,
                        ),
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
    result["drivers"] = serde_json::Value::Array(
        DRIVERS_SNAPSHOT
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
            .iter()
            .map(|driver| {
                serde_json::json!({
                    "project": driver.project.as_ref().map(|path| path.to_string_lossy()),
                    "webUrl": driver.web_url,
                    "driven": driver.driven,
                })
            })
            .collect(),
    );
    result["health"] = serde_json::json!({
        "rootPresent": analyzer.health().root_present,
        "files": analyzer.health().files,
        "parsed": analyzer.health().parsed,
        "versionMismatch": analyzer.health().version_mismatch,
        "unreadable": analyzer.health().unreadable,
        "incomplete": analyzer.health().incomplete,
        "retryUnjudged": analyzer.health().retry_unjudged,
        "staleFormat": analyzer.health().stale_format,
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
/// The whole command line of a `sysinfo` process, joined the way the macOS
/// record keeps it.
#[cfg(not(target_os = "macos"))]
fn command_line(process: &Process) -> String {
    process
        .cmd()
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
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
        _ if is_dsh_cli(&name, &command_line(process)) => Some("deepseek"),
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

/// A terminal agent's row label: the agent, its form, and the project it runs in.
///
/// A DeepSeek Harness terminal row also names the conversation it is driving, so
/// the terminal and desktop forms show which conversation each of them reports
/// rather than looking like two rows for the same thing.
fn terminal_label(kind: &str, display: &str, cwd: Option<&Path>) -> String {
    let project = cwd
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty());
    if kind != "deepseek" {
        return match project {
            Some(project) => format!("{display} ({project})"),
            None => display.to_owned(),
        };
    }
    let form = crate::i18n::deepseek_form(false);
    let title = cwd.and_then(crate::deepseek::session_title_for);
    match (project, title.as_deref()) {
        (Some(project), Some(title)) => format!("{display} · {form} ({project}) · {title}"),
        (Some(project), None) => format!("{display} · {form} ({project})"),
        (None, Some(title)) => format!("{display} · {form} · {title}"),
        (None, None) => format!("{display} · {form}"),
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
        let kind = |executable: &str| agent_kind_from_executable(executable, "");
        assert_eq!(kind("pi"), Some("pi"));
        assert_eq!(kind("/Users/me/.local/bin/pi"), Some("pi"));
        // Shells are not agents even though their command lines mention one.
        assert_eq!(kind("/bin/zsh"), None);
        assert_eq!(kind("/bin/bash"), None);
        assert_eq!(kind("claude"), Some("claude"));
        assert_eq!(kind("dsh"), Some("deepseek"));
        // A GUI helper whose bundled path contains "Codex" is not the CLI: the
        // basename must be exactly `codex`.
        assert_eq!(
            kind("/Applications/ChatGPT.app/Contents/Resources/codex"),
            Some("codex")
        );
        assert_eq!(
            kind(
                "/Applications/ChatGPT.app/Contents/Frameworks/Codex Framework.framework/Versions/151.0/Helpers/Codex (Renderer).app/Contents/MacOS/Codex (Renderer)"
            ),
            None
        );
        assert_eq!(
            kind(
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
    fn conversations_are_listed_without_the_application_running() {
        // The conversations live in `$DSH_HOME`, which the CLI shares and keeps
        // writing, so closing the application window must not empty the list. The
        // rows then have no process to activate.
        let sessions = [desktop_session("session-a", AgentState::Working)];
        let rows = desktop_rows(None, &sessions, &[]);
        assert_eq!(rows.len(), 1, "a conversation is listed on its own");
        assert_eq!(rows[0].pid, 0, "there is no window to activate");
        assert_eq!(rows[0].uptime, Duration::ZERO);
        assert_eq!(rows[0].label, "DeepSeek Harness · 同步代码");

        // With no conversation in range there is nothing to describe, so a host
        // that is not running gets no row at all — "ready" would be a claim about
        // a window that does not exist.
        assert!(desktop_rows(None, &[], &[]).is_empty());
        // A running host still reports itself.
        let host_only = desktop_rows(Some((43958, Duration::from_secs(60))), &[], &[]);
        assert_eq!(host_only.len(), 1);
        assert_eq!(host_only[0].pid, 43958);
    }

    #[test]
    fn a_conversation_navigates_to_whoever_is_driving_it() {
        // One row per conversation: the `dsh web` UI opens for the conversation a
        // CLI is writing, and every other conversation brings the application
        // window forward.
        let session = desktop_session("session-driven", AgentState::Working);
        // The web server is profile-wide — its UI lists every workspace — so it
        // carries no project and reaches a conversation wherever it lives.
        let drivers = [DeepSeekDriver {
            project: None,
            web_url: Some("http://127.0.0.1:3080/".into()),
            driven: Some("session-driven".into()),
        }];
        // The conversation a CLI is writing opens its web UI, window or not.
        assert_eq!(
            session_target(&session, &drivers, true).as_deref(),
            Some("http://127.0.0.1:3080/")
        );
        assert_eq!(
            session_target(&session, &drivers, false).as_deref(),
            Some("http://127.0.0.1:3080/")
        );

        // Another conversation belongs to the window while it is open…
        let other = desktop_session("session-other", AgentState::Ready);
        assert_eq!(session_target(&other, &drivers, true), None);
        // …and to the web UI when it is not, because the window cannot be
        // activated and that UI is the only destination left.
        assert_eq!(
            session_target(&other, &drivers, false).as_deref(),
            Some("http://127.0.0.1:3080/")
        );
        // The profile-wide server reaches a conversation in a project it was not
        // started in, which is the shape a `dsh web` launch from elsewhere takes.
        let mut elsewhere = desktop_session("session-elsewhere", AgentState::Ready);
        elsewhere.cwd = Some(PathBuf::from("/Users/me/code/other"));
        assert_eq!(
            session_target(&elsewhere, &drivers, false).as_deref(),
            Some("http://127.0.0.1:3080/")
        );

        // No CLI serving a destination: the window is the only place to go.
        assert_eq!(session_target(&session, &[], true), None);
        assert_eq!(session_target(&session, &[], false), None);
        // A CLI with no listening address cannot be navigated to either. A
        // terminal CLI is scoped to its own project and still offers no URL.
        let silent = [DeepSeekDriver {
            project: Some(PathBuf::from("/Users/me/code/nita")),
            web_url: None,
            driven: Some("session-driven".into()),
        }];
        assert_eq!(session_target(&session, &silent, true), None);
        assert_eq!(session_target(&session, &silent, false), None);
    }

    #[test]
    fn a_terminal_row_names_its_project_and_conversation() {
        let path = Path::new("/Users/me/code/AgentIndicator");
        // Non-DeepSeek agents keep their long-standing shape.
        assert_eq!(
            terminal_label("pi", "Pi", Some(path)),
            "Pi (AgentIndicator)"
        );
        assert_eq!(terminal_label("claude", "Claude", None), "Claude");
        // DeepSeek names the form, then the project. The conversation comes from
        // the log of this project, so it is asserted separately.
        let label = terminal_label("deepseek", "DeepSeek Harness", Some(path));
        assert!(
            label.starts_with(&format!(
                "DeepSeek Harness · {} (AgentIndicator)",
                crate::i18n::deepseek_form(false)
            )),
            "{label}"
        );
        // With no project there is still a form, never a bare duplicate.
        let bare = terminal_label("deepseek", "DeepSeek Harness", None);
        assert!(bare.contains(crate::i18n::deepseek_form(false)));
        assert_ne!(bare, crate::i18n::deepseek_form(false));
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
            last_prompt: Some(SystemTime::now() - Duration::from_secs(600)),
            format_version: Some(4),
            run: None,
            turn_open: true,
            automatic_confirmation_mode: false,
            missing_rows: 0,
        }
    }

    #[test]
    fn desktop_conversations_own_their_rows() {
        let session = desktop_session("session-0c3f162a", AgentState::Working);
        let waiting = desktop_session("session-106ec104", AgentState::WaitingReply);
        let rows = desktop_rows(
            Some((43958, Duration::from_secs(417))),
            &[session, waiting],
            &[],
        );
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].key, rows[1].key, "each conversation owns its row");
        assert_eq!(rows[0].kind, "DeepSeek Harness");
        // The form is named, so a desktop row cannot be mistaken for a terminal
        // one: the two report the same agent through different machinery.
        // One row per conversation, with no form marker: the terminal and the
        // desktop application share a profile, so the conversation is the unit
        // and its destination is decided when it is clicked.
        assert_eq!(rows[0].label, "DeepSeek Harness · 同步代码");
        assert_eq!(rows[0].pid, 43958, "clicking brings the app forward");
        assert_eq!(rows[0].state, AgentState::Working);
        assert_eq!(rows[1].state, AgentState::WaitingReply);
        assert_eq!(rows[0].context.as_ref().unwrap().used_tokens, 152_082);
        // With no measured run the row shows no duration at all, rather than
        // borrowing the application's uptime (which is what produced "1337h").
        assert_eq!(rows[0].uptime, Duration::ZERO);

        // The measured run wins when the log provided one.
        let mut measured = desktop_session("session-measured", AgentState::Working);
        measured.run = Some(Duration::from_secs(125));
        let measured_rows = desktop_rows(Some((43958, Duration::from_secs(417))), &[measured], &[]);
        assert_eq!(measured_rows[0].uptime, Duration::from_secs(125));
    }

    #[test]
    fn descriptor_rows_carry_the_auto_confirmation_mode() {
        let mut session = desktop_session("session-auto", AgentState::Ready);
        session.automatic_confirmation_mode = true;
        let rows = desktop_rows(Some((43958, Duration::from_secs(30))), &[session], &[]);
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
    fn the_dsh_cli_is_recognized_however_it_was_installed() {
        // Every install runs a JavaScript entry file through a runtime: there is
        // no binary named `dsh`. Two command-line shapes have to be covered, and
        // the second is the one this machine actually runs.
        assert!(is_dsh_cli(
            "node",
            "node /Users/me/.npm/_npx/1e7f6d9597241db0/node_modules/@deepseek-ai/dsh/lib/bin.js --profile web"
        ));
        assert!(is_dsh_cli(
            "node",
            "node /Users/me/.dsh/profiles/node_modules/@deepseek-ai/dsh/lib/bin.js"
        ));
        assert!(is_dsh_cli(
            "bun",
            "bun /Users/me/.bun/install/global/node_modules/@deepseek-ai/dsh/lib/bin.js"
        ));
        assert!(is_dsh_cli(
            "deno",
            "deno run -A /Users/me/node_modules/@deepseek-ai/dsh/lib/bin.js"
        ));
        // Captured from this machine: the shim npm links onto `PATH` names the
        // package nowhere, so a package-path-only match reported a perfectly live
        // CLI as stopped.
        assert!(is_dsh_cli(
            "node",
            "node /Users/durunzhe/.nvm/versions/node/v24.19.0/bin/dsh web"
        ));
        // Captured from this machine: `npx @deepseek-ai/dsh web` runs the local
        // shim in `node_modules/.bin`, not the global `bin` directory. Matching
        // only `bin` left the running CLI unrecognized, so its conversations had
        // no driver and clicking one did nothing. The `_npx` cache id is opaque
        // and irrelevant — only the `.bin/dsh` shape is matched.
        assert!(is_dsh_cli(
            "node",
            "node /Users/me/.npm/_npx/0000000000000000/node_modules/.bin/dsh web"
        ));
        // Windows separators and quoting.
        assert!(is_dsh_cli(
            "node",
            "node \"C:\\Users\\me\\AppData\\Roaming\\npm\\node_modules\\@deepseek-ai\\dsh\\lib\\bin.js\""
        ));

        // Merely mentioning the name is not an agent.
        assert!(!is_dsh_cli("node", "node script.js --name dsh"));
        assert!(!is_dsh_cli(
            "grep",
            "grep -r dsh node_modules/@deepseek-ai/dsh"
        ));
        assert!(!is_dsh_cli(
            "cat",
            "cat node_modules/@deepseek-ai/dsh/README.md"
        ));
        assert!(!is_dsh_cli("node", "node /tmp/probe/dsh"));
        // The `.bin` rule still requires `dsh` to sit directly inside it.
        assert!(!is_dsh_cli("node", "node /tmp/.bin/nested/dsh"));
        assert!(!is_dsh_cli("node", "node /tmp/.bin/dsh.js"));
        assert!(!is_dsh_cli(
            "node",
            "node node_modules/@deepseek-ai/dsh-other/lib/bin.js"
        ));
        assert!(!is_dsh_cli(
            "node",
            "node /tmp/fake/@deepseek-ai/dsh/lib/bin.js"
        ));
        // A package path without a JS runtime is not a running CLI either.
        assert!(!is_dsh_cli(
            "/bin/cat",
            "cat /tmp/node_modules/@deepseek-ai/dsh/lib/bin.js"
        ));
        // A directory argument is not an invocation.
        assert!(!is_dsh_cli(
            "node",
            "node -e \"0\" node_modules/@deepseek-ai/dsh"
        ));
    }

    #[test]
    fn the_real_cli_command_line_is_recognized() {
        // Captured from `ps -axo command=` for a running `dsh web` on this
        // machine, and from `ps comm=` for its executable.
        let executable = "node";
        let command = "node /Users/durunzhe/.nvm/versions/node/v24.19.0/bin/dsh web";
        assert!(
            is_dsh_cli(executable, command),
            "the shim invocation must be recognized"
        );
        assert_eq!(
            agent_kind_from_executable(executable, command),
            Some("deepseek"),
            "and it must be classified as the deepseek agent"
        );
    }

    #[test]
    fn a_path_with_spaces_is_still_matched() {
        // The desktop host's own path contains spaces; a whitespace-only split
        // would cut `@deepseek-ai/dsh` in half.
        assert!(is_dsh_cli(
            "node",
            "node /Applications/DeepSeek Harness.app/Contents/Resources/app.asar/dsh/node_modules/@deepseek-ai/dsh/lib/bin.js"
        ));
    }

    #[test]
    fn the_two_ranges_are_independent_in_the_detector() {
        // The menu is only the surface. What matters is that the two settings
        // are separate pieces of state feeding separate readers, so changing one
        // cannot move the other.
        let mut detector = Detector::new();
        let hosted = Some(Duration::from_secs(60 * 60));
        let desktop = Some(Duration::from_secs(12 * 60 * 60));
        detector.set_conversation_window(hosted);
        detector.set_deepseek_desktop_window(desktop);
        assert_eq!(detector.conversation_window(), hosted);
        assert_eq!(detector.deepseek_desktop_window(), desktop);

        // Moving one leaves the other exactly where it was, including to "all"
        // (the `None` that keeps every conversation).
        detector.set_deepseek_desktop_window(None);
        assert_eq!(
            detector.conversation_window(),
            hosted,
            "the hosted range must not follow the desktop one"
        );
        assert_eq!(detector.deepseek_desktop_window(), None);

        detector.set_conversation_window(None);
        assert_eq!(detector.deepseek_desktop_window(), None);
        assert_eq!(detector.conversation_window(), None);

        // And back the other way.
        detector.set_conversation_window(desktop);
        detector.set_deepseek_desktop_window(hosted);
        assert_eq!(detector.conversation_window(), desktop);
        assert_eq!(detector.deepseek_desktop_window(), hosted);
        assert_ne!(
            detector.conversation_window(),
            detector.deepseek_desktop_window(),
            "the two readers hold their own values"
        );
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
        let rows = desktop_rows(Some((43958, Duration::from_secs(30))), &[], &[]);
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
