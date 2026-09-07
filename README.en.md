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

Release build (remaps local paths automatically):

```bash
bash scripts/build-release.sh
```

Run the tests:

```bash
cargo test
```

Run the tray monitor locally:

```bash
cargo run --release
```

Print detected agents, sessions and states without starting the tray:

```bash
cargo run --release -- --diagnose
```

Package the macOS `.app`. Without `ASI_SIGN_IDENTITY` it is signed ad-hoc; with it, Developer ID signing is used:

```bash
bash scripts/package-macos-app.sh
ASI_SIGN_IDENTITY="Developer ID Application: Runzhe Du (TEAMID)" bash scripts/package-macos-app.sh
```

Sign + notarize + staple (requires configured notarytool credentials):

```bash
ASI_SIGN_IDENTITY="Developer ID Application: Runzhe Du (TEAMID)" ASI_TEAM_ID="TEAMID" ASI_NOTARY_PROFILE="AC_API_KEY" bash scripts/notarize-macos-app.sh
```

## Installation

The current release is **v0.2.13** (macOS arm64, Developer ID signed and notarized). x86_64 macOS / Windows / Linux artifacts are produced automatically for later versions by the [release workflow](.github/workflows/release.yml).

### Homebrew (macOS)

```bash
brew install DuRunzhe/tap/agent-status-indicator
```

Update to the latest version:

```bash
brew upgrade DuRunzhe/tap/agent-status-indicator
```

### npm (cross-platform)

```bash
npm install -g agent-status-indicator
```

After installing, launch the tray monitor with the `agent-status-indicator` command. The npm package bundles prebuilt binaries for every platform — no Rust or native Node toolchain required. On macOS (Apple Silicon) it ships the notarized `.app`, so Gatekeeper will not block it.

Update to the latest version:

```bash
npm update -g agent-status-indicator
```

#### Adding it to your applications (optional)

**macOS (Apple Silicon)**: copy the bundled notarized `.app` into /Applications.

```bash
cp -R "$(npm root -g)/agent-status-indicator/app/darwin-arm64/AgentStatusIndicator.app" /Applications/
```

Launch it afterwards with `open -a AgentStatusIndicator` or from Launchpad.
When updating, if an older copy exists in /Applications, remove the old `.app` before copying the new one.

**Windows**: create a Start Menu shortcut (run in PowerShell).

```powershell
$exe = "$(npm root -g)\agent-status-indicator\bin\win32-x64\agent-status-indicator.exe"
$ws = New-Object -ComObject WScript.Shell
$lnk = $ws.CreateShortcut("$env:APPDATA\Microsoft\Windows\Start Menu\Programs\AgentStatusIndicator.lnk")
$lnk.TargetPath = $exe
$lnk.Save()
```

**Linux**: add a desktop entry (the desktop environment must support AppIndicator/StatusNotifier).

```bash
BIN="$(npm root -g)/agent-status-indicator/bin/linux-x64/agent-status-indicator"
mkdir -p ~/.local/share/applications
cat > ~/.local/share/applications/agent-status-indicator.desktop <<EOF
[Desktop Entry]
Type=Application
Name=AgentStatusIndicator
Comment=AI coding agent tray monitor
Exec=$BIN
Terminal=false
Categories=Utility;
EOF
```

### Bun (cross-platform)

Bun reads the npm registry directly; its global directory defaults to `$(bun pm root -g)` with command entry points in `~/.bun/bin`:

```bash
bun install -g agent-status-indicator
```

Update to the latest version:

```bash
bun update -g agent-status-indicator
```

#### Adding it to your applications (optional)

**macOS (Apple Silicon)**:

```bash
cp -R "$(bun pm root -g)/agent-status-indicator/app/darwin-arm64/AgentStatusIndicator.app" /Applications/
```

**Windows** (PowerShell):

```powershell
$exe = "$(bun pm root -g)\agent-status-indicator\bin\win32-x64\agent-status-indicator.exe"
$ws = New-Object -ComObject WScript.Shell
$lnk = $ws.CreateShortcut("$env:APPDATA\Microsoft\Windows\Start Menu\Programs\AgentStatusIndicator.lnk")
$lnk.TargetPath = $exe
$lnk.Save()
```

**Linux**:

```bash
BIN="$(bun pm root -g)/agent-status-indicator/bin/linux-x64/agent-status-indicator"
mkdir -p ~/.local/share/applications
cat > ~/.local/share/applications/agent-status-indicator.desktop <<EOF
[Desktop Entry]
Type=Application
Name=AgentStatusIndicator
Comment=AI coding agent tray monitor
Exec=$BIN
Terminal=false
Categories=Utility;
EOF
```

### curl (macOS / Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.sh | sh
```

Updating: just re-run the install command (it resolves the latest version).

```bash
curl -fsSL https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.sh | sh
```

Pin a version:

```bash
VERSION=0.2.13 curl -fsSL https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.sh | sh
```

Install elsewhere:

```bash
PREFIX=/usr/local/bin curl -fsSL https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.sh | sh
```

### PowerShell (Windows)

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.ps1 | iex"
```

Updating: just re-run the install command above (it resolves the latest version).

### winget (Windows, once merged into microsoft/winget-pkgs)

```powershell
winget install --id DuRunzhe.AgentStatusIndicator
```

Update to the latest version:

```powershell
winget upgrade --id DuRunzhe.AgentStatusIndicator
```

### GitHub Releases

Download the `tar.gz` / `zip` for your platform, or the npm `tgz`, from <https://github.com/DuRunzhe/AgentIndicator/releases>. The install scripts verify the `.sha256` sidecar published next to each asset.

### Usage

```bash
agent-status-indicator
agent-status-indicator --diagnose
agent-status-indicator --debug-ui
```

- `--diagnose`: prints the detected agents, sessions and states without starting the tray.
- `--debug-ui`: debug mode; writes the tray UI state to `~/.agent-status-indicator-ui.json`.
- Click the tray icon to open the menu: instances are shown with model, context and uptime in the “waiting for confirmation → waiting for reply → working → ready” order; clicking a live instance jumps to its terminal or browser session.
- Native notifications fire when human attention is needed; notification types, display options and start-at-login are adjusted in the Settings menu.
- If you added it to your applications as above, you can also launch it from the graphical launcher.

## Uninstalling

Each channel installs independently, so remove only the ones you used.

Homebrew:

```bash
brew uninstall DuRunzhe/tap/agent-status-indicator
```

If you previously ran `brew services start`, stop it first:

```bash
brew services stop agent-status-indicator
```

npm:

```bash
npm uninstall -g agent-status-indicator
```

Bun:

```bash
bun remove -g agent-status-indicator
```

curl-installed binary:

```bash
rm -f ~/.local/bin/agent-status-indicator
```

If you installed with a custom `PREFIX`, delete the file from that directory instead. Once winget is merged:

```powershell
winget uninstall --id DuRunzhe.AgentStatusIndicator
```

The in-app start-at-login option leaves a LaunchAgent behind; remove it after uninstalling:

```bash
launchctl bootout "gui/$(id -u)/com.agentstatusindicator.app" 2>/dev/null || true
rm -f ~/Library/LaunchAgents/com.agentstatusindicator.app.plist
```

Optional leftover config: `~/.config/agent-status-indicator/config.json`. Uninstalling never touches your Claude/Codex/OpenCode/Pi session files.
