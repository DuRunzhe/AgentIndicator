# AgentStatusIndicator

**中文** | [English](README.en.md)

原生 AI Coding Agent 托盘监控器（macOS 优先，支持 Windows/Linux）。监控 Claude Code、Codex CLI、OpenCode、DeepSeek Harness 与 Pi 等 Coding Agent 的运行状态：进程存活时按项目区分多个会话，在系统托盘汇总展示「等待确认、等待回复、进行中、就绪、异常、已停止」六态；可识别 error、failed、aborted 及断连信号，点击菜单项跳回对应终端或浏览器会话，并在需要人工介入时触发原生系统通知。

**DeepSeek Harness 桌面端**：桌面应用（Electron）用一个进程承载所有已打开的会话，因此托盘直接读取该进程所用 profile 的会话投影缓存，把每个活跃会话单独列成一行（标题、项目、模型、上下文占用）。投影缓存不携带的两类信号从会话事件日志补齐：**未决的确认提示**判为等待确认、**失败/断连**判为异常；`approval: never` 的会话识别为自动确认模式，不再重复提醒。等待确认/异常的会话始终优先展示，超过 8 行时末尾以灰色「… N 个会话未显示」说明被省略的数量。点击任意一行把桌面端窗口带到前台：走应用注册的 `dsh://open`（可还原最小化窗口、重建被关闭的窗口），再退回普通应用激活。桌面端目前**没有会话级深链接**——其 shell 只认 `dsh://open` 自身，前端也不把当前会话写进 URL，因此无法直接跳到某个会话。显示范围**独立配置**：设置里有「ChatGPT 会话显示范围」与「DeepSeek Harness 会话显示范围」两组，各含 15 分钟 / 1 小时 / 12 小时 / 24 小时（默认）/ 全部；进行中、等待确认、等待回复、异常的会话始终保留。两者在**功能与数据层**各自独立：分别持久化为 `conversation_window` 与 `deepseek_desktop_window`，分别送入 ChatGPT 托管会话与 DeepSeek 桌面端会话的过滤，互不影响。托盘每一行都采用统一格式 `Kind (项目) · 标题`（标题未知时只保留 `Kind (项目)`，标题超过 24 字符截断）；DeepSeek Harness 的终端与桌面端会话不再区分形态——同一会话永远是一行，点击时按当前驱动方决定跳到 `dsh web` 还是桌面端窗口。

源码与发布：<https://github.com/DuRunzhe/AgentIndicator>

## 安装

当前已发布 **v0.2.30**（macOS arm64，Developer ID 签名 + 公证）。x86_64 macOS / Windows / Linux 制品由 [release workflow](.github/workflows/release.yml) 在后续版本自动补齐。

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
curl -fsSL https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.sh | VERSION=0.2.30 sh
```

指定安装目录：

```bash
curl -fsSL https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.sh | PREFIX=/usr/local/bin sh
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
agent-status-indicator --diagnose-deepseek-desktop
agent-status-indicator --debug-ui
agent-status-indicator --check-update
```

- `--diagnose`：打印当前探测到的 Agent、会话与状态，不启动托盘。
- `--diagnose-deepseek-desktop`：只诊断 DeepSeek Harness 桌面端：识别到的宿主进程、所用 `DSH_HOME`、读到的会话，以及一次会话缓存扫描（冷读/稳态）的耗时，不启动托盘。
- `--debug-ui`：调试模式，把托盘 UI 状态写入 `~/.agent-status-indicator-ui.json`。
- `--check-update`：向 GitHub 查询最新版本并打印结果，不启动托盘。
- 单击托盘图标展开菜单：每行以统一格式 `Kind (项目) · 标题` 显示会话（Claude、Codex、ChatGPT、OpenCode、DeepSeek Harness、Pi 均如此，标题缺失时只保留 `Kind (项目)`），并按“等待确认 → 等待回复 → 进行中 → 就绪”排序，附带模型、上下文与时长；点击存活实例可跳回对应终端、浏览器会话或把 DeepSeek Harness 桌面端窗口带到前台。
- 系统通知默认关闭，可在“设置”中开启，并调整等待确认、等待回复及自动确认模式下的通知。
- “设置”提供六项显示选项、语言、开机自启及 DeepSeek 浏览器标签页复用。会话显示范围按来源**分成两组独立配置**：「ChatGPT 会话显示范围」与「DeepSeek Harness 会话显示范围」，各自支持 15 分钟、1 小时、12 小时、24 小时（默认）或全部；进行中、等待确认/回复或异常的会话始终保留。二者分别持久化、分别作用，改变其中一组不影响另一组。
- 点击“设置”中的 Claude 上下文采集菜单项可安装或卸载采集器。安装会配置 Claude 的 `statusLine`，保留并转发原有命令；卸载会还原原配置。
- “关于”面板每次打开都会检查 GitHub 最新版本；有新版本时按钮变为“更新到 x.y.z”，点击后显示进度并在下载完成后自动安装、重启。
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

配置文件按系统存放：macOS 为 `~/Library/Application Support/agent-status-indicator/config.json`，Linux 为 `${XDG_CONFIG_HOME:-$HOME/.config}/agent-status-indicator/config.json`，Windows 为 `%APPDATA%\agent-status-indicator\config.json`。可按需删除；会话文件无需清理。
## 实现方案

核心采用 **Rust 单进程 + `tray-icon` 原生系统托盘**。直接运行二进制或 `.app` 无需 Node.js；通过 npm / Bun 命令启动时会额外常驻一个 Node 包装进程。

选择这套方案的原因：托盘本来就是轻量原生控件，使用 WebView 会额外引入渲染进程和几十到上百 MB 内存；纯 Swift 又无法共享 Windows/Linux 实现。Rust 可同时满足原生菜单、单二进制发布和低常驻资源占用。

### 架构

```text
系统进程表 ───────┐
Claude session ───┤
Codex rollout ────┼─> 2 秒增量采集器 ─> 状态优先级引擎 ─> 原生托盘菜单
OpenCode SQLite ──┤                              ├─> 原生系统通知
DeepSeek 进程 ────┤                              └─> 终端/浏览器/桌面端聚焦
DeepSeek 桌面缓存 ┘
```

状态优先级：等待确认 → 等待回复 → 进行中 → 就绪 → 已停止。工具调用必须按 ID 配对，只有未完成的 `request_user_input` / `AskUserQuestion` 判为等待回复，显式提权或完整终端确认提示判为等待确认。

DeepSeek Harness 终端会话与桌面端走两条路径：终端 `dsh` 进程按项目读取 `~/.dsh/sessions` 日志；桌面应用（Electron，`DeepSeek Harness.app`）由一个进程承载全部会话，托盘改为读取该进程 `DSH_HOME` 下 `storages/session_projcache/sessions` 的每会话投影缓存——未完成的 `openStep`/`pendingCalls` 判为进行中，其余为就绪。每条会话一行，`open_url` 为空时点击回到桌面端窗口。

**判定规则何以保持通用**（避免硬编码式探测）：

- 投影缓存拿不到的信号（等待、失败）从会话事件日志推导：事件类型先归类为**语义角色**（turn 开/关、进展、失败、审批、提问、无关、未知）一张表，再交给一个**按事件序号排序的时间线状态机**，而不是把事件名直接写进判定分支。
- 失败被建模为**日志中的一个位置**而非锁存标志：`llm/retry` 的 `retry >= maxRetries` 记为失败，之后只有出现"对话确实往前走了"的事件才视为已恢复。`step/end`/`turn/end` 有意不算进展——失败的轮次会自己关闭，离线会话正是停在这里。两者都是**从字段语义推导**（`retry`/`maxRetries` 来自插件自身的重试预算），不是魔法数字。
- 会话日志里没有 error 事件：`agent/error` 是总线事件、不落盘（见官方 API 目录），因此失败必须由 `llm/retry` 推导。
- **未知事件被计数而非静默忽略**：事件类型表对照 `dsh-session` 的官方清单（59 类）逐项覆盖，`--diagnose-deepseek-desktop` 会输出每个会话的 `unknownEvents`；上游新增事件类型时会显示为非零数字，而不是变成一次静默误判。
- **格式变动可见**：解析容错（不因一份坏文档让托盘失效），但每一次偏离预期形状都被计数成"格式健康度"：投影版本不符、结构改名、JSON 不可读、重试预算字段缺失、profile 目录不存在。健康度不达标时菜单里会出现一行灰色告警（如「DeepSeek Harness 设定档格式已变更 (N)」），而不是安静地少几个会话——这正是"空列表"和"应用空闲"无法区分的问题。`scripts/verify-format-mutations.sh` 用 6 种上游格式变异验证这套机制。
- 终端与桌面两条读取器**共用同一份**事件分类与时间线实现，不各自维护一套规则。

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
| 桌面端会话扫描 | ≤ 3 ms | 未变化时只 stat；实测 0.03–0.10 ms，无文件变化时 0 次解析 |
| 桌面端单会话变化 | ≤ 15 ms | 投影缓存整篇重写，单篇 2–25 KB，实测约 11 ms 解析 |
| 安装体积 | ≤ 15 MB（压缩后） | 单个 strip + LTO 二进制 |

CI 应在 macOS arm64/x64、Windows x64、Linux x64 上记录 RSS、CPU、扫描耗时和菜单更新时间；超过预算即阻止发布。

## 当前实现进度

- [x] 单进程原生系统托盘
- [x] Claude/Codex/OpenCode/DeepSeek 多实例进程发现
- [x] 进程树任务活跃判定、2 秒异步刷新、六态数据模型（含异常）
- [x] DeepSeek projection/session 状态、等待信号、模型与上下文解析
- [x] DeepSeek Harness 桌面端：宿主进程识别、按会话投影缓存读取多会话状态、会话事件日志补齐等待确认与异常、自动确认模式识别、按会话显示范围过滤、溢出计数行、点击聚焦应用窗口（`dsh://open` + 应用激活回退）
- [ ] DeepSeek Harness 桌面端会话级深链接（依赖上游：shell 目前只处理 `dsh://open`，前端无 URL 路由）
- [x] Codex Terminal 确认界面与后台任务正向状态纠正
- [x] 原生动态菜单、汇总图标、npm/Homebrew 发布骨架
- [x] 逐字节增量解析 Claude/Codex transcript，工具 ID 配对、模型与上下文
- [x] OpenCode SQLite 状态、模型和上下文读取
- [x] 等待态原生通知与 0/60/180 秒提醒；点击通知聚焦对应终端或 DeepSeek 浏览器会话
- [x] macOS 按 TTY 精确定位 Terminal/iTerm 标签页
- [x] 原生设置菜单、六项显示配置和 macOS 登录启动设置
- [x] macOS LaunchAgent、Windows Startup、Linux XDG autostart 登录启动
- [ ] Windows/Linux 对既有终端窗口的精确聚焦（实现已合入：Windows 直接走 Win32 还原并置前所属终端窗口，Linux 沿进程树定位终端模拟器窗口；待真机验收后勾选）

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
