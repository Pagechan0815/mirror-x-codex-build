# Mirror X Codex 手机端控制 — Research.md

> 版本 v1.0 · 2026-08-11 · 基于本地代码深读 + 全网调研

---

## 1. Codex App-Server 协议深度解析

### 1.1 传输层

官方源码 `codex-rs/app-server/README.md` 描述：  
**JSON-RPC 2.0 over stdio**，每条消息以换行符分隔，`jsonrpc:"2.0"` 字段在协议层省略（wire 层可省）。  
进程以 `codex app-server` 启动，父进程写 stdin、读 stdout；双向推送依赖父进程持续轮询 stdout。

本机实测结果（codex 0.147.0-alpha.6.6，Windows）：
- `initialize` → 同步返回 `userAgent`、`codexHome`、`platformOs`
- `thread/list` → `{ data: Thread[], nextCursor, backwardsCursor }`
- `thread/turns/list` → `{ data: Turn[] }`
- `thread/resume` → 恢复已有 thread
- `turn/start` → 触发新 turn，流式通过 notification 推送
- `codex app-server daemon` → **Windows 明确不支持**，报错 "only supported on Unix platforms"

### 1.2 Thread 数据结构（实测字段）

```
id, sessionId, cwd, path, preview, status, name,
createdAt, updatedAt, recencyAt,
cliVersion, modelProvider, agentNickname, agentRole,
historyMode, source, threadSource,
canAcceptDirectInput, ephemeral,
forkedFromId, parentThreadId,
gitInfo, extra, section, sectionEnteredAt,
turns[]
```

`cwd` 字段存在且非空，可用于按项目分组。  
`status` 字段用于判断 turn 是否在进行中。

### 1.3 Turn / Item 结构

Turn 包含 `items[]`，item 有 `type`：
- `userMessage` — 用户输入
- `agentMessage` — Codex 文本输出
- `toolCall` / `toolResult` — 工具调用及结果
- `approval` — 待审批请求（sandbox_permissions 问题，见 §1.4）

### 1.4 已知问题：approval 不通过 app-server 暴露

GitHub issue #21982：`sandbox_permissions` 审批当前**不经过 app-server** 推送给客户端，turn 状态挂起直至超时。  
影响评估：纯文字对话和代码生成不受影响；文件写入/命令执行的审批在当前版本需要本机桌面端处理。  
**结论**：手机端 P0 功能中"工具调用审批"需要标注为 Partial，或等待官方修复。

### 1.5 方法覆盖矩阵

| 方法 | 来源 | 实测 | 备注 |
|------|------|------|------|
| initialize | 官方文档 + 实测 | ✅ | 必须第一个发 |
| initialized | 官方文档 | ✅ | notify，无返回 |
| thread/list | 官方文档 + 实测 | ✅ | pageSize 参数有效 |
| thread/turns/list | 官方文档 + 实测 | ✅ | |
| thread/resume | 原型代码 | ✅ | |
| thread/start | 原型代码 | 未实测 | 需 cwd 参数 |
| turn/start | 原型代码 | ✅（间接）| |
| turn/started | 原型代码 | — | notification |
| turn/completed | 原型代码 | — | notification |
| thread/status/changed | 原型代码 | — | notification |

---

## 2. 现有原型代码深度分析

### 2.1 中继服务（`apps/codex-plus-mobile-relay/src/main.rs`，1245 行）

**已实现能力：**
- TCP 监听，自动区分 WebSocket 升级请求和 HTTP 请求
- `Host` / `Client` 双角色，`room` 为隔离单元
- URL 参数注册（`/host?room=X&token=Y`）或首条 JSON 消息注册
- `RelayState`：`HashMap<room, RoomState>`，每个 room 存双向 `mpsc::UnboundedSender`
- 新连接踢出同 room 同 role 的旧连接（`set_sender` 中调用 `previous.send(Close)`）
- 统计指标：`total_connections`、`active_connections`、`forwarded_messages`、`forwarded_bytes`
- `/health` 端点、`/status` 端点（JSON 完整房间状态）
- `/mobile` 手机页面（内嵌 53KB HTML+JS）
- `/` 测试页面

**安全缺陷（已确认）：**
```js
// line 822 — 手机页 client 连接
socket = new WebSocket(`...${scheme}://${location.host}/client?room=${room}&token=${room}`);
```
`token` 直接等于 `room`，任何人知道 room 名就能接入。  
中继服务本身**不做 token 验证**（`register_peer` 中只检查 token 是否与 room 首次注册时一致），但没有随机 token 就形同虚设。

**缺失能力：**
- 无握手后 AES-GCM 校验（加密只在 PWA JS 里，服务端不验证密文合法性）
- 无限速（每 IP 连接频率）
- 无 host 未在线时拒绝 client 的逻辑（client 可以先于 host 连接并等待）
- `codex-plus-mobile-relay` 未加入 workspace Cargo.toml（v1.2.24 被移除）

### 2.2 桌面 Host 进程（`bd8a5ef` commit，已从 launcher 移除）

原型在 `crates/codex-plus-core/src/launcher.rs` 中实现：
- `MobileRelayHostConfig`：从 settings 或环境变量读取 `relay_url`、`room`、`token`、`encryption_key`
- `run_mobile_relay_host`：tokio task，断线自动重连，指数退避
- `run_mobile_relay_host_once`：连接中继 `/host?room=&token=`
- host 收到 `appServerConnect` 消息 → 调用 `ensure_app_server_runtime()` 启动本机 `codex app-server` 子进程
- host 收到 `appServerMessage` → 转发到 app-server stdin
- app-server stdout → 转发回 relay → 客户端

**`ensure_app_server_runtime()` 逻辑：**
1. 检查内存缓存
2. 检查环境变量 `CODEX_PLUS_APP_SERVER_URL` / `CODEX_APP_SERVER_URL`
3. 都没有则 `spawn codex app-server`

**为何在 v1.2.24 被移除：**
- `launcher_no_longer_contains_mobile_control_runtime` 测试断言明确阻止
- 原因推测：早期安全性不够，产品方向调整（改走 Desktop Connector）

### 2.3 Manager UI（`apps/codex-plus-manager/src/`）

基于 Tauri + React，已有：
- 中继 key 配置面板（`relay_profiles`）
- 模型选择器
- 系统托盘集成

无手机控制 UI 入口。

---

## 3. 竞品与参考实现调研

### 3.1 CodexMonitor（Dimillian/CodexMonitor）

Tauri app，用于管理多个本地 Codex agent。  
实现了 app-server protocol 客户端（多 workspace sidebar + 会话列表）。  
关键点：直接 stdio 通信，**没有中继层**，即同机使用。

### 3.2 codex-gateway（agentrq/codex-gateway）

将 app-server JSON-RPC 桥接为 MCP server。  
实现了完整的 MCP ↔ app-server 转换层。  
关键点：同样无中继，纯本机。

### 3.3 codex-acp（agentclientprotocol/codex-acp）

ACP over stdio，将 Codex app-server 包装为 ACP agent。  
支持 WeChat 等第三方接入。  
关键点：需要 ACP 协议中间层，对普通用户门槛高。

### 3.4 StealthRelay（Olib-AI/StealthRelay）

零知识 WebSocket 中继，Rust 实现，Docker 化。  
特点：端到端加密，服务端不存明文，支持 room 机制。  
**与本项目高度相似**，差异是 StealthRelay 不需要 host 先上线，且有 approved pools 机制。  
参考价值：限速实现、TLS 配置、Docker Compose 结构。

### 3.5 vldr/Relay

简单 Rust WebSocket 中继，room 机制，代码极简（可参考作为对比基线）。

### 3.6 opencode E2EE Remote Control（issue #15236）

`opencode` 项目计划的端到端加密远程控制方案，与本项目需求几乎完全一致：  
WebCrypto AES-GCM + blind relay + PWA 客户端。  
**结论：我们的方案与业界方向完全对齐。**

---

## 4. PWA 技术评估

### 4.1 iOS 限制（关键）

| 特性 | iOS Safari | Android Chrome |
|------|-----------|----------------|
| 添加到主屏幕 | ✅（手动，无提示） | ✅（自动提示） |
| WebSocket (ws://) | **❌ HTTPS 页面下不允许** | ✅ |
| WebSocket (wss://) | ✅ | ✅ |
| Web Push 通知 | ✅（iOS 16.4+） | ✅ |
| 后台 Service Worker | 受限 | ✅ |
| localStorage | ✅ | ✅ |
| WebCrypto (AES-GCM) | ✅ | ✅ |

**结论：必须使用 wss（TLS），否则 iOS 无法建立连接。**  
必须配置域名 + SSL 证书，或用自签名 + 用户信任（不现实）。

### 4.2 PWA vs 微信小程序

| 维度 | PWA | 微信小程序 |
|------|-----|-----------|
| 上线周期 | 即时 | 审核 3-7 天 |
| 企业主体 | 不需要 | 需要认证 |
| WebSocket 长连接 | ✅ | 受限（最大 5 个） |
| AES-GCM | ✅（WebCrypto） | 需第三方库 |
| 内容审核风险 | 无 | 有（代码内容） |
| 安装门槛 | 扫码即用 | 搜索/扫码 |

**结论：PWA 是唯一可行选项。**

---

## 5. 加密方案评估

### 5.1 AES-256-GCM（当前原型方案）

- 密钥派生：`SHA-256(api_key)` → 32 字节 AES key
- 每条消息独立随机 12 字节 nonce
- GCM 提供认证（AEAD），篡改可检测
- WebCrypto API 原生支持，零依赖

**弱点**：SHA-256(key) 作为派生函数太直接，如果 key 熵低（如短密码），暴力破解风险。  
`sk-xxx` 格式 key 通常 48+ 字符随机字符，熵足够。

**改进方案**：用 HKDF 替换直接 SHA-256：`HKDF(key, salt="mirror-x-codex-v1", info="mobile-enc")`。  
WebCrypto 支持 HKDF。成本低，安全性提升显著。

### 5.2 密钥生命周期

- key 存 localStorage（PWA 侧），明文但仅在用户自己设备
- 无 key rotation 机制（app-server 层面 key 不过期）
- 建议：Manager 可生成独立的"手机访问 token"，与 API key 分离，可随时撤销

---

## 6. 服务器部署评估（193.112.101.159）

### 6.1 现状

- 已运行 new-api（中转站），Docker Compose 管理
- 端口 80/443 已用于中转站 Web 服务
- 有 root SSH 访问

### 6.2 中继服务部署方案

- 端口建议：`8765`（避免与 new-api 冲突）
- Docker 化：单容器，无状态，内存存储（重启清空所有 room）
- Nginx 反代：`/relay` 路径代理到 `ws://127.0.0.1:8765`，复用 443 端口和已有 SSL 证书
- 或者：单独绑定子域名 `relay.jingziai.com`（推荐，隔离清晰）

### 6.3 现有域名情况

中转站已有域名（从 public-guide/index.html 可推断），但需确认是否有通配符证书或可添加子域名。

---

## 7. Windows 防火墙与出站连接

`codex-plus-launcher` 安装包已有 NSIS/WiX 安装脚本。  
中继 host 进程发起**出站** TCP 连接，Windows 默认允许出站，**不需要额外防火墙规则**。  
若企业环境有出站限制，需提示用户放行 `mirror-x-codex-launcher.exe` 出站规则。

---

## 8. 关键风险汇总

| 风险 | 严重性 | 概率 | 已有缓解 | 待补充 |
|------|--------|------|----------|--------|
| approval 不经 app-server（issue #21982） | 高 | 已确认 | 无 | 标注为已知限制，等官方修复 |
| codex app-server 接口版本漂移 | 中 | 中 | — | 版本检测 + 降级提示 |
| iOS ws:// 不可用 | 高 | 必然 | — | **必须配 wss，本期阻塞项** |
| 中继安全 token=room | 高 | 已确认 | — | 本期必修 |
| 无限速导致中继被扫描滥用 | 中 | 低 | — | 加限速 |
| launcher 测试断言阻止恢复手机 host | 中 | 已确认 | — | 移除/替换断言 |
| app-server 进程在 codex 未安装时找不到 | 高 | 中 | resolve_codex_app_dir 已有 | 加前置检测 + 提示 |
