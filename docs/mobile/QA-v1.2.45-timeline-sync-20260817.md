# Mirror X Codex 手机任务时间线与同步性能 QA

> 日期：2026-08-17  
> PWA 标记：`v20260817.1`  
> 范围：本机隔离验证 + 独立公网 canary；未切换正式 `/relay`

## 1. 用户问题

用户实际使用 Codex 时，会先看到可见的思考摘要、执行进度、命令和工具过程，最后再得到结论。旧手机端存在四个问题：

1. 快速会话索引成功后仍等待完整磁盘历史扫描，首次进入慢。
2. 打开会话时 `thread/read` 和 `thread/turns/list` 串行执行，延迟叠加。
3. `commentary`、`final_answer`、plan 和 reasoning 被混入同一个流式字符串，过程与结论无法区分。
4. 每个小增量都重新解析完整 Markdown，长代码和长回复产生近似 O(n²) 的重复渲染。

## 2. 已实现

- 快速索引成功后立即进入工作台，完整磁盘历史在后台补齐。
- `thread/read` 与 `thread/turns/list` 并行启动。
- 每个会话保留内存快照，切回已看过的会话先显示旧快照，再后台更新。
- 按 Codex `MessagePhase` 区分：
  - `commentary`：执行过程；
  - `final_answer`：最终结论；
  - 无 phase 的旧历史：仅把最后一个无 phase 的 agent message 作为兼容结论。
- 支持 reasoning summary、reasoning text、plan 和 agent message 增量。
- 流式 Markdown 最多每 90ms 合并重绘一次，避免每个 token 全量重绘。
- 执行中，命令、子 Agent、进度和思考摘要统一进入打开的“当前执行过程”。
- 完成后，过程自动变为关闭的“思考与执行过程”，最终结论单独展示。
- 命令输出仍可二次展开，默认不占满手机屏幕。

## 3. 自动化验证

| 项目 | 结果 |
| --- | --- |
| `node scripts/mobile_pwa_format_check.mjs` | Pass |
| `node scripts/mobile_reconnect_check.mjs` | Pass |
| `py -3 -m py_compile scripts/mobile_pwa_mock_host.py` | Pass |
| `cargo test -p codex-plus-mobile-relay` | 11/11 Pass |
| `git diff --check` | Pass；仅既有 CRLF 提示 |

新增回归覆盖：

- completed turn 的 process/final 分组；
- active turn 的对象型 `status: { type: "inProgress" }`；
- interrupted turn 不把明确标记为 `commentary` 的消息误判为结论；
- 无 phase 旧历史的最后 agent message 兼容结论；
- reasoning summary 事件和 90ms 合并渲染标记。

## 4. 浏览器真实流程

环境：

- Chromium
- 视口：`390 × 844`
- 独立公网 canary：`/relay-canary-v1245/`
- 临时隔离 Key 派生的加密 Mock Host
- `desktopSync` 模式

验证结果：

1. 首屏先调用快速 `thread/list(useStateDbOnly=true)`。
2. 快速列表返回后约 64ms 即开始并行读取 thread metadata 和 turns；完整历史扫描在后台执行。
3. 执行中的任务显示一个打开的“当前执行过程”，包含 4 项：
   - 命令执行；
   - 子 Agent；
   - Codex 进度；
   - 思考摘要。
4. 完成后同一任务显示关闭的“思考与执行过程 · 4 项”。
5. 最终结论独立显示，Markdown 标题、列表、任务项、引用、表格、代码和图片占位正常。
6. 点击过程面板可以恢复查看全部过程。
7. 浏览器控制台：0 error、0 warning。
8. 手机抽屉可从左上角打开，再由顶部关闭按钮收回；关闭后输入框与附件按钮保持可见。
9. active turn 再次发送实际调用 `turn/steer`，包含 `expectedTurnId`；停止后再次发送实际调用 `turn/start`。

证据：

- `output/playwright/v20260817.1-timeline/live-process-group-390x844.png`
- `output/playwright/v20260817.1-timeline/completed-process-collapsed-390x844.png`
- `output/playwright/v20260817.1-timeline/events-final-2.jsonl`
- `output/playwright/v20260817.1-public-canary-timeline/active-process-drawer-closed-390x844.png`
- `output/playwright/v20260817.1-public-canary-timeline/active-process-drawer-open-390x844.png`
- `output/playwright/v20260817.1-public-canary-timeline/completed-process-final-markdown-390x844.png`
- `output/playwright/v20260817.1-public-canary-timeline/events.jsonl`

## 5. 公网 Canary 与真实 Host

- canary Relay 健康检查：`status=ok`，版本 `1.2.45`。
- canary PWA 标记：`v20260817.1`。
- `/status` 仅返回遮罩后的 room 标识。
- 本机 `D:\mirrorplus\mirror-x-codex-manager.exe` 自动保持 Host 在线。
- 真实安装 Host 已完成 App Server 初始化并读取 20 个会话。
- 17,852,851 字节安装包经 69 个加密分块下载完成，文件 SHA-256 一致。
- 验证脚本使用 30 秒无进度超时和 300 秒总上限，避免把持续有进度的慢速公网传输误报为失败。
- 正式页面仍为 `v20260815.5`，本轮未切换正式 `/relay`。

回滚：

- Relay：`/root/mirror-x-relay-canary/v1.2.45/codex-plus-mobile-relay-linux-x64.before-v20260817.1`
- PWA：`/var/www/mirror-x-mobile-canary-v1245.before-v20260817.1/`

## 6. 边界

- 手机只能显示 Codex App Server 实际公开的 commentary、reasoning summary/text、plan、工具与消息事件；不能承诺展示模型未公开的内部隐藏推理。
- 当前结果已发布到独立 canary，未替换正式 `/relay`，未影响现有正式用户。
- Windows 真实 Host、加密会话、会话读取和 17.8MB 文件下载已验证；Android、iPhone 的扫码、软键盘、锁屏切网和长时间真机使用仍需店主实机验收。
