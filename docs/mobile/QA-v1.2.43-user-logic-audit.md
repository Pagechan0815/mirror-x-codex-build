# Mirror X Codex v1.2.43 用户逻辑审计与 QA

日期：2026-08-14  
平台：Windows  
分支：`agent/windows-only-release`  
基线 HEAD：`7ab1354`  
约束：未 commit、未 push、未发布公网 Relay、未修改镜子AI生产服务

## 1. Product Goal Understanding

目标不是做一个缩小版远程桌面，而是让普通用户在电脑开机并运行 Codex Desktop 与 Mirror X Codex 时，可以用手机：

1. 直接看到本机项目、文件与历史会话。
2. 打开从未在手机端或当前 Desktop 窗口中预先打开的会话。
3. 实时看到 Desktop App Server 正在执行的任务状态与输出。
4. 在任务进行中安全提交下一条指令，不破坏同一个 active turn。
5. 网络短时中断后保留当前页面、输入和待发送状态，并自动恢复。
6. 明确知道当前是连接中、运行中、排队中、停止中、重连中还是初始化失败。

主路径：

```text
Phone PWA
  → encrypted WebSocket
Relay
  → Manager MobileRelayHost
Codex Desktop dispatcher
  → Desktop-owned Codex App Server
```

## 2. User Journey Map

| 阶段 | 用户动作 | 正常反馈 | 失败反馈与恢复 |
|---|---|---|---|
| 准备 | 电脑启动 Codex Desktop 和 Manager | Manager 显示手机控制状态 | 缺 Key 时提示先配置 Key |
| 配对 | 手机扫码或输入同一 Key | 建立端到端加密通道 | Key 错误时清除旧配对并要求重扫 |
| 初始化 | 手机读取历史和项目 | 自动打开上次或最近会话 | 快速索引失败自动回退完整历史 |
| 浏览 | 打开抽屉、项目、文件、历史会话 | 无须 Desktop 预先打开会话 | 完整历史失败显示“立即重试” |
| 执行 | 手机发送任务 | 显示工作状态和增量输出 | 离线时保留输入，不允许假发送 |
| 繁忙 | Desktop 正在执行时手机发送 | 新消息排队，当前 turn 完成后发送 | active writer 错误继续排队并轮询 |
| 停止 | 用户点停止 | 按钮显示“停止中…” | 缺 `threadId` 或 `turnId` 时不发模糊请求 |
| 断线 | 锁屏、切网、Relay/Host 短时中断 | 保留工作台并自动恢复 session | 超过恢复能力时显示明确可重试状态 |
| 退出 | 用户点断开连接 | 清除本手机配对和 session | 不修改电脑 `.codex`、项目或模型配置 |

## 3. 已发现并修复的用户逻辑漏洞

### S1：错误会话写入与并发写入

- `ensureThreadReady()` 原使用全局 readiness，切换会话期间可能将消息发往错误 thread。
- `turn/start` 返回成功到 `turn/started` notification 到达之间存在并发窗口。
- 双击发送或快速重复提交可能覆盖 pending submission。
- 修复为每个 thread 独立 runtime，并增加 `sendInFlight`、pending guard 和成功 RPC 后立即标记 active。

### S1：待发送消息可能永久卡住

- active turn 异常结束时，queued message 原本可能不再触发发送。
- 新增周期性 `thread/read` 核对 writer 状态；空闲后自动提交。
- active-writer 错误不再把消息丢回输入框或直接丢弃。

### S1：相同文本误判

- 用户连续发送“继续”等相同文本时，旧逻辑可能把历史中的上一条当作本次已送达。
- pending submission 记录发送前用户文本数量，并优先使用 `clientUserMessageId`。
- 浏览器验证连续两次“继续”产生两个不同 client ID 和两个 `turn/start`。

### S1：停止请求目标不明确

- 没有 `turnId` 时原逻辑仍可能发送停止。
- 现在先读取 active turn，必须同时具备 `threadId + turnId` 才执行。
- 按钮进入“停止中…”状态并阻止重复点击。

### S1：快速重启 Host 的旧状态覆盖新状态

- 旧 runtime reporter 可能在新 Host 已启动后写回“已停止/断线”。
- Host 增加 generation；旧 generation 的状态更新被丢弃。
- restart/切换 Relay URL 前等待旧 runtime 完整停止。

### S2：初始化假死

- 快速 State DB 历史查询失败时，页面原本直接进入“初始化失败”。
- 现在自动回退完整历史；仅两条路径都失败且无旧数据时才失败。
- 失败页提供“立即重试”，并自动重试两次。
- 已进入工作台时初始化失败不会清空当前内容。

### S2：首次连接停在空欢迎页

- 没有 previous thread 或 active task 时，用户连接成功后仍要自己打开抽屉选会话。
- 现在按“上次会话 → Desktop active 会话 → 最近会话”的顺序自动打开。

### S2：重连后任务状态丢失

- session resume 后主动读取 thread，恢复 active 状态和 `turnId`。
- active writer 未完成时不会误发 queued submission。

### S2：手机抽屉和登录页控件异常

- 登录页三横杠被 `display: block !important` 错误显示，现由 `[hidden]` 规则覆盖。
- 手机三横杠可打开抽屉；顶部关闭按钮和抽屉内关闭按钮均可收回。

### S2：Relay 输入与状态暴露

- room/token 现在必须是 32 位小写十六进制。
- 单帧最大 2 MiB。
- Ping/Pong/Frame 不再转发给另一端。
- `/status` 仅返回脱敏 room ID。
- Manager 与 Host 诊断日志也不再记录完整 room ID。

## 4. 测试矩阵

### 4.1 正常场景

| 场景 | 结果 |
|---|---|
| 输入正确 Key 并连接 | 通过 |
| 首次连接自动打开最近会话 | 通过 |
| 打开未预先加载的历史会话 | 通过 |
| 连续发送两条相同文本 | 通过，2 个独立 `turn/start` |
| 双击发送 | 通过，仅 1 个 `turn/start` |
| Desktop active turn 实时同步 | live test 通过 |
| active turn 期间发送下一条 | 排队并在空闲后发送 |
| 精确停止当前 turn | `threadId + turnId` 均存在 |
| Markdown、表格、代码、链接、`[!image]` | 通过 |

### 4.2 异常场景

| 场景 | 结果 |
|---|---|
| 快速历史索引首次失败 | 自动回退完整历史 |
| 所有历史查询失败 | 明确失败页，可手动重试 |
| Host 暂时离线 | 保留页面并重试 |
| Relay 整进程重启 | Host 和手机恢复同一 session |
| 错误 token | `TOKEN_MISMATCH` |
| 外部 Key | 房间隔离且无法解密 |
| 第二个手机/标签页接管 | 原客户端收到 `CLIENT_REPLACED` |
| 连续点击停止 | 防重复提交 |

### 4.3 边界与破坏性场景

| 场景 | 结果 |
|---|---|
| 畸形或超长 room/token | 拒绝 |
| 超过 2 MiB 单帧 | 拒绝 |
| 浏览器刷新/锁屏恢复 | session ID 保留 |
| 同一 active writer 并发任务 | 第二条进入排队 |
| 快速停用再启用手机控制 | generation 防旧状态污染 |
| 手机主动断开 | 清除手机本地配对，不修改电脑数据 |

## 5. 自动化与 live 证据

| 范围 | 结果 |
|---|---|
| Relay unit tests | 11/11 |
| Mobile Host unit tests | 17/17 |
| Manager Windows subsystem | 24/24 |
| Desktop Sync unit tests | 2 passed，1 ignored manual live |
| Host + Relay + Desktop live recovery | 1/1 |
| Mobile Relay E2E | 全部通过 |
| reconnect transport check | 通过 |
| crypto fallback vectors | 通过 |
| Manager TypeScript check | 通过 |
| Manager Vite build | 通过 |
| release build | 通过 |
| `cargo fmt --check` | 通过 |
| `git diff --check` | 通过 |
| `node --check app.js` | 通过 |

浏览器测试视口：`390×844`。本轮验证了最近会话自动打开、重复文本、双击发送、初始化失败重试和格式渲染。事件证据：

```text
D:\mirror++\CodexPlusPlus\output\playwright\qa-mobile-events-8797.ndjson
D:\mirror++\CodexPlusPlus\output\playwright\qa-mobile-events-8798.ndjson
D:\mirror++\CodexPlusPlus\output\playwright\qa-mobile-events-8799.ndjson
```

Mock Host 只证明 PWA UI 与状态机；真实 Desktop dispatcher 能力由 `mobile_relay_host_live` 验证。

## 6. 本机安装证据

安装文件：

```text
D:\mirrorplus\mirror-x-codex-manager.exe
D:\mirrorplus\mirror-x-codex.exe
```

Manager：

- FileVersion：`1.2.43`
- ProductVersion：`1.2.43`
- SHA256：`C613C1766B5AE992A710EC008F7E4C3DE6F1C211DC916CB011A2467A9B26F39A`
- 验证进程：`11724`
- 实际运行路径：`D:\mirrorplus\mirror-x-codex-manager.exe`

Launcher：

- FileVersion：`1.2.43`
- ProductVersion：`1.2.43`
- SHA256：`B76F2DD9FBEC216F415A29E97F390BC6A25EF7D6E73AD9BE10EE31149443B471`

PID 仅代表本轮核验时的进程。

## 7. 回滚

备份：

```text
D:\mirrorplus\backups\20260814-232937-mobile-logic-v1.2.43\
```

回滚只替换 Manager 与 Launcher EXE，不修改：

- 用户 `.codex`
- 项目文件
- MCP 与 Skills
- Mirror X 模型配置
- API Key
- Codex Desktop 安装

## 8. 最终验收结论

Windows 本机代码、Manager/Host、测试 Relay 和 Desktop Sync 主链路达到“可演示、可继续测试”的标准。它显著降低了初始化假死、误发、重复发送、排队丢失、停止错目标和重连状态丢失的风险。

本轮没有发布公网 Relay，因此公开手机页面是否已获得 v1.2.43 PWA 与 Relay 修复，结论仍为“未发布、不可对外宣称已上线”。

## 9. 尚未解决的结构性风险

### S1：丢失手机的独立撤销

配对凭证会保存在手机浏览器 `localStorage`，以支持刷新、锁屏和浏览器重启后恢复。只要用户 Key 不变，旧手机理论上仍可重新派生或保留同一房间凭证。

当前撤销方式：

1. 在仍持有的手机上点“断开连接”，清除该手机本地凭证；或
2. 更换用户 API Key。

正式商用前建议增加独立于 API Key 的可旋转 pairing secret 与“撤销所有手机”按钮。该改动会改变“只输入 Key”流程，需要单独做兼容设计，不能在本轮暗中改变。

### S2：真机与公网发布门禁

- Android 和 iPhone 各至少一次真实网络验收。
- canary Relay 验证 WSS、Nginx、旧客户端兼容、限流和恢复。
- 连接排空与可回滚切流后才能发布生产。
- macOS 未构建、未测试，不能复用 Windows 结论。

## 10. Windows 正式安装包验收

安装包：

```text
D:\mirror++\CodexPlusPlus\dist\windows\mirror-x-codex-1.2.43-windows-x64-setup.exe
```

- SHA256：`B5B0692454C7EB60452AD7E3DD878E2B9AF08EEFE2A79CA536B4756FCF7A502F`
- NSIS 静默覆盖升级：退出码 `0`
- 注册表 `DisplayVersion`：`1.2.43`
- 桌面快捷方式：存在
- Manager、Launcher、Imagegen：均为 `1.2.43`
- 覆盖升级后 Manager 成功重新启动

ZIP：

```text
D:\mirror++\CodexPlusPlus\dist\windows\mirror-x-codex-1.2.43-windows-x64.zip
```

- SHA256：`0103E898563EE889AB7F9FB68EFE057C6CC202353B0E9F34950E046C75AA4459`
- 仅包含 `mirror-x-codex-manager.exe`、`mirror-x-codex.exe`、`mirror-x-imagegen.exe`

审计时发现旧发布暂存目录会让 ZIP 混入历史 `codex-plus-plus` 文件。已修复
`release-assets.yml` 和 `pr-build.yml`：每次 staging 前先清理 `dist/windows/app`。
NSIS 安装包原本使用显式文件列表，没有受到该残留影响。

本次覆盖升级前备份：

```text
D:\mirrorplus\backups\20260814-remote-full-installer-v1.2.43\
```

## 11. 服务器独立 canary

只读核对确认当前公开域名仍由 Nginx 转发到 `8765`，公开 `/health` 版本为
`1.2.39`。服务器同时存在一个 systemd 管理的 `8766` 实例，但 Nginx 没有使用它。

为避免影响在线连接，本轮新增完全隔离的 canary：

```text
127.0.0.1:8767
systemd unit: mirror-x-relay-canary-v1243
version: 1.2.43
```

Linux x64 binary SHA256：

```text
d1a4b22fb58812f8d45a5a2f7ba4111a7dd2477c76539fba21e336c905a8e453
```

验证结果：

- Linux 服务器原生构建：通过
- `/health` 返回 `1.2.43`
- 经 SSH tunnel 执行完整 Relay E2E：全部通过
- 390×844 手机视口打开服务器 canary PWA：通过
- PWA build：`v20260814.6`
- 自动读取历史并打开最近会话：通过
- canary 未接入 Nginx，公网客户仍使用原 `8765`

PWA 文件已经上传到未激活的 staging 目录：

```text
/var/www/mirror-x-mobile/.stage-v1.2.43/
```

生产切换仍未执行。正式切换前必须：

1. 把 canary 改为持久 systemd unit。
2. 备份 Nginx 与现有 PWA。
3. 原子把新连接从 `8765` 切到 `8767`。
4. 保留 `8765` 作为即时回滚，不立即终止旧连接。
5. 验证公开 `/health`、WSS、PWA build 和真实手机后再排空旧实例。
