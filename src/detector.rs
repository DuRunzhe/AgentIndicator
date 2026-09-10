#[cfg(target_os = "macos")]
use crate::macos_process::{MacProcessSource, ProcessMetadata, ProcessRecord};
use crate::model::{AgentInstance, AgentState};
use crate::session::SessionAnalyzer;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
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
    opencode: crate::opencode::OpenCodeAnalyzer,
    pi: crate::pi::PiAnalyzer,
    terminal: crate::terminal::TerminalProbe,
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
            opencode: crate::opencode::OpenCodeAnalyzer::default(),
            pi: crate::pi::PiAnalyzer::default(),
            terminal: crate::terminal::TerminalProbe::default(),
            #[cfg(target_os = "macos")]
            web_urls: crate::web::WebUrlDetector::default(),
        }
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
        let mut instances: Vec<_> = roots
            .into_iter()
            .map(|(pid, process, kind)| {
                let cwd = process.cwd().map(PathBuf::from);
                let active = has_task_descendant(pid, kind, &self.system);
                let mut instance = AgentInstance {
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
            .collect();
        let tracked_pids: Vec<_> = roots
            .iter()
            .flat_map(|(root, _, _)| process_tree_pids(root.pid, &processes))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let metadata = self.macos_processes.metadata_for(&tracked_pids);
        let mut instances: Vec<_> = roots
            .into_iter()
            .map(|(process, kind, host)| {
                let group_pids = process_tree_pids(process.pid, &processes);
                let group_metadata = group_pids
                    .iter()
                    .filter_map(|pid| metadata.get(pid))
                    .collect::<Vec<_>>();
                let cwd = group_metadata.iter().find_map(|entry| entry.cwd.clone());
                let display = host.map_or_else(|| display_name(kind), |host| host.display);
                // A GUI host keeps its helper processes alive permanently, so
                // descendant activity says nothing about the embedded agent;
                // the rollout it writes is the source of truth instead.
                let active =
                    host.is_none() && has_active_process_descendant(process.pid, kind, &processes);
                let mut instance = AgentInstance {
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
                };
                match kind {
                    "claude" => enrich_claude(&mut instance, &mut self.sessions),
                    "codex" => {
                        let rollout = enrich_macos_codex(
                            &mut instance,
                            &mut self.sessions,
                            active,
                            codex_rollouts_from_metadata(&group_metadata),
                            &mut self.terminal,
                            host.is_none(),
                        );
                        // A hosted session lives inside the application's own
                        // conversation view, so clicking must open that thread
                        // instead of only activating the application.
                        if host.is_some() {
                            instance.open_url = rollout.as_deref().and_then(codex_thread_url);
                        }
                    }
                    "deepseek" => enrich_deepseek(&mut instance, &mut self.deepseek),
                    "opencode" => enrich_opencode(&mut instance, &mut self.opencode),
                    _ => {}
                }
                instance
            })
            .collect();
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
    }
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
#[cfg(target_os = "macos")]
fn agent_kind_from_executable(executable: &str) -> Option<&'static str> {
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
) -> Option<PathBuf> {
    // Returns the rollout the reported facts came from, so a hosted session can
    // link to that exact conversation.
    let chosen = most_actionable(
        rollouts
            .iter()
            .filter_map(|path| Some((path.clone(), analyzer.analyze_codex_rollout(path)?))),
        |(_, facts)| (facts.state.unwrap_or(AgentState::Stopped), facts.activity),
    );
    let (rollout, facts) = match chosen {
        Some((path, facts)) => (Some(path), Some(facts)),
        None => (
            None,
            instance
                .cwd
                .as_deref()
                .and_then(|cwd| analyzer.analyze_codex_for_cwd(cwd)),
        ),
    };
    let facts = facts?;
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

#[cfg(not(target_os = "macos"))]
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

    #[cfg(not(target_os = "macos"))]
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

    #[cfg(not(target_os = "macos"))]
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

    #[test]
    fn stopped_instances_have_no_focus_pid() {
        let instance = stopped_instance("opencode");
        assert_eq!(instance.kind, "OpenCode");
        assert_eq!(instance.state, AgentState::Stopped);
        assert_eq!(instance.pid, 0);
    }
}
