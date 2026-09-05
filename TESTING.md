# macOS 测试使用说明

## 安装测试包

### npm 本地安装

```bash
npm install -g ./agent-status-indicator-0.2.10.tgz
agent-status-indicator
```

进程会常驻前台，菜单栏出现圆形状态图标。测试结束后在菜单中选择“退出”，或在启动终端按 `Control-C`。

卸载：

```bash
npm uninstall -g agent-status-indicator
```

### 独立二进制

```bash
tar -xzf agent-status-indicator-0.2.10-aarch64-apple-darwin.tar.gz
./agent-status-indicator
```

如果 macOS 阻止未签名程序，可在“系统设置 → 隐私与安全性”中选择仍要打开。当前测试包未公证，仅用于本机功能验证。

## 首次权限

- 点击 Codex/Claude 菜单项跳转终端时，macOS 可能要求允许控制 Terminal 或 iTerm2。
- Codex 确认界面探测需要读取对应 Terminal 标签页内容，也可能触发自动化权限提示。
- 开启通知后，按系统提示允许 AgentStatusIndicator 发送通知。

不授予自动化权限不会影响进程和会话文件检测，但确认界面识别及精确标签跳转会降级。

## 建议测试场景

1. 无 Agent 运行：顶部显示“无活动”，菜单保留 Claude、Codex、OpenCode、DeepSeek Harness 四条灰色“已停止”项，且不可点击。
2. 启动 Claude Code、Codex、OpenCode 或 DeepSeek Harness：菜单出现按项目区分的实例。
3. 让 Codex 执行普通工具：状态应为“进行中”。
4. 触发 Codex 提权确认：应在约 1 秒内显示“等待确认”。
5. 让 Codex 后台 terminal 继续运行：不得一直停留在“等待确认”。
6. 触发 `request_user_input`、`AskUserQuestion` 或 DeepSeek `ask_user_question`：应显示“等待回复”。
7. 点击实例：Terminal/iTerm2 应切换到对应 TTY 标签页。
8. 点击“设置 → 通知 → 点击开启通知”：应先显示说明弹窗，然后发送测试通知并打开系统通知设置；只有选择“已看到”后才显示为开启。进入等待状态时应立即通知，持续等待约 60 秒和 180 秒再次通知。
   在说明弹窗或验证弹窗中取消后，应用应继续运行，通知保持关闭；连续点击不得出现多个引导弹窗。
9. 退出后重新启动：通知开关应保持此前设置。
10. 在“设置 → 显示配置”中分别关闭时长、模型、上下文占比、已用和总上下文，实例行应立即更新且重启后保持。
11. 开启“设置 → 开机自启”，确认 `~/Library/LaunchAgents/com.agentstatusindicator.app.plist` 已生成；再次关闭后应删除。
12. 使用 `codex resume <thread-id>` 恢复旧会话：菜单应读取该 thread 对应 rollout 的状态、模型和上下文，而非同一目录下最近创建的其他 rollout；随后执行任务和完成任务时，状态应在约 1 秒内依次同步为“进行中”和“就绪”。
13. 同时存在等待回复、进行中和就绪实例时，顶部应显示三种状态的数量；已停止项不参与顶部汇总。

## 性能检查

## 状态诊断

当状态与终端不一致时，先运行：

```bash
agent-status-indicator --diagnose
```

它只输出当前进程、匹配的会话、状态、模型和上下文，不启动托盘或写入配置。请将输出与对应 Agent 终端的实际状态一并反馈。

需要确认托盘 UI 线程最后收到的状态时，使用调试模式启动：

```bash
agent-status-indicator --debug-ui
cat ~/.agent-status-indicator-ui.json
```

该文件仅在 `--debug-ui` 模式下更新，避免正常使用时持续磁盘写入。

找到 PID：

```bash
pgrep -x agent-status-indicator
```

观察 CPU 和内存：

```bash
ps -o pid,%cpu,rss,etime,command -p "$(pgrep -x agent-status-indicator)"
```

目标：空闲 CPU 不高于 0.5%，稳定 RSS 不高于 35 MB。首次扫描大量历史 Codex/DeepSeek 会话时允许短暂升高。

## 日志和问题反馈

当前测试版直接从终端启动，异常输出会显示在启动终端。反馈时请提供：

- macOS 与芯片型号
- 使用的 Agent 类型和启动终端
- 预期状态与实际状态
- 复现步骤
- 上述 `ps` 命令输出
