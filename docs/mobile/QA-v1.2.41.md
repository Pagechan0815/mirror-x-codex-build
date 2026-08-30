# Mirror X Codex 手机远程 v1.2.41 QA 记录

日期：2026-08-13

## 结论

Windows 本机安装版已更新到 `1.2.41`。本轮修复通过单元测试、真实
`codex app-server` live test、本地 Relay 端到端测试和 Chromium 手机视口测试。

本轮没有发布公网 Relay，也没有 commit 或 push。公网仍需在解决
`8765/8766` 双实例拓扑后，按 canary、连接排空和 Nginx 原子切流流程发布。

## 已修复

- 手机页面刷新、锁屏恢复和浏览器重开保持稳定 `sessionId`。
- Host 与 Relay 断开时不再终止本地 `codex app-server`。
- Relay 整进程重启后，Host 自动重连并恢复原 app-server。
- 最近查看的会话按配对房间保存，刷新后自动回到原会话。
- 运行状态按 `threadId` 隔离，后台会话输出不写入当前会话 DOM。
- `refreshSelectedThread()` 不再用旧历史覆盖正在流式输出的会话。
- `turn/start` 使用稳定消息 ID，并在响应不确定时进入“发送状态确认中”。
- app-server 异常退出后限次自动重建，避免无限重启。
- 第二台手机或第二个标签页接管时，旧页面收到 `CLIENT_REPLACED`，
  停止自动重连、禁用输入，并提供“在此设备重新接管”。
- Relay writer 退出纳入连接生命周期，减少失效连接计数滞留。
- 历史刷新加入 generation，旧请求不能覆盖新请求。
- 长回复不再永久截断，支持展开全部和复制全文。
- Markdown 标题、列表、行内代码、链接、引用、表格、代码块和
  `[!image]` 占位均通过浏览器渲染验证。
- 844×390 手机横屏改为抽屉导航，避免固定侧栏挤压会话。
- 补齐 favicon 与新版 PWA meta，最终浏览器控制台为 0 error、0 warning。

## 自动化测试

通过：

- `cargo test -p codex-plus-core mobile_relay_host::tests --lib`
  - 16/16
- `cargo test -p codex-plus-mobile-relay`
  - 9/9
- `cargo test -p codex-plus-manager --test windows_subsystem`
  - 23/23
- `cargo test -p codex-plus-core --test mobile_relay_host_live -- --ignored`
  - 真实 Codex app-server 初始化、会话读取、同 session 恢复、Relay 整进程重启恢复均通过
- `node scripts/mobile_reconnect_check.mjs`
- `node scripts/mobile_crypto_check.mjs`
- `py -3 scripts/mobile_e2e_check.py`
  - Host 离线、错误 token、端到端加密转发、双标签接管均通过
- Manager `npm run check`
- Manager `npm run vite:build`
- `cargo check -p codex-plus-manager`
- `cargo build -p codex-plus-manager --release`

## 浏览器实测

通过：

- 390×844 竖屏连接、打开抽屉、选择未预先打开的历史会话。
- 手机发送消息并收到流式/格式化回复。
- 刷新页面后自动回到原会话。
- 844×390 横屏使用抽屉导航，会话区保持全宽。
- 双标签页接管后旧标签明确显示“已在另一设备打开”，不发生重连风暴。
- 最终 Chromium 控制台 0 error、0 warning。

## 本机安装证据

- Release：
  `D:\mirror++\CodexPlusPlus\target\release\mirror-x-codex-manager.exe`
- 安装：
  `D:\mirrorplus\mirror-x-codex-manager.exe`
- FileVersion / ProductVersion：`1.2.41`
- SHA256：
  `ECBDC5A73B504B7F39A0960AE36B828B89665911F1CE526592782F48D85A5232`
- 旧版备份：
  `D:\mirrorplus\backups\20260813-184247-mobile-stability-v1.2.41\mirror-x-codex-manager.exe`
- 旧版备份 SHA256：
  `1FE4086029C5BE6DD47419C740D9B6EF7CE6BBDB0BE85F65675D167FDCE51692`
- 安装后进程响应正常，窗口标题为 `mirror x codex`。

## 尚未对外承诺的边界

- 公网 Relay 尚未部署 `1.2.41`，不能宣称所有客户已获得本轮 Relay 协议修复。
- 手机端是 Codex app-server 的独立客户端，不等同于 Codex Desktop UI 像素级双向同步。
- 同一配对房间当前采用单主手机/单活动标签策略，不支持多设备同时写入。
- 手机断网时无法保证立刻显示实时 token；恢复后通过会话历史校准。
- 用户主动“断开连接”会清除手机本地配对和 session；重新使用需再次扫码或输入 Key。
- 公网发布前必须先消除 `8765/8766` 双 Relay 拓扑，不能直接重启当前在线的 `8765`。
