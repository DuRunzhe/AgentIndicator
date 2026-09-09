# AgentStatusIndicator

**中文** | [English](README.en.md)

原生 AI Coding Agent 托盘监控器（macOS 优先，支持 Windows/Linux）。监控 Claude Code、Codex CLI、OpenCode、DeepSeek Harness 与 Pi 等 Coding Agent 的运行状态：进程存活时按项目区分多个会话，在系统托盘汇总展示「等待确认、等待回复、进行中、就绪、已停止」五态，点击菜单项跳回对应终端或浏览器会话，并在需要人工介入时触发原生系统通知。

源码与发布：<https://github.com/DuRunzhe/AgentIndicator>

## 安装

当前已发布 **v0.2.16**（macOS arm64，Developer ID 签名 + 公证）。x86_64 macOS / Windows / Linux 制品由 [release workflow](.github/workflows/release.yml) 在后续版本自动补齐。

### curl（macOS / Linux）

```bash
curl -fsSL https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.sh | sh
```

不带 `VERSION` 时脚本会依次从 GitHub API、releases 重定向与 npm registry 自动查找最新版本；只有三者都不可达时才需要手动指定。更新：重新运行安装命令即可（默认取最新）。macOS（Apple Silicon）下除命令行二进制外，脚本还会一并从 Release 下载公证的 `.app` 装进“应用程序”（该 `.app` 资产随 workflow 改动后的新版本发布；若某版本未发布此资产，脚本会打印提示并仅安装命令行二进制）。

```bash
curl -fsSL https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.sh | sh
```

指定版本：

```bash
VERSION=0.2.16 curl -fsSL https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.sh | sh
```

指定安装目录：

```bash
PREFIX=/usr/local/bin curl -fsSL https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.sh | sh
```

#### 添加到系统应用（可选）

- **macOS（Apple Silicon）**：无需手动处理。安装脚本会自动下载公证的 `.app` 并装入 `/Applications`（不可写时回退 `~/Applications`），之后可从启动台（Launchpad）或 `open -a AgentStatusIndicator` 启动，不会被 Gatekeeper 拦截；命令行二进制则装入 `${PREFIX:-$HOME/.local}/bin`。
- **macOS（x86_64）**：该架构目前未随 Release 发布公证 `.app`，此方式只安装命令行二进制；需要应用包请改用 npm / Bun 渠道。
- **Linux**：无需手动处理。安装脚本已自动写入桌面入口 `$XDG_DATA_HOME`（默认 `~/.local/share`）下的 `applications/agent-status-indicator.desktop` 与 `icons/hicolor/…` 图标；桌面环境支持 AppIndicator/StatusNotifier 即可从应用列表启动。

### npm（跨平台）

```bash
npm install -g agent-status-indicator
```

装完即可用 `agent-status-indicator` 命令启动托盘。npm 包内置各平台预编译二进制，不需要 Rust 或 Node 原生工具链；macOS（Apple Silicon）包内含已公证的 `.app`，不会被 Gatekeeper 拦截。

更新到最新版：

```bash
npm update -g agent-status-indicator
```

#### 添加到系统应用（可选）

**macOS（Apple Silicon）**：把包内公证的 `.app` 复制进“应用程序”。

```bash
cp -R "$(npm root -g)/agent-status-indicator/app/darwin-arm64/AgentStatusIndicator.app" /Applications/
```

之后可用 `open -a AgentStatusIndicator` 或启动台启动。
更新时如需替换 /Applications 里的旧副本，先删除旧 `.app` 再重新复制。

**Windows**：给“开始菜单”创建快捷方式（在 PowerShell 中执行）。

```powershell
$exe = "$(npm root -g)\agent-status-indicator\bin\win32-x64\agent-status-indicator.exe"
$ws = New-Object -ComObject WScript.Shell
$lnk = $ws.CreateShortcut("$env:APPDATA\Microsoft\Windows\Start Menu\Programs\AgentStatusIndicator.lnk")
$lnk.TargetPath = $exe
$lnk.Save()
```

**Linux**：写入桌面入口（桌面环境需支持 AppIndicator/StatusNotifier）。

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

### Bun（跨平台）

Bun 直接读取 npm registry，全局目录默认在 `$(bun pm root -g)`，命令入口在 `~/.bun/bin`：

```bash
bun install -g agent-status-indicator
```

更新到最新版：

```bash
bun update -g agent-status-indicator
```

#### 添加到系统应用（可选）

**macOS（Apple Silicon）**：

```bash
cp -R "$(bun pm root -g)/agent-status-indicator/app/darwin-arm64/AgentStatusIndicator.app" /Applications/
```

**Windows**（PowerShell）：

```powershell
$exe = "$(bun pm root -g)\agent-status-indicator\bin\win32-x64\agent-status-indicator.exe"
$ws = New-Object -ComObject WScript.Shell
$lnk = $ws.CreateShortcut("$env:APPDATA\Microsoft\Windows\Start Menu\Programs\AgentStatusIndicator.lnk")
$lnk.TargetPath = $exe
$lnk.Save()
```

**Linux**：

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

### Homebrew（macOS）

```bash
brew install DuRunzhe/tap/agent-status-indicator
```

更新到最新版：

```bash
brew upgrade DuRunzhe/tap/agent-status-indicator
```

#### 添加到系统应用（可选）

Homebrew tap 只安装**命令行二进制**（真实位置 `$(brew --prefix)/opt/agent-status-indicator/bin/agent-status-indicator`，`$(brew --prefix)/bin/agent-status-indicator` 是指向它的符号链接），**不含 macOS `.app`**，所以没有可复制进 `/Applications` 的应用包。

- 只需托盘与开机自启：直接运行 `agent-status-indicator`，在“设置”菜单打开「开机自启」，或注册为 brew 服务：

  ```bash
  brew services start agent-status-indicator
  ```

- 确实需要在 Launchpad / Spotlight 中出现的 `.app`：请改用 npm 或 Bun 方式安装（内含公证的 `.app`），Homebrew 方式卸载即可，不必两种都装。

### PowerShell（Windows）

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.ps1 | iex"
```

更新：重新运行上面的安装命令即可（默认取最新）。

#### 添加到系统应用（可选）

脚本把可执行文件放在 `%LOCALAPPDATA%\Programs\AgentStatusIndicator\agent-status-indicator.exe` 并把该目录加入用户 PATH，但**不会**自动创建开始菜单快捷方式。需要时在 PowerShell 中执行：

```powershell
$exe = "$env:LOCALAPPDATA\Programs\AgentStatusIndicator\agent-status-indicator.exe"
$ws = New-Object -ComObject WScript.Shell
$lnk = $ws.CreateShortcut("$env:APPDATA\Microsoft\Windows\Start Menu\Programs\AgentStatusIndicator.lnk")
$lnk.TargetPath = $exe
$lnk.Save()
```

开机自启可在应用“设置”菜单中开启。

### winget（Windows，合入 microsoft/winget-pkgs 后可用）

```powershell
winget install --id DuRunzhe.AgentStatusIndicator
```

更新到最新版：

```powershell
winget upgrade --id DuRunzhe.AgentStatusIndicator
```

合入后由 winget 自行管理文件位置，并自动注册开始菜单入口，无需手动「添加到系统应用」。

### GitHub Release

直接下载 https://github.com/DuRunzhe/AgentIndicator/releases 下对应平台的 `tar.gz` / `zip`，以及 npm `tgz`。安装脚本会校验随资产发布的 `.sha256`。

注意：`tar.gz` / `zip` 资产默认只含命令行二进制；公证 `.app` 单独以 darwin-arm64 的 `*.app.tar.gz` 发布（npm `tgz` 内也含一份），macOS Apple Silicon 用上方 curl 命令安装时会自动装好。手动解压的二进制需自己决定放在哪、是否创建开始菜单 / desktop entry。

### 使用

```bash
agent-status-indicator
agent-status-indicator --diagnose
agent-status-indicator --debug-ui
```

- `--diagnose`：打印当前探测到的 Agent、会话与状态，不启动托盘。
- `--debug-ui`：调试模式，把托盘 UI 状态写入 `~/.agent-status-indicator-ui.json`。
- 单击托盘图标展开菜单：实例按“等待确认 → 等待回复 → 进行中 → 就绪”显示模型、上下文与时长；点击存活实例可跳回对应终端或浏览器会话。
- 需要人工介入时触发系统通知；通知类型、显示内容与开机自启都在“设置”菜单中调整。
- 已按上文加入系统应用后，也可以直接从图形启动器启动。

## 卸载

各渠道独立安装，请按实际使用的渠道分别卸载。

Homebrew：

```bash
brew uninstall DuRunzhe/tap/agent-status-indicator
```

若之前运行过 `brew services start`，先停止服务：

```bash
brew services stop agent-status-indicator
```

npm：

```bash
npm uninstall -g agent-status-indicator
```

Bun：

```bash
bun remove -g agent-status-indicator
```

curl 安装的二进制：

```bash
rm -f ~/.local/bin/agent-status-indicator
```

若安装时用了 `PREFIX`，删除对应目录里的该文件。winget 合入后：

```powershell
winget uninstall --id DuRunzhe.AgentStatusIndicator
```

应用内开启的开机自启会留下 LaunchAgent，卸载后手动清理：

```bash
launchctl bootout "gui/$(id -u)/com.agentstatusindicator.app" 2>/dev/null || true
rm -f ~/Library/LaunchAgents/com.agentstatusindicator.app.plist
```

配置残留可删除 `~/.config/agent-status-indicator/config.json`。卸载不影响 Claude/Codex/OpenCode/Pi 的会话文件。
## 实现方案

采用 **Rust 单进程 + `tray-icon` 原生系统托盘**，不使用 Electron、Python 或常驻 Node.js。`winit` 负责跨平台事件循环，`sysinfo` 负责低成本进程快照，Claude/Codex/OpenCode 的会话文件由 Rust 增量解析。Node 仅作为 npm 安装后的平台二进制启动包装，不参与常驻运行。

选择这套方案的原因：托盘本来就是轻量原生控件，使用 WebView 会额外引入渲染进程和几十到上百 MB 内存；纯 Swift 又无法共享 Windows/Linux 实现。Rust 可同时满足原生菜单、单二进制发布和低常驻资源占用。

### 架构

```text
系统进程表 ───────┐
Claude session ───┤
Codex rollout ────┼─> 2 秒增量采集器 ─> 状态优先级引擎 ─> 原生托盘菜单
OpenCode SQLite ──┤                              ├─> 原生系统通知
DeepSeek 进程 ────┘                              └─> 终端/浏览器聚焦
```

状态优先级：等待确认 → 等待回复 → 进行中 → 就绪 → 已停止。工具调用必须按 ID 配对，只有未完成的 `request_user_input` / `AskUserQuestion` 判为等待回复，显式提权或完整终端确认提示判为等待确认。

### 依赖

| 依赖 | 用途 | 常驻额外进程 |
|---|---|---:|
| Rust 标准库 + `crossbeam-channel` | 采集/UI 解耦 | 0 |
| `tray-icon` + `winit` | macOS/Windows/Linux 原生托盘与事件循环 | 0 |
| `sysinfo` | 一次刷新进程树 | 0 |
| `serde_json` | 增量解析 JSON/JSONL | 0 |
| `objc2-user-notifications` | macOS `UNUserNotificationCenter` 原生通知与点击回调 | 0 |
| `windows` | Windows WinRT Toast 通知与前台激活回调 | 0 |
| `notify-rust`（zbus 后端） | Linux Freedesktop D-Bus 通知与“打开会话”动作 | 0 |

Linux 运行时需桌面环境提供 AppIndicator/StatusNotifier 支持；Windows 使用系统通知区域；macOS 使用 NSStatusItem。

## 性能验收指标

指标以 release 构建、10 个活跃 Agent、累计 1 GB 会话日志为压力场景：

| 指标 | 目标 | 说明 |
|---|---:|---|
| 稳态 RSS（macOS） | ≤ 35 MB | 单二进制、无常驻解释器与渲染进程的开销下限 |
| 空闲平均 CPU | ≤ 0.5% | 2 秒采集，无每秒进程拉起 |
| P95 状态发现延迟 | ≤ 2.5 秒 | 当前约 2 秒轮询 |
| 托盘菜单打开 P95 | ≤ 50 ms | UI 不等待采集与磁盘读取 |
| 稳态磁盘写入 | 0 B/s | 状态保存在内存，仅配置落盘 |
| 长日志每轮读取 | 仅新增字节 | offset + inode/mtime 增量缓存 |
| 安装体积 | ≤ 15 MB（压缩后） | 单个 strip + LTO 二进制 |

CI 应在 macOS arm64/x64、Windows x64、Linux x64 上记录 RSS、CPU、扫描耗时和菜单更新时间；超过预算即阻止发布。

## 当前实现进度

- [x] 单进程原生系统托盘
- [x] Claude/Codex/OpenCode/DeepSeek 多实例进程发现
- [x] 进程树任务活跃判定、2 秒异步刷新、五态数据模型
- [x] DeepSeek projection/session 状态、等待信号、模型与上下文解析
- [x] Codex Terminal 确认界面与后台任务正向状态纠正
- [x] 原生动态菜单、汇总图标、npm/Homebrew 发布骨架
- [x] 逐字节增量解析 Claude/Codex transcript，工具 ID 配对、模型与上下文
- [x] OpenCode SQLite 状态、模型和上下文读取
- [x] 等待态原生通知与 0/60/180 秒提醒；点击通知聚焦对应终端或 DeepSeek 浏览器会话
- [x] macOS 按 TTY 精确定位 Terminal/iTerm 标签页
- [x] 原生设置菜单、六项显示配置和 macOS 登录启动设置
- [x] macOS LaunchAgent、Windows Startup、Linux XDG autostart 登录启动
- [ ] Windows/Linux 对既有终端窗口的精确聚焦（当前安全降级为启动/激活终端）

当前版本是可运行的第一阶段骨架，尚不能视为完整功能等价版本；上面未完成项是发布 `v1.0.0` 前的硬性范围。

## 开发运行

release 构建（自动重映射本机路径）：

```bash
bash scripts/build-release.sh
```

运行测试：

```bash
cargo test
```

本地启动托盘：

```bash
cargo run --release
```

不启动托盘，打印当前探测到的 Agent、会话与状态：

```bash
cargo run --release -- --diagnose
```

macOS 打包 `.app`。不设 `ASI_SIGN_IDENTITY` 时做 ad-hoc 签名，设置后用 Developer ID 签名：

```bash
bash scripts/package-macos-app.sh
ASI_SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" bash scripts/package-macos-app.sh
```

签名 + 公证 + 装订（需要已配置 notarytool 凭据）：

```bash
ASI_SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" ASI_TEAM_ID="TEAMID" ASI_NOTARY_PROFILE="AC_API_KEY" bash scripts/notarize-macos-app.sh
```

