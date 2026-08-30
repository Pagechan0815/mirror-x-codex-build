# Mirror X Codex 手机端控制 — Architecture.md

> 版本 v1.0 · 2026-08-11  
> 作者：世界级架构师视角  
> 状态：已自我批判并完成第一轮优化（见 §9）

---

## 1. 系统边界与约束

### 1.1 硬约束（不可改变）

| 约束 | 来源 |
|------|------|
| codex app-server 只暴露 stdio JSON-RPC，无网络监听 | 官方设计，Windows 上 daemon 子命令不支持 |
| iOS Safari 不允许 HTTPS 页面发起 ws:// 连接 | 浏览器安全策略 |
| approval 审批目前不经 app-server 推送（issue #21982） | 官方已知 bug |
| 手机不开 VPN，中继必须在国内服务器 | 用户强约束 |
| 电脑必须开机，Codex 必须运行 | 物理约束 |

### 1.2 软约束（可在设计中权衡）

- 用户零基础，方案必须极简（单次扫码，免账号）
- 中继服务宕机不能影响桌面端正常使用
- 回滚安全（桌面端恢复出厂不损坏会话）
- macOS 暂缓

---

## 2. 整体架构

```
┌─────────────────────────────────────────────────────────┐
│  手机端 (iOS/Android)                                   │
│  ┌─────────────────────────────────────────────────┐   │
│  │  Mirror X Mobile PWA                            │   │
│  │  - 扫码/链接打开，可添加到桌面                    │   │
│  │  - WebCrypto AES-256-GCM 加解密                  │   │
│  │  - 渲染：项目列表 / 会话历史 / 流式输出            │   │
│  └─────────────┬───────────────────────────────────┘   │
│                │ wss:// (TLS)                           │
└────────────────┼────────────────────────────────────────┘
                 │
┌────────────────▼────────────────────────────────────────┐
│  中继层 (193.112.101.159)                               │
│  ┌─────────────────────────────────────────────────┐   │
│  │  mirror-x-relay (Rust binary / Docker)          │   │
│  │  - 纯 WebSocket 字节转发，不解密内容              │   │
│  │  - room 隔离，host/client 双角色                  │   │
│  │  - 限速：握手 10次/分钟/IP                        │   │
│  │  - host 未在线时拒绝 client 接入                  │   │
│  └──────────────┬──────────────────────────────────┘   │
│  Nginx 反代     │ /relay → ws://127.0.0.1:8765         │
│  SSL 终止       │ 复用已有 443 端口                     │
└─────────────────┼───────────────────────────────────────┘
                  │ ws:// outbound（桌面主动外连）
┌─────────────────▼───────────────────────────────────────┐
│  桌面端 (用户 Windows PC)                               │
│                                                         │
│  ┌──────────────────┐  ┌──────────────────────────────┐ │
│  │ Mirror X Manager │  │ Mirror X Launcher            │ │
│  │ (Tauri, 托盘)    │  │ (codex-plus-launcher)        │ │
│  │ - 手机控制 UI    │  │ - 驱动 app-server 子进程      │ │
│  │ - 二维码生成     │◄─┤ - 承载 MobileRelayHost task  │ │
│  │ - 连接状态显示   │  │ - 断线重连                    │ │
│  └──────────────────┘  └──────────┬───────────────────┘ │
│                                   │ stdio JSON-RPC       │
│                         ┌─────────▼──────────────────┐  │
│                         │ codex app-server            │  │
│                         │ (官方 CLI 子进程)            │  │
│                         │ 管理所有 thread/turn         │  │
│                         └────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
```

---

## 3. 加密与身份设计

### 3.1 密钥派生（HKDF 方案）

```
输入：api_key（用户的镜子AI中转 key，sk-xxx 格式）

room_id   = HKDF-SHA256(api_key, salt="mirror-x-room-v1",   len=16) → hex string
enc_key   = HKDF-SHA256(api_key, salt="mirror-x-enc-v1",    len=32) → AES-256 key
relay_tok = HKDF-SHA256(api_key, salt="mirror-x-relay-tok-v1", len=16) → hex (中继注册 token)
```

HKDF 在浏览器端通过 WebCrypto 实现，在 Rust 端通过 `hkdf` crate 实现。  
三个值由同一个 key 派生，手机和桌面独立计算，中间不需要传递密钥。

### 3.2 消息加密格式

```json
{
  "type": "encrypted",
  "nonce": "<base64url 12 bytes>",
  "payload": "<base64url AES-256-GCM ciphertext+tag>"
}
```

明文 payload 为 JSON，类型：
- `{ "type": "httpRequest", "id", "method", "path", "body" }` — HTTP 代理请求
- `{ "type": "appServerConnect", "id", "sessionId" }` — 建立 app-server 会话
- `{ "type": "appServerMessage", "sessionId", "message" }` — 转发 JSON-RPC 消息
- `{ "type": "appServerResponse", "sessionId", "message" }` — app-server 返回

### 3.3 鉴权流程

```
桌面 host 连接:
  wss://relay/ws?room=<room_id>&token=<relay_tok>&role=host

手机 client 连接:
  wss://relay/ws?room=<room_id>&token=<relay_tok>&role=client

中继服务校验:
  1. room 首次注册时记录 token
  2. 后续连接 token 必须与记录一致（防止 room 枚举）
  3. host 未在线时，client 连接立即返回错误（不排队等待）
  4. 新 host/client 踢出同 role 旧连接

AES-GCM 握手校验（host 侧）:
  - 收到 client 第一条消息后，尝试 AES-GCM 解密
  - 解密失败 → 发送 Close，不处理后续消息
  - 解密成功 → 正式建立会话
```

---

## 4. 数据流：发送一条消息

```
手机 PWA
  1. user 点击发送
  2. encrypt({ type:"appServerMessage", sessionId, message: JSON.stringify(rpc_payload) })
  3. ws.send(encrypted_envelope)
          │
          ▼
中继服务（blind relay）
  4. 直接转发给 host
          │
          ▼
桌面 MobileRelayHost
  5. 收到 WS 消息
  6. decrypt(envelope) → 得到 appServerMessage
  7. app_server_sessions[sessionId].sender.send(rpc_payload)
          │
          ▼
codex app-server (stdin)
  8. 处理 turn/start，开始执行

返回流：
  app-server stdout → 逐行 notification
  → host 读取 → encrypt → ws.send → 中继 → 手机 PWA
  → decrypt → 解析 notification → 流式渲染
```

---

## 5. 桌面端组件设计

### 5.1 MobileRelayHostRuntime（launcher.rs 恢复）

```rust
struct MobileRelayHostConfig {
    relay_url: String,    // wss://relay.jingziai.com/relay
    room: String,         // hex(HKDF(key, "room"))
    token: String,        // hex(HKDF(key, "relay-tok"))
    enc_key: [u8; 32],    // HKDF(key, "enc")
}

// tokio task，断线自动重连，指数退避 1s→2s→4s→8s→30s 上限
async fn run_mobile_relay_host(config, shutdown_rx) {
    loop {
        match run_mobile_relay_host_once(...).await {
            Ok(_) => break,  // 主动关闭
            Err(e) => { log(e); sleep(backoff).await; }
        }
    }
}
```

### 5.2 AppServerSessionManager

```rust
// host 端维护 app-server 会话池
struct AppServerSessionManager {
    sessions: HashMap<String, AppServerSession>,
    app_server: Arc<AppServerRuntime>,
}

struct AppServerSession {
    session_id: String,
    sender: mpsc::UnboundedSender<String>,  // → app-server stdin
    // 每个会话独立连接 app-server（避免 JSON-RPC id 冲突）
}
```

**设计决策**：每个手机会话独立 spawn 一个 `codex app-server` 进程。  
理由：app-server 当前协议不支持多路复用（一个 initialize 对应一个 client）。

### 5.3 BackendSettings 新增字段

```rust
// settings.rs 新增
#[serde(rename = "mobileControlEnabled", default)]
pub mobile_control_enabled: bool,

#[serde(rename = "mobileControlRelayUrl", default)]
pub mobile_control_relay_url: String,
// 默认值："wss://relay.jingziai.com/relay"
```

---

## 6. 中继服务设计

### 6.1 限速（新增）

```rust
struct RateLimiter {
    buckets: Arc<Mutex<HashMap<IpAddr, TokenBucket>>>,
}

struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
    // 10 次/分钟，burst 3
}
```

### 6.2 host 未在线拒绝策略

```rust
// register_peer 修改
if role == Role::Client {
    if room.host.is_none() {
        bail!("host not online");
        // client 收到 {"type":"error","code":"HOST_OFFLINE","message":"请先打开电脑上的 Mirror X Codex"}
    }
}
```

### 6.3 部署配置（docker-compose）

```yaml
services:
  mirror-x-relay:
    image: mirror-x-relay:latest
    restart: unless-stopped
    environment:
      CODEX_PLUS_MOBILE_RELAY_BIND: "0.0.0.0:8765"
    ports: []          # 不直接暴露，走 Nginx 反代
    networks: [internal]

# Nginx 新增 location
# location /relay {
#   proxy_pass http://127.0.0.1:8765;
#   proxy_http_version 1.1;
#   proxy_set_header Upgrade $http_upgrade;
#   proxy_set_header Connection "upgrade";
# }
```

---

## 7. 手机 PWA 设计

### 7.1 单页应用结构

```
App
├── screens/
│   ├── SetupScreen      # 首次输入 key
│   ├── ConnectingScreen # 连接中 / 电脑离线
│   ├── ProjectListScreen# 项目+会话列表
│   ├── ThreadScreen     # 单个会话完整历史
│   └── SendBarScreen    # 底部发送栏（组件）
├── core/
│   ├── relay.js         # WebSocket 连接管理
│   ├── crypto.js        # HKDF + AES-GCM
│   ├── rpc.js           # JSON-RPC over relay
│   └── storage.js       # localStorage 读写
└── manifest.json        # PWA manifest
```

### 7.2 状态机

```
SETUP → CONNECTING → ONLINE → OFFLINE
                        ↑          │
                        └──reconnect (5s 轮询)

ONLINE 子状态:
  PROJECT_LIST → THREAD_LOADING → THREAD_READY → SENDING
```

### 7.3 关键 UI 规格

- 最小字体 14px（手机可读）
- 项目分组折叠/展开，按 `recencyAt` 排序
- 流式输出：逐字追加，无抖动（`transform: translateZ(0)` 锁定层）
- 工具调用：折叠显示，展开看参数
- 底部固定发送栏，textarea 自适应高度（max 6 行）
- 离线遮罩：半透明覆盖 + "电脑离线" 提示

---

## 8. Manager UI 新增面板

```
手机控制 标签页
┌────────────────────────────────┐
│ ● 已连接 / ○ 等待手机 / ○ 未启用 │
├────────────────────────────────┤
│  [启用手机控制]  (开关)         │
│  中继服务器: wss://...  (可改)  │
├────────────────────────────────┤
│  [二维码区域 / 链接文字]        │
│  扫码在手机上打开               │
├────────────────────────────────┤
│  已连接手机 IP / 断开时间       │
└────────────────────────────────┘
```

二维码编码内容：`https://relay.jingziai.com/relay/mobile?room=<room_id>`  
（**不包含 api key**，手机端 PWA 首次打开时输入 key）

---

## 9. 自我批判与优化记录

### 9.1 批判点一：每个会话独立 app-server 进程 — 资源浪费

**原方案**：每个 mobile session → spawn 独立 `codex app-server`。  
**问题**：app-server 启动时间约 1-2s，内存占用约 50MB，多会话并发时资源消耗高。  
**优化**：单实例 app-server，通过 JSON-RPC id 命名空间复用。  
**可行性**：实测 `thread/list` 等读操作可以在单进程并发；`turn/start` 也可以在同一进程发起不同 thread 的 turn。  
**结论**：改为单 app-server 实例，`appServerSessionId` 仅作为 proxy 层的 id，不对应独立进程。

### 9.2 批判点二：room_id 可能碰撞 — 截断太短

**原方案**：`room_id = HKDF[0:16]`（32 个 hex 字符）。  
**问题**：中转站如果有 10 万用户，碰撞概率约 1/2^64，实际上足够安全。  
**结论**：保持 16 字节，无需改动。

### 9.3 批判点三：PWA 首次需要手动输入 key — 用户体验差

**原方案**：二维码不含 key，手机打开后手动输入。  
**问题**：零基础用户输错 key 概率高，体验差。  
**优化选项 A**：二维码中包含 URL fragment（`#key=xxx`），fragment 不发送到服务器，相对安全。  
**优化选项 B**：二维码中包含一次性 token，桌面端验证后传递 enc_key（需要额外握手轮次）。  
**结论**：采用选项 A，用 URL fragment 传递 key，不经服务器，iOS/Android 均支持。  
格式：`https://relay.jingziai.com/relay/mobile?room=<room_id>#<api_key>`

### 9.4 批判点四：中继服务无 TLS，直接 ws:// — iOS 完全不可用

**原方案**：IP + 端口，ws:// 直连。  
**问题**：iOS Safari 在 HTTPS 页面（PWA）不允许 ws:// 连接，iOS 用户 100% 失败。  
**优化（阻塞项）**：必须配置域名 + Let's Encrypt 证书 + wss://。  
**具体操作**：在 `193.112.101.159` 上为子域名 `relay.jingziai.com` 申请证书，Nginx 配置 SSL 终止。

### 9.5 批判点五：approval 审批功能声称支持但实际无效

**原方案**：PRD P0 功能列"工具调用审批"。  
**问题**：官方 issue #21982 确认 approval 不经 app-server 推送，手机端无法收到审批请求。  
**优化**：MVP 版本降级为"仅展示工具调用结果"，审批标注为"受官方限制，待修复"。  
当官方修复后，host 侧监听 approval 事件并转发即可，手机端 UI 预留审批区域。

### 9.6 批判点六：断线重连期间手机端状态不一致

**问题**：断线重连时，手机端 app-server 会话已关闭，pending RPC 请求丢失。  
**优化**：重连成功后自动重新调用 `initialize` + `thread/resume`，恢复最后一个活跃 thread。  
手机端 RPC 层实现 pending queue，重连后 replay。

### 9.7 批判点七：单点故障 — 中继服务宕机

**问题**：中继宕机则所有用户手机端全部失联。  
**优化（v1.2.41 以后）**：中继服务 Docker 配置 `restart: always` + 健康检查。  
更长远：多节点中继（超出当前范围）。  
**当前可做**：中继宕机时，桌面端 Codex 正常使用不受影响（两者独立进程）。

---

## 10. 组件间接口契约

### 10.1 中继协议（WebSocket 消息格式）

**注册消息**（URL 参数或首条 JSON）：
```
URL: wss://relay/ws?room=<room_id>&token=<relay_tok>&role=host|client
```

**服务端 → client 的错误消息**：
```json
{"type":"error","code":"HOST_OFFLINE","message":"..."}
{"type":"error","code":"TOKEN_MISMATCH","message":"..."}
{"type":"error","code":"RATE_LIMITED","message":"..."}
```

**服务端 → 注册成功**：
```json
{"type":"registered","role":"client","room":"<room_id>"}
```

### 10.2 加密消息格式（host ↔ client 透明字节）

见 §3.2。

### 10.3 host 内部 app-server 消息类型

```
client→host (解密后):
  appServerConnect   { sessionId }
  appServerMessage   { sessionId, message: "<jsonrpc string>" }
  appServerClose     { sessionId }
  httpRequest        { id, method, path, body }

host→client (加密前):
  appServerConnected { sessionId }
  appServerMessage   { sessionId, message: "<jsonrpc notification/response>" }
  appServerClosed    { sessionId, reason }
  httpResponse       { id, status, body }
```

---

## 11. 技术选型汇总

| 组件 | 技术 | 理由 |
|------|------|------|
| 中继服务 | Rust + tokio-tungstenite（复用现有） | 已有完整实现，改造成本低 |
| 桌面 host | Rust（复用 launcher.rs） | 与现有架构一致 |
| 手机 PWA | 原生 HTML+JS（无框架） | 零依赖，内嵌在中继服务器上提供，无需 npm/build |
| 加密 | WebCrypto AES-256-GCM + HKDF | 浏览器原生，零依赖 |
| Rust 加密 | aes-gcm 0.10 + hkdf（新增） | 已有 aes-gcm，加 hkdf crate |
| 部署 | Docker + Nginx 反代 | 与现有中转站部署一致 |
| SSL | Let's Encrypt（Certbot） | 免费，自动续期 |

---

## 12. v1.2.42 当前实现架构勘误

> 本文第 1—11 节保留了 2026-08-11 的设计演进记录。当前主路径已经从“Manager 单独启动 App Server”升级为“Manager 连接 Codex Desktop dispatcher，并复用 Desktop 持有的同一个 App Server writer”。本节是 v1.2.42 的当前实现依据；第 5.2 节和第 9.1 节中的独立/单实例子进程讨论仅作为历史记录。

### 12.1 主路径：同步 Desktop 正在执行的任务

v1.2.42 的首选运行模式为 `desktopSync`：

```text
Phone PWA
  │  encrypted RPC / notification
  ▼
Relay
  │  opaque forwarding only
  ▼
Manager MobileRelayHost
  │  CDP bridge
  ▼
Codex Desktop renderer dispatcher
  │  mcp-request / mcp-notification / mcp-response
  ▼
Desktop-owned Codex App Server
  │
  ├─ .codex 会话与配置
  ├─ 当前项目文件
  ├─ MCP / Skills
  └─ 当前 active turn
```

关键实现：

- `crates/codex-plus-core/src/desktop_sync.rs`：发现 Codex Desktop CDP target，安装 bridge，并桥接 Desktop dispatcher。
- `crates/codex-plus-core/src/mobile_relay_host.rs`：优先启动 `DesktopSyncRuntime`，向手机报告 `mode: "desktopSync"`。
- `apps/codex-plus-mobile-relay/pwa/app.js`：恢复 Desktop active turn、跟踪 `turnId`、排队发送和断线恢复。

手机 RPC 通过 dispatcher 的 `mcp-request` 进入 Desktop 持有的 App Server。Desktop 收到的 `mcp-notification` 和 `mcp-response` 同时转给手机，因此手机看到的是同一 writer 的任务数据流，而不是完成后再从磁盘轮询历史。

### 12.2 降级路径：standalone

当 Codex Desktop 未运行、CDP 不可达或 dispatcher 无法发现时，Manager 可以启动 `standalone` App Server：

```text
Phone → Relay → Manager Host → standalone Codex App Server
```

该路径用于兼容和故障降级，不是 v1.2.42 的首选同步路径。它仍可使用本机 `.codex`、项目文件、MCP、Skills 和配置，但不应被描述为与 Desktop active turn 实时同步。

### 12.3 单 writer 约束

同步同一 Desktop writer 不等于允许两个界面任意并发写入。若 Desktop 当前 turn 仍处于 active：

1. 手机恢复并显示该 active turn 及其 `turnId`。
2. 手机新消息调用 `turn/steer`，并同时提交 `threadId + expectedTurnId + input`。
3. Codex 把输入追加到当前 active turn，继续按新要求执行。
4. 如果任务在提交瞬间已经结束，客户端重新核对 thread 状态后才把消息作为新的 `turn/start` 发送。
5. 如果当前 turn 是 `review` 或手动 `compact`，客户端明确提示暂不可引导，不自动改成排队。
6. 停止请求必须同时携带 `threadId` 和 `turnId`。

该约束用于避免第二个 `turn/start` 与 Desktop 竞争 active writer，同时允许用户在长任务执行过程中纠正方向。`expectedTurnId` 是安全前置条件，防止任务切换竞态导致引导发往错误 turn。

### 12.4 恢复语义

- Relay 暂时不可用：Host 保留 Desktop reader，并使用指数退避重连。
- Relay 先恢复、Host 后恢复：手机保留工作台和当前会话，不退回 Key 输入页。
- `HOST_OFFLINE` / `RATE_LIMITED`：保留当前 DOM 和会话状态，节流提示，不进入重连风暴。
- 手机晚连接：从会话历史恢复 active turn 和 `turnId`。
- 单房间只允许一个活动手机/标签页；新客户端接管时旧客户端收到 `CLIENT_REPLACED`。

### 12.5 明确边界

- 这是 App Server 数据流同步，不是远程桌面或像素级 UI 镜像。
- Desktop 本地界面内部的临时视觉状态不一定属于 App Server 协议，手机只同步协议可观察状态。
- 电脑关机、Manager 停止或 Desktop bridge 不可达时，无法承诺 Desktop active turn 实时同步。
- 当前 Windows 已验证；macOS 仍需独立构建和真机验证。
