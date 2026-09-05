# AgentStatusIndicator

**中文** | [English](README.en.md)

原生 AI Coding Agent 托盘监控器（macOS 优先，支持 Windows/Linux）。监控 Claude Code、Codex CLI、OpenCode、DeepSeek Harness 与 Pi 等 Coding Agent 的运行状态：进程存活时按项目区分多个会话，在系统托盘汇总展示「等待确认、等待回复、进行中、就绪、已停止」五态，点击菜单项跳回对应终端或浏览器会话，并在需要人工介入时触发原生系统通知。

源码与发布：<https://github.com/DuRunzhe/AgentIndicator>

## 指定实现方案

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
- [x] 原生设置菜单、五项显示配置和 macOS 登录启动设置
- [x] macOS LaunchAgent、Windows Startup、Linux XDG autostart 登录启动
- [ ] Windows/Linux 对既有终端窗口的精确聚焦（当前安全降级为启动/激活终端）

当前版本是可运行的第一阶段骨架，尚不能视为完整功能等价版本；上面未完成项是发布 `v1.0.0` 前的硬性范围。

## 开发运行

```bash
bash scripts/build-release.sh           # release 构建（自动重映射本机路径）
cargo test
# 查看当前探测到的 Agent、会话匹配和状态（不启动托盘）
cargo run --release -- --diagnose
# macOS 分发打包 / 签名 / 公证
bash scripts/package-macos-app.sh
ASI_SIGN_IDENTITY="Developer ID Application: ... (TEAMID)" bash scripts/package-macos-app.sh
ASI_SIGN_IDENTITY="..." ASI_TEAM_ID="..." ASI_NOTARY_PROFILE="AC_API_KEY" bash scripts/notarize-macos-app.sh
```

## 安装

当前已发布 **v0.2.10**（macOS arm64，Developer ID 签名 + 公证）。x86_64 macOS / Windows / Linux 制品由 [release workflow](.github/workflows/release.yml) 在后续版本自动补齐。

### Homebrew（macOS）

```bash
brew install DuRunzhe/tap/agent-status-indicator
```

### npm / Bun（跨平台，自动匹配平台二进制）

```bash
npm install -g agent-status-indicator
bun install -g agent-status-indicator
```

npm 包内置各平台预编译二进制，不要求用户安装 Rust 或 Node 原生编译工具链；macOS 走包内已公证的 `.app`，下载后不会被 Gatekeeper 拦截。

### curl（macOS / Linux）

```bash
curl -fsSL https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.sh | sh
# 指定版本：VERSION=0.2.10 curl -fsSL ... | sh
# 指定目录：PREFIX=/usr/local/bin curl -fsSL ... | sh
```

### PowerShell（Windows）

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.ps1 | iex"
```

### winget（Windows，合入 microsoft/winget-pkgs 后可用）

```powershell
winget install --id DuRunzhe.AgentStatusIndicator
```

### GitHub Release

直接下载 https://github.com/DuRunzhe/AgentIndicator/releases 下对应平台的 `tar.gz` / `zip`，以及 npm `tgz`。安装脚本会校验随资产发布的 `.sha256`。

### 升级

```bash
npm update -g agent-status-indicator     # 或 bun update -g ...
brew upgrade DuRunzhe/tap/agent-status-indicator
# curl/PowerShell 脚本默认取 latest，直接重跑即可
```
