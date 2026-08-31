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
            .filter_map(|process| process_kind(process).map(|kind| (process, kind)))
            .filter(|(process, kind)| !has_process_agent_parent(process, kind, &processes))
            .collect();
        let tracked_pids: Vec<_> = roots
            .iter()
            .flat_map(|(root, _)| process_tree_pids(root.pid, &processes))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let metadata = self.macos_processes.metadata_for(&tracked_pids);
        let mut instances: Vec<_> = roots
            .into_iter()
            .map(|(process, kind)| {
                let group_pids = process_tree_pids(process.pid, &processes);
                let group_metadata = group_pids
                    .iter()
                    .filter_map(|pid| metadata.get(pid))
                    .collect::<Vec<_>>();
                let cwd = group_metadata.iter().find_map(|entry| entry.cwd.clone());
                let active = has_active_process_descendant(process.pid, kind, &processes);
                let mut instance = AgentInstance {
                    kind: display_name(kind).into(),
                    label: cwd
                        .as_ref()
                        .and_then(|path| path.file_name())
                        .and_then(|name| name.to_str())
                        .map(|project| format!("{} ({project})", display_name(kind)))
                        .unwrap_or_else(|| display_name(kind).into()),
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
                };
                match kind {
                    "claude" => enrich_claude(&mut instance, &mut self.sessions),
                    "codex" => enrich_macos_codex(
                        &mut instance,
                        &mut self.sessions,
                        active,
                        codex_rollout_from_metadata(&group_metadata),
                        &mut self.terminal,
                    ),
                    "deepseek" => enrich_deepseek(&mut instance, &mut self.deepseek),
                    "opencode" => enrich_opencode(&mut instance, &mut self.opencode),
                    _ => {}
                }
                instance
            })
            .collect();
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
    }
}

fn supported_kinds() -> [&'static str; 4] {
    ["claude", "codex", "opencode", "deepseek"]
}

#[cfg(target_os = "macos")]
fn process_kind(process: &ProcessRecord) -> Option<&'static str> {
    agent_kind_from_command(&process.command)
}

#[cfg(target_os = "macos")]
fn agent_kind_from_command(command: &str) -> Option<&'static str> {
    command
        .split_whitespace()
        .filter_map(|value| Path::new(value).file_name().and_then(|name| name.to_str()))
        .map(|name| name.trim_matches('"').to_ascii_lowercase())
        .find_map(|name| match name.as_str() {
            "claude" => Some("claude"),
            "codex" => Some("codex"),
            "opencode" => Some("opencode"),
            "dsh" | "deepseek-harness" => Some("deepseek"),
            _ => None,
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
fn codex_rollout_from_metadata(metadata: &[&ProcessMetadata]) -> Option<PathBuf> {
    let home = dirs::home_dir()?.join(".codex/sessions");
    metadata
        .iter()
        .flat_map(|entry| entry.files.iter())
        .filter(|path| path.starts_with(&home))
        .filter(|path| crate::session::primary_codex_rollout_cwd(path).is_some())
        .filter_map(|path| Some((path.metadata().ok()?.modified().ok()?, path.clone())))
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

fn kind_order(kind: &str) -> usize {
    supported_kinds()
        .iter()
        .position(|candidate| display_name(candidate) == kind)
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
    rollout: Option<PathBuf>,
    terminal: &mut crate::terminal::TerminalProbe,
) {
    let facts = rollout
        .as_deref()
        .and_then(|path| analyzer.analyze_codex_rollout(path))
        .or_else(|| {
            instance
                .cwd
                .as_deref()
                .and_then(|cwd| analyzer.analyze_codex_for_cwd(cwd))
        });
    let Some(facts) = facts else {
        return;
    };
    if let Some(cwd) = facts.cwd {
        instance.cwd = Some(cwd.clone());
        if let Some(project) = cwd.file_name().and_then(|name| name.to_str()) {
            instance.label = format!("Codex ({project})");
        }
    }
    let requires_terminal_probe = facts.requires_terminal_probe;
    let activity = facts.activity;
    instance.model = facts.model;
    instance.context = facts.context;
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
    match name.as_str() {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        "opencode" => Some("opencode"),
        "dsh" | "deepseek-harness" => Some("deepseek"),
        _ => None,
    }
}

fn display_name(kind: &str) -> &str {
    match kind {
        "claude" => "Claude",
        "codex" => "Codex",
        "opencode" => "OpenCode",
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
