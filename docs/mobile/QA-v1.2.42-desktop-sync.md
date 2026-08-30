# Mirror X Codex v1.2.42 Desktop Sync QA

日期：2026-08-14  
平台：Windows  
分支：`agent/windows-only-release`  
基线 HEAD：`7ab1354`  
发布约束：未 commit、未 push、未发布公网 Relay、未修改中转站生产服务

## 1. 验收结论

v1.2.42 的 Desktop Sync 主链路已在本机通过代码级、单元、live、构建和移动视口验证：

```text
Phone PWA
  → encrypted WebSocket
Relay
  → Manager MobileRelayHost
Codex Desktop CDP bridge
  → Desktop dispatcher
Desktop-owned Codex App Server
```

手机与 Desktop 共享同一 App Server writer。手机可以实时接收 Desktop 当前任务的状态与输出，不再要求等任务完成后重新读取历史。

## 2. 本轮修复和新增

### 2.1 Desktop 同步

- 新增 `crates/codex-plus-core/src/desktop_sync.rs`。
- 通过 CDP 定位 Codex Desktop renderer。
- 在 renderer 中发现 Desktop dispatcher。
- 手机请求以 `mcp-request` 进入 dispatcher。
- Desktop 的 `mcp-notification` 和 `mcp-response` 实时回传手机。
- `desktopSync` 模式不再因 active writer 自动 fork 原会话。

### 2.2 active turn 安全

- 手机晚连接时恢复 active turn 和 `turnId`。
- Desktop 正在工作时，手机新消息进入 `queued` 状态。
- 当前 turn 完成后自动提交排队消息。
- stop 请求携带 `threadId + turnId`。
- 避免手机与 Desktop 对同一 writer 并发发起第二个 `turn/start`。

### 2.3 断线恢复

- Relay 重启后 Desktop reader 不再退出。
- Relay 先恢复、Host 后恢复时，手机不退回 Key 页面。
- `HOST_OFFLINE` 和 `RATE_LIMITED` 时保留工作台、会话与内容。
- 重连使用指数退避，避免 Host 未上线时触发限流风暴。
- 重复网络提示已节流。
- 恢复后状态明确显示“正在实时同步电脑 Codex”。

### 2.4 手机体验和格式

- 项目列表与历史会话可直接加载，无须预先在手机打开。
- 抽屉可打开、关闭并返回会话。
- 任务进行中有 loading/运行状态。
- Markdown 标题、列表、表格、代码块、链接和 `[!image]` 占位兼容。
- 覆盖竖屏与横屏布局。

## 3. 自动化和 live 验证

| 测试范围 | 结果 |
|---|---|
| Core | 159 passed，1 ignored live test |
| Relay | 9/9 |
| Manager Windows subsystem | 23/23 |
| Desktop dispatcher 真实只读 live test | 通过 |
| Host + 本地 Relay + Desktop Sync live test | 通过 |
| Relay 整进程重启后同 session 恢复及继续 RPC | 通过 |
| Manager TypeScript check | 通过 |
| Manager Vite build | 通过 |
| 端到端加密 | 通过 |
| 房间隔离 | 通过 |
| 错误 token | 通过 |
| `CLIENT_REPLACED` | 通过 |
| 手机消息排队、停止、自动发送 | 通过 |
| Markdown/表格/代码块/链接/`[!image]` | 通过 |

说明：真实 live test 使用本地 Relay 和本机 Codex Desktop。测试 Relay 与 mock Host 在测试后已停止，没有操作公网生产 Relay。

## 4. 移动视口证据

以下截图均位于：

```text
D:\mirror++\CodexPlusPlus\output\playwright\
```

这些截图使用本地演示 Host，证明手机页面的运行状态、排队、抽屉、横竖屏和恢复布局。它们不单独证明 Desktop dispatcher 链路；Desktop Sync 的链路证据来自第 3 节的真实 dispatcher/live test。

| 截图 | 视口/内容 |
|---|---|
| `desktop-sync-active-390x844.png` | 390×844，active/loading 状态 |
| `desktop-sync-queued-390x844.png` | 390×844，繁忙期间手机消息排队 |
| `desktop-sync-drawer-open-390x844.png` | 390×844，项目/会话抽屉 |
| `desktop-sync-landscape-844x390.png` | 844×390，手机横屏 |
| `desktop-sync-portrait-412x915.png` | 412×915，手机竖屏 |
| `desktop-sync-recovered-412x915.png` | 412×915，恢复后的页面和输入状态 |

不要使用 `desktop-sync-complete-390x844.png` 作为证据：该次 Playwright 页面意外跳转到 `about:blank`，不属于有效截图。

## 5. 本机安装证据

安装文件：

```text
D:\mirrorplus\mirror-x-codex-manager.exe
```

验证值：

- FileVersion：`1.2.42`
- ProductVersion：`1.2.42`
- SHA256：`31CE7A94E5BF946A7A5CC9143C38514457E44BB4190B0A3EAE5320064F5E00FA`
- 验证时运行 PID：`13628`
- 运行路径：`D:\mirrorplus\mirror-x-codex-manager.exe`

PID 只代表本次验证时的进程，不作为长期固定值。

## 6. 回滚

回滚备份目录：

```text
D:\mirrorplus\backups\20260814-215227-desktop-sync-v1.2.42\
```

目录内两个旧版备份的 SHA256：

```text
ECBDC5A73B504B7F39A0960AE36B828B89665911F1CE526592782F48D85A5232
```

回滚原则：

1. 退出当前 Manager。
2. 保留当前 v1.2.42 文件作为故障证据，不直接删除。
3. 从上述备份目录恢复旧版可执行文件。
4. 再核对文件版本、SHA256 和实际运行路径。
5. 不修改用户 `.codex`、项目文件或 Mirror X 配置。

## 7. 客户演示步骤

1. Windows 电脑启动 Codex Desktop。
2. 启动 `D:\mirrorplus\mirror-x-codex-manager.exe`。
3. 打开 Manager 的手机控制，确认 Host 已连接。
4. 手机扫码或打开测试配对页，输入该用户自己的 Key。
5. 打开一个已有项目和会话。
6. 在 Desktop 发起一个需要持续输出的任务。
7. 手机应立即显示运行状态，并持续出现同一任务输出。
8. Desktop 仍在运行时，从手机提交下一条消息。
9. 手机显示排队；Desktop 当前 turn 完成后，该消息自动发送。
10. 打开和收回左侧抽屉，切换历史会话，再回到当前会话。

## 8. 商业承诺边界

### 可承诺

- Windows 条件满足时，手机可访问本机项目和会话。
- `desktopSync` 可实时同步 Desktop App Server 的任务事件与输出。
- 繁忙时不会盲目并发写入，手机消息采用排队策略。
- 短时 Relay/Host 中断后自动恢复，并保留手机当前工作台。
- 不同 Key 派生的房间相互隔离，中继只转发密文。

### 不可承诺

- 不是桌面画面、鼠标和窗口的像素级镜像。
- 电脑关机、休眠或 Desktop/Manager 不运行时不能继续本地执行。
- 不能保证所有公网网络环境永不掉线。
- 单房间不支持多个手机或标签页同时写入。
- macOS 本轮没有构建和真机验证。
- 公网 Relay 本轮没有升级，不能把本机验证等同于线上已交付。

## 9. 发布前门禁

- Android 与 iPhone 各完成至少一次真机流程。
- 使用 canary Relay 验证 WSS、Nginx 路径、限流和恢复。
- 验证公开环境的旧客户端兼容。
- 准备 Relay 回滚二进制、配置和切流步骤。
- 连接排空后再切换正式 Relay。
- 核对最终安装包版本、SHA256 和下载来源。
