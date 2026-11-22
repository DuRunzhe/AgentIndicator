# macOS 测试使用说明

## 安装测试包

### npm 本地安装

```bash
npm install -g ./agent-status-indicator-0.2.52-alpha.1.tgz
agent-status-indicator
```

进程会常驻前台，菜单栏出现圆形状态图标。测试结束后在菜单中选择“退出”，或在启动终端按 `Control-C`。

卸载：

```bash
npm uninstall -g agent-status-indicator
```

### 独立二进制

```bash
tar -xzf agent-status-indicator-0.2.52-alpha.1-aarch64-apple-darwin.tar.gz
./agent-status-indicator
```

如果 macOS 阻止未签名程序，可在“系统设置 → 隐私与安全性”中选择仍要打开。当前测试包未公证，仅用于本机功能验证。

## 首次权限

- 点击 Codex/Claude 菜单项跳转终端时，macOS 可能要求允许控制 Terminal 或 iTerm2。
- Codex 确认界面探测需要读取对应 Terminal 标签页内容，也可能触发自动化权限提示。
- 开启通知后，按系统提示允许 AgentStatusIndicator 发送通知。

不授予自动化权限不会影响进程和会话文件检测，但确认界面识别及精确标签跳转会降级。

## 建议测试场景

1. 无 Agent 运行：顶部显示“无活动”，菜单保留 Claude、Codex、OpenCode、DeepSeek Harness、Pi、Gemini、Grok、Cursor 八条灰色“已停止”项，且不可点击。
2. 启动 Claude Code、Codex、OpenCode、Gemini CLI、Grok CLI、Cursor（编辑器或 `cursor-agent` CLI）或 DeepSeek Harness：菜单出现按项目区分的实例。
3. 让 Codex 执行普通工具：状态应为“进行中”。
4. 触发 Codex 提权确认：应在约 1 秒内显示“等待确认”。
5. 让 Codex 后台 terminal 继续运行：不得一直停留在“等待确认”。
6. 触发 `request_user_input`、`AskUserQuestion` 或 DeepSeek `ask_user_question`：应显示“等待回复”。
7. 点击实例：Terminal/iTerm2 应切换到对应 TTY 标签页。
8. 点击“设置 → 通知 → 点击开启通知”：应先显示说明弹窗，然后发送测试通知并打开系统通知设置；只有选择“已看到”后才显示为开启。进入等待状态时应立即通知，持续等待约 60 秒和 180 秒再次通知。
   在说明弹窗或验证弹窗中取消后，应用应继续运行，通知保持关闭；连续点击不得出现多个引导弹窗。
9. 退出后重新启动：通知开关应保持此前设置。
10. 在“设置 → 显示配置”中分别关闭已停止 Agent、时长、模型、上下文占比、已用和总上下文，实例行应立即更新且重启后保持；关闭“已停止 Agent”后，未运行 Agent 的灰色占位项应立即隐藏，再次开启后恢复。
    CPU 与内存在新建配置下应默认关闭：删除 `~/.config/agent-status-indicator/config.json` 后启动，表格只显示 Token 用量行，会话行上也不带 CPU/内存。打开“CPU 使用率”后表格应多出 CPU 行，同时会话行尾部出现带标签的 `· CPU 12.5%`；打开“内存占用”后同理出现 `· 内存 300M`——两处受同一个开关控制，且标签必须在，否则 `300M` 容易被当成 Token 数。
11. 开启“设置 → 开机自启”，确认 `~/Library/LaunchAgents/com.agentstatusindicator.app.plist` 已生成；再次关闭后应删除。
12. 使用 `codex resume <thread-id>` 恢复旧会话：菜单应读取该 thread 对应 rollout 的状态、模型和上下文，而非同一目录下最近创建的其他 rollout；随后执行任务和完成任务时，状态应在约 1 秒内依次同步为“进行中”和“就绪”。
13. 同时存在等待回复、进行中和就绪实例时，顶部应显示三种状态的数量；已停止项不参与顶部汇总。
14. 打开 DeepSeek Harness 桌面端并开始一个会话：菜单里应出现 `DeepSeek Harness (<项目>) · <会话标题>` 一行（与 ChatGPT、Pi 等行同格式，不再带“终端/桌面端”形态标记），状态在一次采集周期（约 2 秒）内变为“进行中”并显示模型与上下文；再开一个会话应新增一行，两行点击都应切到各自的会话（详见「DeepSeek Harness 桌面端验收 → 会话级跳转」）。
15. 在桌面端触发需要人工确认/选择（提问、审批）：对应行应显示“等待回复”或“等待确认”；继续生成后回到“进行中”，一轮结束后变为“就绪”。
15b. 让一个会话跑失败（例如在沙箱内触发一次失败命令，或断开网络让请求出错）：该行应变为红色“异常”并触发异常通知；随后在该会话里继续对话，新的事件写入日志后应恢复为“进行中/就绪”。
15c. 把某会话的 `permissions.approval` 设为 `never`（自动确认）：关闭“自动确认模式仍通知”后，该会话进入等待态时不应再发通知。
15d. 开启“设置 → 通知 → 通知：任务完成”，让一个会话跑完一轮（“进行中” → “就绪”）：应在变为“就绪”的时刻收到一次完成通知，点击通知跳回该会话；继续停在“就绪”不应反复提醒，关闭该开关后下一轮结束时不再通知。
16. 关闭桌面端：所有桌面端会话行应消失，只剩“已停止”的灰色占位项。
17. 启动 Gemini CLI 并发送一轮对话：菜单出现 `Gemini (<项目>) · <标题>`，模型与上下文在一次采集周期内出现；工具执行时显示“进行中”，需要确认的命令应显示“等待确认”，一轮结束显示“就绪”，最后一句是问句时显示“等待回复”，API 报错时显示“异常”。同一项目开两个 `gemini` 进程时，应各占一行且状态互不串台。在工具执行中按 `Ctrl+C` 中止且之后约 5 分钟没有新写入：行应回退到进程活跃度给出的状态（通常“就绪”），不得永久停在“进行中”。
18. 启动 Grok CLI 并发送一轮对话：菜单出现 `Grok (<项目>) · <标题>`，模型与上下文来自 `~/.grok/sessions/<编码项目>/<会话>/summary.json` 与 `signals.json`；工具执行/流式输出显示“进行中”，出现待处理交互显示“等待”，`turn_completed` 后显示“就绪”或“等待回复”，`error_kind` 显示“异常”。在一轮尚未 `turn_completed` 时中止，且约 1 分钟没有新写入：行应回退到进程活跃度给出的状态，不得永久停在“进行中”。
19. 启动 Cursor CLI（`cursor-agent`，或安装器链接的 `agent`）并发送一轮对话：菜单出现 `Cursor (<项目>) · <标题>`，标题与模型来自 `~/.cursor/chats/<工作区 MD5>/<会话>/store.db` 与 `meta.json`；有未完成的工具调用时显示“进行中”，工具结果写回并给出最终回答后显示“就绪”，最后一句是问句时显示“等待回复”。Cursor CLI 的 chat store 不记录 token，因此它不向用量表的 Cursor 列贡献，但 CPU/内存仍应汇总。
20. 上述三种 CLI 在项目目录里各开两个会话（或一个进程用 `/new`/新会话）：每个进程应绑定自己的会话文件，标题、模型和状态不得互相镜像。
21. 打开 Cursor 编辑器，在 Agent/Composer 里发起一轮对话：菜单出现 `Cursor (<项目>) · <标题>`，标题、模型和上下文来自全局 `globalStorage/state.vscdb` 的 `composerData`；生成中/工具执行显示“进行中”，出现阻塞待确认操作显示“等待确认”，完成后显示“就绪”，未读且最后一句是问句时显示“等待回复”，`error`/`failed` 记录显示“异常”，用户中止（`aborted`）回到“就绪”。同一工作区开多个 Composer 应各占一行。
22. 关闭 Cursor 编辑器：编辑器侧的行应消失；若 CLI 也未运行，只剩一条灰色“已停止”的 Cursor 占位项。
23. 打开“设置 → ChatGPT / Cursor 会话显示范围”，切到 15 分钟：超过 15 分钟且已结束的 Composer 行应消失，进行中/等待/异常的 Composer 必须保留；切回“全部”后历史 Composer 重新出现。超过 8 行时末尾应出现“N 个会话未显示”。
24. 在 Cursor 编辑器里让一个会话产生 token（新版 Cursor 的 `bubbleId` 会写 `tokenCount`），`--diagnose-usage --render` 的 Cursor 列应与这些 bubble 的 input/output 之和一致；旧版 Cursor 不写 token 时该列显示 `--`。

## Token 用量表验收

先在终端核对数字，再核对菜单：

```bash
agent-status-indicator --diagnose-usage --render
agent-status-indicator --diagnose-usage --render --usage-period week
agent-status-indicator --diagnose-usage --render --usage-period month
```

1. 打印的三行表格应与菜单里的表格逐字一致：表头上方是 `周期: ● 当日 ○ 当周 ○ 当月`，随后是 `输入/命中`、`输出`、`费用` 表头、八行 Agent（Claude、Codex、OpenCode、DeepSeek、Pi、Gemini、Grok、Cursor）与一行总计。
2. 打开菜单，确认表格位于会话列表下方、设置入口上方，列之间对齐；点击周期行应在“日 → 周 → 月 → 日”之间循环，箭头所指项随之改变，表格数字按新维度重算（≤10 秒内，通常 1 秒内）。
3. 与各 Agent 自带统计对数：
   - Claude Code：在会话里运行 `/status`（或 `/cost`）对比本次会话的 input/output token；本表的 `输入` 为 `input_tokens + cache_creation + cache_read`。
   - Codex：rollout 里最后一条 `token_count` 的 `total_token_usage` 与本表同一会话的累计一致（本表按事件差值累计，不是把多条累计值相加）。
   - Pi：对比 `/stats` 或 `~/.pi/agent/sessions` 里 assistant 消息的 `usage` 之和；`费用` 应为各条 `usage.cost.total` 之和。
   - OpenCode：`sqlite3 ~/.local/share/opencode/opencode.db "select sum(cost) from session"` 与本月/本周数字量级一致。
   - DeepSeek Harness：投影缓存 `rows.tokenUsage.val.totals` 与当日行一致；该源只有会话累计值，因此计入“会话最后活跃”的那个维度。
   - Gemini CLI：`~/.gemini/tmp/<项目>/chats/session-*.json[l]` 各 `gemini` 消息的 `tokens` 之和；本表 `输入` 为 `input + tool`，`命中` 为 `cached` 占比，`输出` 为 `output + thoughts`。
   - Grok CLI：`~/.grok/sessions/<编码项目>/<会话>/updates.jsonl` 中每个 `turn_completed.usage`（同一 `prompt_id` 只取最后一条）的 `inputTokens`/`outputTokens`；`费用` 为 `costUsdTicks / 1e10` 之和。
   - Cursor 编辑器：`state.vscdb` 的 `bubbleId:*` 每条 `tokenCount` 之和；本表 `输入` 为 `inputTokens`，`输出` 为 `outputTokens`，没有缓存/费用字段。
   - Cursor CLI：`store.db` 不记录 token，因此 CLI 不贡献该列；CPU/内存仍在汇总中。
4. 用空闲机器观察 1 分钟：`ps -o pid,%cpu -p "$(pgrep -x agent-status-indicator)"`，空闲 CPU 应仍不高于 0.5%（稳态一轮重算约 2 ms，仅 stat 未变化的文件）。
5. 在“设置 → 显示配置”关闭「Token 用量」：整块表格与周期行应立即消失，重启后保持关闭。
6. 运行一个 Agent 若干轮后等待 10 秒：对应行数字应增长；把系统时间改到次日再观察，日视图应从零重新累计，周/月视图仍包含前一天的数据。

## DeepSeek Harness 桌面端验收

桌面端是单个 Electron 应用进程，托盘按会话展开，读的是该进程所用 profile 的会话投影缓存。

```bash
agent-status-indicator --diagnose-deepseek-desktop
```

输出中的关键字段：

- `hosts`：识别到的桌面端宿主进程（`pid` 应为 Electron 主进程）与解析出的 `home`；`hosts` 为空说明进程识别失败，`home` 为 `null` 说明只能回退到 `~/.dsh`。
- `processes`：所有路径/命令行含 `deepseek` 的进程及其 `kind`/`host` 判定，用于确认助手进程（Helper、Renderer）没有被当成独立会话。
- `sessions`：读到的会话及其状态、标题、模型；`state` 与桌面端窗口里该会话是否在生成一致。
- `timing`：`refreshColdMs`（首次读全部会话）、`refreshWarmMs`（无变化时）、`refreshWarmParsed`（无变化时应为 0）、`sessionsMs`。稳态 `refreshWarmMs` 应在 3 ms 以内，`refreshWarmParsed` 应为 0（不重复解析未变化文件）。

非默认 `DSH_HOME` 启动的桌面端：`hosts[].home` 应指向该目录，`sessions` 应来自其 profile，而不是 `~/.dsh`。

会话显示范围：把“设置 → DeepSeek Harness → 会话显示范围”切到 15 分钟，长时间未使用的已结束会话应从菜单消失，进行中/等待中的会话必须保留。

### 会话级跳转

设置里与桌面端相关的一切都在同一个分组「设置 → DeepSeek Harness」下：会话显示范围（子菜单）、会话级跳转开关、一行权限入口（未授权时文案为「会话级跳转需要辅助功能授权，点击前往授权」，已授权时变为「打开辅助功能设置」），以及「测试会话级跳转」自检行。

权限只在用户主动要这个功能时才去要，三处时机必须分清楚：

- **启动托盘**：不得出现任何系统授权提示，也不得自动打开系统设置（即使上次是未授权状态）。
- **点击任意桌面端会话行**：不得中途插入任何确认框；未授权时直接退化为第 3 级（只把窗口带到前台），已授权时直接跳到会话。
- **在未授权状态打开「会话级跳转」开关（首次）**：只出现一个系统授权提示，不得同时把系统设置面板也打开；点提示里的「打开系统设置」按钮才跳到辅助功能面板，此时 App 已在列表里，勾选即可。
- **再次点击那行授权入口**：直接打开辅助功能面板，**不得**再弹系统提示（提示每个签名只弹一次，标志已落盘为 `deepseek_accessibility_prompted`）。
- 勾选后同一分组的授权行文案应从「会话级跳转需要辅助功能授权，点击前往授权」变为「打开辅助功能设置」。
- 已授权状态再关闭并重新打开「会话级跳转」开关：不得再弹任何提示，也不得跳转面板。
- 验收标准：任何一次点击最多只出现一个界面。

点击任意桌面端会话行应直接切到该会话，而不是只把窗口带到前台。行为按能力逐级降级，每一级都能单独验证：

1. **原生路由**（`dsh://session/<id>`）：当前所有已发布版本都不支持，`--diagnose-deepseek-desktop` 的 `sessionJump.nativeRoute` 应为 `false`。上游一旦支持，`src/deepseek_jump.rs` 里的 `NATIVE_ROUTE_MIN_VERSION` 改成对应版本号即可启用，行上的地址已经就是这个形状。
2. **辅助功能**：托盘在应用的侧栏里按标题找到该会话的行并“按下”，随后读窗口标题确认切换成功。
3. **仅窗口**：前两级都不可用时退化为 `dsh://open`（还原最小化窗口、重建被关闭的窗口），与历史行为一致。

验证步骤：

- **授权链路**：设置 → DeepSeek Harness →「会话级跳转需要辅助功能授权，点击前往授权」。首次点击会触发系统弹窗把 AgentStatusIndicator 加入“辅助功能”列表，确认弹窗后系统设置定位到该面板，手动打开开关；授权后同一行文案应变为「打开辅助功能设置」（这一行始终存在，方便随时回到该面板）。
- **一键自检**：设置 → DeepSeek Harness →「测试会话级跳转」应弹框给出结论（可用 / 只有窗口）。结论与 `session-jump.log` 的 `jump route=` 行一致。也可用冒烟用例验证（会真实操作桌面窗口，因此默认 `ignored`）：
```bash
cargo test -- --ignored smoke_press_session
cargo test -- --ignored smoke_open
```
- **成功切换**：菜单里有两个不同标题的会话行时，依次点击，窗口标题与内容应各自切换；第 2 级生效时 `--diagnose-deepseek-desktop` 的 `sessionJump.rungs` 应为 `["accessibility","window"]`。
- **同名会话**：在同一项目下建两个同名会话（例如都叫“同步最新代码”），点击应落在所属项目（workspace）的那一行；行地址里带 `workspace=` 参数就是为了这个。
- **窗口不在前台/最小化**：最小化后点击应还原并切换；用关闭按钮关掉窗口（应用仍驻留）后点击应重建窗口再切换。
- **降级（关掉开关）**：设置 → DeepSeek Harness → 「点击关闭会话级跳转」后点击任意会话行，只把窗口带到前台，`sessionJump` 相关行为不再触发。
- **降级（未授权）**：在系统设置里撤销辅助功能授权，重新点击会话行应仍然把窗口带到前台（不弹错误、不卡住）。
- **降级（找不到会话）**：会话从侧栏消失（例如在应用内删除或切到别的 project）后点击该行，应在数秒内退化为窗口聚焦。

调试与复现：

```bash
open dsh://open                                     # 仅窗口（第 3 级），等价于 `open_window()`
open "dsh://session/<id>?title=<标题>&workspace=<项目名>"   # 带会话的地址（第 1 级的形状；当前版本会走第 2/3 级）
```

上游限制（本轮实现绕开的正是这一层）：桌面端 shell 的 `open-url` 只处理 `dsh://open` 与 `dsh://open/`，其它 `dsh://` URL 被静默忽略且 `open` 仍返回 0；前端（`dsh-web-frontend`）也没有任何 URL 路由（bundle 里不存在 `location.search`/`location.hash`/`pushState`）。因此第 1 级目前只能“按版本号预判”，不能靠探测确认——这也是它默认关闭、并保留第 2 级作为过渡的原因。

辅助功能这一级依赖 Electron 应用侧的 UI 结构（`AXOutline` → 工作区分组 → 会话行），属于“用 UI 结构当契约”：DSH 改版后可能失效，失效时的表现是第 3 级（只带出窗口），不会误跳——因为每次按下都会用窗口标题校验目标。

## 性能检查

## 命令行参数

不带参数启动托盘；以下命令只打印结果并退出，可用于确认装的是哪个版本、以及这个版本接受哪些参数：

```bash
agent-status-indicator --version
agent-status-indicator --help
```

再拿一个不存在的参数确认它被拒绝（退出码 2，不启动托盘、也不会在 AppKit 内崩溃）：

```bash
agent-status-indicator --bogus; echo "exit=$?"
```

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
- 进程死亡且终端里没有输出时，缓存目录下的 `agent-status-indicator/panic.log`（Windows 为 `%LOCALAPPDATA%\agent-status-indicator\panic.log`）

## Windows 托盘稳定性验收

托盘每次刷新都会重设行状态，而每个图标位图都是一个 GDI 对象，所以这里既要验“点得动”，也要验“放得久”。

1. 从安装包启动托盘，在任务管理器「详细信息」页加上「GDI 对象」和「句柄」两列。
2. 静置 10 分钟不操作：GDI 对象数应基本平稳，不再以每次刷新约 19 个的速度增长。
3. 反复点击托盘图标打开菜单，逐行走一遍会话行、各个设置开关、语言、期间选择（Quit 除外）：进程应始终存活，菜单不得消失。
4. 点击菜单外部关闭菜单（不选任何行）：进程应继续运行。
5. 托盘仍然退出时读 `%LOCALAPPDATA%\agent-status-indicator\panic.log`：有 `panicked at` 行就整行贴进反馈；没有该文件说明是系统结束了进程，需要附事件查看器的应用程序错误。

## Codex 确认状态回归

确认识别优先使用会话工具信号，Terminal 界面探测作为兜底。界面解析按能力/格式兼容，不按 Codex 版本号分支：要求底部确认/取消按键提示、连续编号及选中项，并检查允许/拒绝快捷键；无快捷键的旧格式保留 Yes/No 兼容。未知格式不猜测为等待确认。未返回的 `apply_patch` 本身不是审批证据。

- `cargo test terminal::tests` 覆盖旧版提示、文件修改审批、文案变化、本地化快捷键提示、历史提示、普通菜单和 TTY 隔离。
- 在 Terminal 中保留一个真实 Codex 确认框，使用其进程 PID 运行只读集成验证（需要自动化权限）：

  ```bash
  ASI_TEST_CODEX_PID=<pid> cargo test terminal::tests::live_terminal_confirmation -- --ignored
  ```

- 接受或取消后，确认托盘恢复后续状态；另一个 TTY 中的执行任务不得被该确认框影响。
- Terminal 脚本必须通过具体 `tab ... of window ...` 读取 `contents`；通过 repeat 引用变量读取可能返回 tab 对象并触发 `-1700` 转换错误。
- 当前界面兜底针对 macOS Terminal；没有终端读取权限、其他终端或全新提示布局时，仍依赖会话日志已有信号，不能保证识别所有审批。

确认框保持未处理至少 20 秒（跨越多次 5 秒探测缓存刷新），状态应持续为等待确认，不应重复触发首次通知；1 分钟和 3 分钟的既有提醒仍保留。确认处理后，新日志活动或明确的终端执行信号应解除等待状态。

## Linux 启动（自动化冒烟）

`ci.yml` 在 `ubuntu-latest`（x86_64）与 `ubuntu-24.04-arm`（ARM64）上用 `xvfb-run` 启动托盘，并在 20 秒内等待 `--debug-ui` 写出 UI 快照：进程必须仍然存活，日志里不得出现 `GTK has not been initialized` 或 `panicked`。GTK 初始化、菜单构建或事件循环的回归都会让该 job 失败。真实的托盘图标与菜单点击仍需按下面的手动步骤在 Linux 机器上验收。

### Linux 托盘图标（容器内验证，无需桌面）

没有 Linux 桌面时，虚拟显示加一个 XEmbed 托盘宿主就能验证图标与菜单是否正确（issue #2 的回归点）：

```bash
sudo apt install -y xvfb stalonetray imagemagick xdotool dbus-x11 libxkbcommon-x11-0
Xvfb :99 -screen 0 1280x800x24 &
eval "$(dbus-launch --sh-syntax)"
DISPLAY=:99 stalonetray --geometry 5x1-0+0 --icon-size 24 &
DISPLAY=:99 ./target/debug/agent-status-indicator &
sleep 8
DISPLAY=:99 import -window root /tmp/shot.png      # 右上角应出现托盘图标
DISPLAY=:99 xdotool mousemove <图标坐标> click 3   # 右键应弹出完整菜单
```

应用在找不到 StatusNotifier host 时会回退到 XEmbed，由 stalonetray 负责显示；菜单激活由事件循环里的 GTK 迭代派发，所以点击 “Quit” 应当让进程退出。这两点任缺其一（未 `gtk::init`、未泵 GTK 主循环），图标或菜单就会是空白。

### Homebrew on Linux（手动）

在 Ubuntu 24.04 的 x86_64 与 ARM64 上分别执行：

```bash
brew tap DuRunzhe/agentindicator https://github.com/DuRunzhe/AgentIndicator
brew trust DuRunzhe/agentindicator
brew install DuRunzhe/agentindicator/agent-status-indicator
agent-status-indicator --version   # 版本号应与 release tag 一致
ldd "$(brew --prefix)/opt/agent-status-indicator/bin/agent-status-indicator" \
  | grep "not found" || echo "all shared libraries resolved"
agent-status-indicator             # 桌面会话内应出现托盘图标，右键能展开菜单
```

公式在 Linux 上会安装 `libayatana-appindicator`（带 GTK3）、`xdotool` 与 `zstd`；Homebrew 安装时会给二进制补上 rpath，托盘后端 `dlopen` 才能找到 Homebrew 目录里的 libayatana-appindicator3。若托盘图标不出现，先确认桌面会话提供 StatusNotifier host（GNOME 需 AppIndicator 扩展），且不要在 `brew services` 的 systemd user unit 里启动（它不继承显示变量）。验证完成后 `brew uninstall DuRunzhe/agentindicator/agent-status-indicator` 清理。

## 终端聚焦手动验收（Windows / Linux）

托盘菜单点击会话项时，应把承载该 Agent 的既有终端窗口带到前台，而不是新开终端。

### Windows

1. 在 Windows Terminal 中启动任一被监控 Agent（如 Claude Code），多开几个标签页。
2. 切到其他应用，点击托盘菜单里该会话项：应切回 Windows Terminal 并置前对应窗口。
3. 把该终端窗口最小化后重复上一步：窗口应先还原再置前。
4. 用传统 conhost（cmd 窗口）重复第 2 步：应置前对应 conhost 窗口。
5. 找不到所属窗口时（如宿主无窗口），应安全降级为启动新的 Windows Terminal，且不崩溃。

### Linux（X11）

1. 安装 xdotool（`sudo apt install xdotool`），在 GNOME Terminal / Konsole 等终端中启动被监控 Agent。
2. 切到其他窗口，点击托盘菜单里该会话项：应置前承载该会话的终端窗口（即使窗口属于终端模拟器进程而非 shell 子进程）。
3. 最小化后重试：应还原并置前。

### Linux（降级路径）

1. Wayland 会话（`echo $XDG_SESSION_TYPE` 输出 wayland）下点击：跳过 xdotool，直接启动默认终端（Wayland 无通用窗口激活接口，属预期限制）。
2. X11 会话但未安装 xdotool 时点击：安全降级为启动 x-terminal-emulator / gnome-terminal。
