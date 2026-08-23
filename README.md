# AgentStatusIndicator

不依赖 SwiftBar 的原生 AI Coding Agent 托盘监控器。目标是复刻
[AgentStatusBar](https://github.com/DuRunzhe/AgentStatusBar) 的五态、多实例、上下文、通知和会话跳转能力，macOS 优先，并共用同一套 Windows/Linux 核心。

## 指定实现方案

采用 **Rust 单进程 + `tray-icon` 原生系统托盘**，不使用 Electron、SwiftBar、Python 或常驻 Node.js。`winit` 负责跨平台事件循环，`sysinfo` 负责低成本进程快照，Claude/Codex/OpenCode 的会话文件由 Rust 增量解析。Node 仅作为 npm 安装后的平台二进制启动包装，不参与常驻运行。

选择这套方案的原因：托盘本来就是轻量原生控件，使用 WebView 会额外引入渲染进程和几十到上百 MB 内存；纯 Swift 又无法共享 Windows/Linux 实现。Rust 可同时满足原生菜单、单二进制发布和低常驻资源占用。

### 架构

```text
系统进程表 ───────┐
Claude session ───┤
Codex rollout ────┼─> 2 秒增量采集器 ─> 状态优先级引擎 ─> 原生托盘菜单
OpenCode SQLite ──┤                              ├─> 原生系统通知
DeepSeek 进程 ────┘                              └─> 终端/浏览器聚焦
```

状态优先级与参考项目一致：等待确认 → 等待回复 → 进行中 → 就绪 → 已停止。工具调用必须按 ID 配对，只有未完成的 `request_user_input` / `AskUserQuestion` 判为等待回复，显式提权或完整终端确认提示判为等待确认。

### 依赖

| 依赖 | 用途 | 常驻额外进程 |
|---|---|---:|
| Rust 标准库 + `crossbeam-channel` | 采集/UI 解耦 | 0 |
| `tray-icon` + `winit` | macOS/Windows/Linux 原生托盘与事件循环 | 0 |
| `sysinfo` | 一次刷新进程树 | 0 |
| `serde_json` | 增量解析 JSON/JSONL | 0 |
| `notify-rust` | 系统通知（后续接入等待状态） | 0 |

Linux 运行时需桌面环境提供 AppIndicator/StatusNotifier 支持；Windows 使用系统通知区域；macOS 使用 NSStatusItem。

## 性能验收指标

指标以 release 构建、10 个活跃 Agent、累计 1 GB 会话日志为压力场景：

| 指标 | 目标 | 参考实现约束 |
|---|---:|---|
| 稳态 RSS（macOS） | ≤ 35 MB | 不高于 Node + SwiftBar/Python 组合 |
| 空闲平均 CPU | ≤ 0.5% | 2 秒采集，无每秒进程拉起 |
| P95 状态发现延迟 | ≤ 2.5 秒 | 当前约 2 秒轮询 |
| 托盘菜单打开 P95 | ≤ 50 ms | UI 不等待采集与磁盘读取 |
| 稳态磁盘写入 | 0 B/s | 状态保存在内存，仅配置落盘 |
| 长日志每轮读取 | 仅新增字节 | offset + inode/mtime 增量缓存 |
| 安装体积 | ≤ 15 MB（压缩后） | 单个 strip + LTO 二进制 |

CI 应在 macOS arm64/x64、Windows x64、Linux x64 上记录 RSS、CPU、扫描耗时和菜单更新时间；超过预算即阻止发布。

## 当前实现进度

- [x] 单进程原生托盘，无 SwiftBar
- [x] Claude/Codex/OpenCode/DeepSeek 多实例进程发现
- [x] 进程树任务活跃判定、2 秒异步刷新、五态数据模型
- [x] DeepSeek projection/session 状态、等待信号、模型与上下文解析
- [x] Codex Terminal 确认界面与后台任务正向状态纠正
- [x] 原生动态菜单、汇总图标、npm/Homebrew 发布骨架
- [x] 逐字节增量解析 Claude/Codex transcript，工具 ID 配对、模型与上下文
- [x] OpenCode SQLite 状态、模型和上下文读取
- [x] 等待态原生通知与 0/60/180 秒提醒
- [x] macOS 按 TTY 精确定位 Terminal/iTerm 标签页
- [x] 原生设置菜单、五项显示配置和 macOS 登录启动设置
- [ ] Windows/Linux 登录启动和聚焦适配

当前版本是可运行的第一阶段骨架，尚不能视为完整功能等价版本；上面未完成项是发布 `v1.0.0` 前的硬性范围。

## 开发运行

```bash
cargo run --release
cargo test
# 查看当前探测到的 Agent、会话匹配和状态（不启动托盘）
cargo run --release -- --diagnose
```

计划安装方式：

```bash
brew install DuRunzhe/tap/agent-status-indicator
npm install -g agent-status-indicator
```

npm 包在发布 CI 中携带对应平台预编译二进制，不要求用户安装 Rust 或 Node 原生编译工具链。Homebrew Formula 中的 URL 与 SHA256 会由 release workflow 自动写入，仓库模板中的占位 SHA 不可直接发布。
