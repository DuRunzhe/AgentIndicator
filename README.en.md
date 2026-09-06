# AgentStatusIndicator

**English** | [中文](README.md)

A native tray monitor for AI coding agents (macOS first, with Windows/Linux support). It watches coding agents such as Claude Code, Codex CLI, OpenCode, DeepSeek Harness and Pi: while a process is alive, sessions are grouped by project, and the tray shows a five-state summary — *waiting for confirmation, waiting for reply, working, ready, stopped*. Clicking a menu item jumps back to the matching terminal or browser session, and native system notifications fire whenever human attention is needed.

Source and releases: <https://github.com/DuRunzhe/AgentIndicator>

## Implementation approach

A **Rust single-process app on the native `tray-icon` system tray**, without Electron, Python or a resident Node.js runtime. `winit` drives the cross-platform event loop, `sysinfo` takes low-cost process snapshots, and Claude/Codex/OpenCode session files are parsed incrementally in Rust. Node appears only as the platform binary launcher inside the npm package; it never runs as a resident process.

Rationale: the tray is a lightweight native control — a WebView would add a rendering process and tens to hundreds of MB of memory, while pure Swift could not share the Windows/Linux implementation. Rust provides native menus, single-binary distribution and low resident resource usage at the same time.

### Architecture

```text
system process table ──┐
Claude session ────────┤
Codex rollout ─────────┼─> 2s incremental collector ─> state priority engine ─> native tray menu
OpenCode SQLite ───────┤                                          ├─> native system notifications
DeepSeek processes ────┘                                          └─> terminal/browser focus
```

State priority: *waiting for confirmation → waiting for reply → working → ready → stopped*. Tool calls are paired by ID; only an unanswered `request_user_input` / `AskUserQuestion` counts as *waiting for reply*, while an explicit escalation or a complete terminal confirmation prompt counts as *waiting for confirmation*.

### Dependencies

| Dependency | Purpose | Extra resident processes |
|---|---:|---:|
| Rust std + `crossbeam-channel` | collector/UI decoupling | 0 |
| `tray-icon` + `winit` | native tray and event loop (macOS/Windows/Linux) | 0 |
| `sysinfo` | one-shot process-tree refresh | 0 |
| `serde_json` | incremental JSON/JSONL parsing | 0 |
| `objc2-user-notifications` | macOS `UNUserNotificationCenter` notifications and click callbacks | 0 |
| `windows` | Windows WinRT toast notifications and foreground activation callbacks | 0 |
| `notify-rust` (zbus backend) | Linux Freedesktop D-Bus notifications and “open session” actions | 0 |

On Linux the desktop environment must provide AppIndicator/StatusNotifier support; Windows uses the notification area; macOS uses `NSStatusItem`.

## Performance acceptance targets

Measured with a release build, 10 active agents and ~1 GB of cumulative session logs:

| Metric | Target | Notes |
|---|---:|---|
| Steady-state RSS (macOS) | ≤ 35 MB | floor for a single binary without a resident interpreter/renderer |
| Idle average CPU | ≤ 0.5% | 2s collection, no per-second process spawning |
| P95 state-discovery latency | ≤ 2.5 s | currently ~2s polling |
| P95 tray-menu open | ≤ 50 ms | UI never waits on collection or disk reads |
| Steady-state disk writes | 0 B/s | state kept in memory; only config hits disk |
| Long-log per-round reads | appended bytes only | offset + inode/mtime incremental cache |
| Install size | ≤ 15 MB (compressed) | single stripped + LTO binary |

CI records RSS, CPU, scan time and menu update time on macOS arm64/x64, Windows x64 and Linux x64, and blocks a release when a budget is exceeded.

## Current status

- [x] Single-process native system tray
- [x] Multi-instance process discovery for Claude/Codex/OpenCode/DeepSeek
- [x] Process-tree activity detection, 2s async refresh, five-state model
- [x] DeepSeek projection/session state, waiting signals, model and context parsing
- [x] Codex terminal confirmation screen and positive correction of background tasks
- [x] Native dynamic menu, summary icon, npm/Homebrew release skeleton
- [x] Byte-level incremental Claude/Codex transcript parsing, tool-ID pairing, model and context
- [x] OpenCode SQLite state, model and context reading
- [x] Native notifications for waiting states with 0/60/180 s reminders; clicking focuses the terminal or the DeepSeek browser session
- [x] Precise Terminal/iTerm tab focus by TTY on macOS
- [x] Native settings menu, five display options and macOS login-startup settings
- [x] macOS LaunchAgent, Windows Startup and Linux XDG autostart entries
- [ ] Precise focus of existing terminal windows on Windows/Linux (currently a safe fallback to launching/activating the terminal)

The current version is a runnable first-stage skeleton, not yet a full feature-parity release; the item above is the hard scope before `v1.0.0`.

## Development

```bash
bash scripts/build-release.sh           # release build (remaps local paths automatically)
cargo test
# Print currently detected agents, session matches and states without the tray
cargo run --release -- --diagnose
# macOS packaging / signing / notarization
bash scripts/package-macos-app.sh
ASI_SIGN_IDENTITY="Developer ID Application: ... (TEAMID)" bash scripts/package-macos-app.sh
ASI_SIGN_IDENTITY="..." ASI_TEAM_ID="..." ASI_NOTARY_PROFILE="AC_API_KEY" bash scripts/notarize-macos-app.sh
```

## Installation

The current release is **v0.2.12** (macOS arm64, Developer ID signed and notarized). x86_64 macOS / Windows / Linux artifacts are produced automatically for later versions by the [release workflow](.github/workflows/release.yml).

### Homebrew (macOS)

```bash
brew install DuRunzhe/tap/agent-status-indicator
```

### npm / Bun (cross-platform, picks the right platform binary)

```bash
npm install -g agent-status-indicator
bun install -g agent-status-indicator
```

The npm package bundles prebuilt binaries for every platform — no Rust or native Node toolchain required; on macOS it runs the notarized `.app` inside the package, so Gatekeeper will not block it.

### curl (macOS / Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.sh | sh
# pin a version: VERSION=0.2.12 curl -fsSL ... | sh
# pick a prefix: PREFIX=/usr/local/bin curl -fsSL ... | sh
```

### PowerShell (Windows)

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.ps1 | iex"
```

### winget (Windows, once merged into microsoft/winget-pkgs)

```powershell
winget install --id DuRunzhe.AgentStatusIndicator
```

### GitHub Releases

Download the `tar.gz` / `zip` for your platform, or the npm `tgz`, from <https://github.com/DuRunzhe/AgentIndicator/releases>. The install scripts verify the `.sha256` sidecar published next to each asset.

### Upgrading

```bash
npm update -g agent-status-indicator     # or bun update -g ...
brew upgrade DuRunzhe/tap/agent-status-indicator
# the curl/PowerShell scripts resolve "latest" by default — just re-run them
```
