# Mirror X Codex 手机端控制 — ObjectModel.md

> 版本 v1.0 · 2026-08-11  
> 基于 Architecture.md 的专项对象建模

---

## 1. 中继层对象模型（Rust）

### 1.1 核心实体

```rust
// ========== RelayState（全局状态，Arc<Mutex>） ==========
pub struct RelayState {
    rooms: HashMap<RoomId, RoomState>,
    rate_limiter: RateLimiter,
    started_at: Instant,
    metrics: Metrics,
}

pub type RoomId = String;  // hex(HKDF(...))

// ========== RoomState（房间隔离单元） ==========
pub struct RoomState {
    room_id: RoomId,
    token: RelayToken,
    host: Option<PeerSender>,
    client: Option<PeerSender>,
    connected_at: Instant,
    last_activity: Instant,
    metrics: RoomMetrics,
}

pub type RelayToken = String;  // hex(HKDF(...))
pub type PeerSender = mpsc::UnboundedSender<Message>;

// ========== Registration（握手阶段的临时对象） ==========
pub struct Registration {
    role: Role,
    room_id: RoomId,
    token: RelayToken,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Host,
    Client,
}

// ========== RegisteredPeer（已验证的对等点） ==========
pub struct RegisteredPeer {
    room_id: RoomId,
    role: Role,
    sender: PeerSender,
    remote_addr: SocketAddr,
    registered_at: Instant,
}

// ========== RateLimiter（限速模块） ==========
pub struct RateLimiter {
    buckets: HashMap<IpAddr, TokenBucket>,
    cleanup_last: Instant,
}

pub struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
    capacity: f64,
    refill_rate: f64,  // tokens per second
}
```

### 1.2 状态机

```
RoomState 状态机：
  Empty → HostOnline → BothOnline → (host or client 断) → HostOnline | ClientWaiting
                                   ↓
                                (双方都断) → cleanup (移除 room)

Peer 状态机：
  Connecting → (WebSocket 握手) → WaitingRegister → (收到注册消息) → Registered
  Registered → (转发消息) → Disconnected (主动/被踢/异常)
```

### 1.3 关键方法签名

```rust
impl RelayState {
    pub fn new() -> Self;
    pub fn get_or_create_room(&mut self, room_id: RoomId, token: RelayToken) -> &mut RoomState;
    pub fn remove_room_if_empty(&mut self, room_id: &RoomId);
    pub fn cleanup_stale_rooms(&mut self, timeout: Duration);
}

impl RoomState {
    pub fn register_peer(&mut self, role: Role, sender: PeerSender) -> Result<(), String>;
    pub fn unregister_peer(&mut self, role: Role);
    pub fn forward_message(&mut self, from: Role, message: Message) -> Result<(), String>;
    pub fn is_empty(&self) -> bool;
}

impl RateLimiter {
    pub fn check_and_consume(&mut self, ip: IpAddr) -> bool;
    pub fn cleanup_stale(&mut self);
}
```

---

## 2. 桌面端对象模型（Rust）

### 2.1 核心实体

```rust
// ========== MobileRelayHostRuntime（launcher.rs） ==========
pub struct MobileRelayHostRuntime {
    shutdown_tx: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

// ========== MobileRelayHostConfig（配置对象） ==========
pub struct MobileRelayHostConfig {
    relay_url: String,       // wss://relay.jingziai.com/relay
    room_id: String,         // hex(HKDF(key, "room"))
    relay_token: String,     // hex(HKDF(key, "relay-tok"))
    enc_key: [u8; 32],       // HKDF(key, "enc")
}

impl MobileRelayHostConfig {
    pub fn from_api_key(api_key: &str, relay_url: String) -> Self;
    pub fn from_settings_and_env(settings: &BackendSettings) -> Option<Self>;
}

// ========== MobileRelayHost（主控 task） ==========
pub struct MobileRelayHost {
    config: MobileRelayHostConfig,
    ws_stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    app_server: Arc<AppServerRuntime>,
    sessions: HashMap<SessionId, AppServerProxy>,
}

pub type SessionId = String;

// ========== AppServerRuntime（app-server 子进程管理） ==========
pub struct AppServerRuntime {
    port: Option<u16>,      // None 表示 stdio 模式（单实例）
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl AppServerRuntime {
    pub async fn ensure() -> Result<Arc<Self>>;
    pub async fn send_rpc(&mut self, payload: &str) -> Result<()>;
    pub async fn read_line(&mut self) -> Result<String>;
}

// ========== AppServerProxy（会话代理） ==========
pub struct AppServerProxy {
    session_id: SessionId,
    pending_rpc: HashMap<RpcId, oneshot::Sender<Value>>,
    next_rpc_id: u64,
}

pub type RpcId = u64;

impl AppServerProxy {
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value>;
    pub async fn notify(&mut self, method: &str, params: Value) -> Result<()>;
    pub fn handle_response(&mut self, response: Value);
}

// ========== EncryptedEnvelope（加密消息信封） ==========
#[derive(Serialize, Deserialize)]
pub struct EncryptedEnvelope {
    #[serde(rename = "type")]
    msg_type: String,  // "encrypted"
    nonce: String,     // base64url
    payload: String,   // base64url(ciphertext+tag)
}

// ========== RelayMessage（解密后的明文消息） ==========
#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RelayMessage {
    #[serde(rename = "appServerConnect")]
    AppServerConnect { id: String, #[serde(rename = "sessionId")] session_id: SessionId },
    
    #[serde(rename = "appServerMessage")]
    AppServerMessage { #[serde(rename = "sessionId")] session_id: SessionId, message: String },
    
    #[serde(rename = "appServerClose")]
    AppServerClose { #[serde(rename = "sessionId")] session_id: SessionId },
    
    #[serde(rename = "httpRequest")]
    HttpRequest { id: String, method: String, path: String, body: Option<String> },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RelayResponse {
    #[serde(rename = "appServerConnected")]
    AppServerConnected { #[serde(rename = "sessionId")] session_id: SessionId },
    
    #[serde(rename = "appServerMessage")]
    AppServerMessage { #[serde(rename = "sessionId")] session_id: SessionId, message: String },
    
    #[serde(rename = "appServerClosed")]
    AppServerClosed { #[serde(rename = "sessionId")] session_id: SessionId, reason: String },
    
    #[serde(rename = "httpResponse")]
    HttpResponse { id: String, status: u16, body: String },
    
    #[serde(rename = "error")]
    Error { code: String, message: String },
}
```

### 2.2 状态机

```
MobileRelayHost 状态机：
  Disconnected → Connecting → (WebSocket 握手) → Registered → Active
                    ↓                                          ↓
                   Failed → (backoff) → Connecting       Disconnected (主动关闭)
                    ↑                                          ↓
                    └──────────────── (网络错误) ───────────────┘

AppServerProxy 状态机：
  Created → (收到 appServerConnect) → Initializing → (initialize 返回) → Ready
  Ready → (turn/start) → TurnActive → (turn/completed) → Ready
                           │
                           └─ (turn/steer + expectedTurnId) → TurnActive
  任何状态 → (appServerClose) → Closed
```

### 2.3 关键方法签名

```rust
impl MobileRelayHost {
    pub async fn run(config: MobileRelayHostConfig, mut shutdown_rx: oneshot::Receiver<()>);
    async fn connect(&self) -> Result<WebSocketStream<...>>;
    async fn handle_message(&mut self, envelope: EncryptedEnvelope) -> Result<()>;
    async fn decrypt(&self, envelope: &EncryptedEnvelope) -> Result<RelayMessage>;
    async fn encrypt(&self, response: &RelayResponse) -> Result<EncryptedEnvelope>;
}

impl AppServerProxy {
    pub async fn initialize(&mut self) -> Result<()>;
    pub async fn thread_list(&mut self, page_size: u32) -> Result<Vec<Thread>>;
    pub async fn thread_resume(&mut self, thread_id: &str) -> Result<()>;
    pub async fn turn_start(&mut self, thread_id: &str, input: Vec<InputItem>) -> Result<String>;
}
```

---

## 3. 手机端对象模型（TypeScript/JavaScript）

### 3.1 核心实体

```typescript
// ========== RelayConnection（WebSocket 连接管理） ==========
class RelayConnection {
    private ws: WebSocket | null;
    private readonly relayUrl: string;
    private readonly roomId: string;
    private readonly relayToken: string;
    private readonly encKey: CryptoKey;
    
    private reconnectTimer: number | null;
    private reconnectBackoff: number;
    
    private messageQueue: EncryptedEnvelope[];
    private listeners: Map<string, (msg: RelayResponse) => void>;
    
    constructor(relayUrl: string, roomId: string, relayToken: string, encKey: CryptoKey);
    
    async connect(): Promise<void>;
    disconnect(): void;
    
    async send(message: RelayMessage): Promise<void>;
    on(type: string, handler: (msg: RelayResponse) => void): void;
    
    private async encrypt(message: RelayMessage): Promise<EncryptedEnvelope>;
    private async decrypt(envelope: EncryptedEnvelope): Promise<RelayResponse>;
    private handleWsMessage(event: MessageEvent): void;
    private handleWsError(event: Event): void;
    private handleWsClose(event: CloseEvent): void;
    private scheduleReconnect(): void;
}

// ========== AppServerRpc（JSON-RPC 客户端） ==========
class AppServerRpc {
    private readonly conn: RelayConnection;
    private sessionId: string | null;
    private nextRpcId: number;
    private pendingCalls: Map<number, { resolve: Function, reject: Function, timeout: number }>;
    
    constructor(conn: RelayConnection);
    
    async connect(): Promise<void>;  // 发送 appServerConnect
    async call(method: string, params: any): Promise<any>;
    notify(method: string, params: any): void;
    
    private handleNotification(notification: any): void;
}

// ========== Thread（会话对象） ==========
interface Thread {
    id: string;
    sessionId: string;
    cwd: string;
    path: string;
    preview: string;
    status: string;
    name: string | null;
    createdAt: string;
    updatedAt: string;
    recencyAt: string;
    turns: Turn[];
    // ... 其余字段
}

// ========== Turn（回合对象） ==========
interface Turn {
    id: string;
    items: Item[];
}

// ========== Item（消息条目） ==========
type Item = UserMessage | AgentMessage | ToolCall | ToolResult | Approval;

interface UserMessage {
    type: "userMessage";
    id: string;
    content: TextContent[];
}

interface AgentMessage {
    type: "agentMessage";
    id: string;
    text: string;
    phase: string | null;
}

interface ToolCall {
    type: "toolCall";
    id: string;
    name: string;
    arguments: any;
}

interface ToolResult {
    type: "toolResult";
    id: string;
    toolCallId: string;
    result: any;
}

// ========== AppState（全局状态机） ==========
enum AppPhase {
    SETUP = "setup",
    CONNECTING = "connecting",
    ONLINE = "online",
    OFFLINE = "offline",
}

interface AppState {
    phase: AppPhase;
    apiKey: string | null;
    projects: ProjectGroup[];
    selectedThreadId: string | null;
    currentThread: Thread | null;
    sendingMessage: boolean;
    errorMessage: string | null;
}

interface ProjectGroup {
    cwd: string;
    threads: Thread[];
    expanded: boolean;
}

// ========== CryptoHelper（加密辅助） ==========
class CryptoHelper {
    static async deriveKeys(apiKey: string): Promise<{
        roomId: string,
        relayToken: string,
        encKey: CryptoKey,
    }>;
    
    static async hkdf(ikm: Uint8Array, salt: string, info: string, length: number): Promise<Uint8Array>;
    static async aesGcmEncrypt(key: CryptoKey, plaintext: Uint8Array): Promise<{ nonce: Uint8Array, ciphertext: Uint8Array }>;
    static async aesGcmDecrypt(key: CryptoKey, nonce: Uint8Array, ciphertext: Uint8Array): Promise<Uint8Array>;
}
```

### 3.2 状态机

```
AppState 状态机：
  SETUP → (输入 key) → CONNECTING → (连接成功) → ONLINE
                          ↓                        ↓
                        OFFLINE ← (断线) ──────────┘
                          ↓
                        (重连成功) → ONLINE
```

### 3.3 数据流

```
用户操作: 点击发送
  ↓
UI 层: handleSendClick()
  ↓
AppState: dispatch({ type: "SEND_MESSAGE", text })
  ↓
AppServerRpc: call("turn/start", { threadId, input })
AppServerRpc: call("turn/steer", { threadId, expectedTurnId, input })
  ↓
RelayConnection: send({ type: "appServerMessage", message: JSON.stringify(rpc) })
  ↓
encrypt() → ws.send()
  ↓
中继服务 → 桌面 host → app-server
  ↓
返回流: app-server stdout → host → 中继 → ws.onmessage
  ↓
decrypt() → RelayResponse
  ↓
AppServerRpc: handleNotification({ method: "turn/started", ... })
  ↓
AppState: dispatch({ type: "TURN_STARTED", ... })
  ↓
UI 重渲染: 显示流式输出
```

---

## 4. Manager UI 对象模型（Tauri + React）

### 4.1 核心实体

```typescript
// ========== MobileControlState（手机控制面板状态） ==========
interface MobileControlState {
    enabled: boolean;
    relayUrl: string;
    connectedClientIp: string | null;
    connectedAt: Date | null;
    lastActivity: Date | null;
    qrCodeDataUrl: string | null;
}

// ========== Tauri Commands（Rust → TypeScript） ==========
@tauri::command
async fn get_mobile_control_status() -> Result<MobileControlState>;

@tauri::command
async fn enable_mobile_control() -> Result<()>;

@tauri::command
async fn disable_mobile_control() -> Result<()>;

@tauri::command
async fn generate_mobile_qr_code() -> Result<String>;  // 返回 data URL
```

---

## 5. 跨层消息格式规范

### 5.1 中继层消息（WebSocket wire format）

```
注册请求（client → 中继）:
  URL: wss://relay/ws?room=<room_id>&token=<relay_tok>&role=client

中继 → client 注册成功:
  {"type":"registered","role":"client","room":"<room_id>"}

中继 → client 错误:
  {"type":"error","code":"HOST_OFFLINE","message":"请先打开电脑"}
```

### 5.2 加密消息（host ↔ client）

```json
// EncryptedEnvelope
{
  "type": "encrypted",
  "nonce": "<12 bytes base64url>",
  "payload": "<ciphertext+tag base64url>"
}

// 明文 payload（RelayMessage）
{
  "type": "appServerMessage",
  "sessionId": "sess-abc123",
  "message": "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"turn/start\",\"params\":{...}}"
}

// 明文 payload（RelayResponse）
{
  "type": "appServerMessage",
  "sessionId": "sess-abc123",
  "message": "{\"jsonrpc\":\"2.0\",\"method\":\"turn/started\",\"params\":{...}}"
}
```

### 5.3 JSON-RPC 消息（手机 ↔ app-server 逻辑层）

```json
// 请求
{
  "jsonrpc": "2.0",
  "id": 5,
  "method": "thread/list",
  "params": { "pageSize": 20 }
}

// 响应
{
  "jsonrpc": "2.0",
  "id": 5,
  "result": {
    "data": [...],
    "nextCursor": "..."
  }
}

// 通知（notification，无 id）
{
  "jsonrpc": "2.0",
  "method": "turn/started",
  "params": { "threadId": "...", "turnId": "..." }
}
```

---

## 6. 数据库与持久化

### 6.1 中继服务（无持久化）

- `RelayState` 纯内存，重启清空所有 room
- metrics 可选输出到 stdout（JSON Lines），外部采集

### 6.2 桌面端（无新增持久化）

- 配置存入 `BackendSettings`（Tauri state file）
- app-server 会话状态由官方 Codex 管理（`~/.codex/state_5.sqlite`）
- host 无需自行持久化会话

### 6.3 手机 PWA（localStorage）

```typescript
interface StoredConfig {
    apiKey: string;
    lastConnectedAt: string;
    seenThreadIds: string[];  // 已读标记（可选）
}

localStorage.setItem("mirror-x-mobile-config", JSON.stringify(config));
```

---

## 7. 错误处理对象

### 7.1 中继层错误码

```rust
pub enum RelayErrorCode {
    HostOffline,
    TokenMismatch,
    RateLimited,
    InvalidMessage,
    DecryptFailed,
}

impl RelayErrorCode {
    pub fn to_json(&self) -> Value {
        json!({
            "type": "error",
            "code": self.as_str(),
            "message": self.message()
        })
    }
}
```

### 7.2 桌面 host 错误

```rust
#[derive(Debug, thiserror::Error)]
pub enum MobileRelayError {
    #[error("WebSocket connection failed: {0}")]
    ConnectionFailed(String),
    
    #[error("Decryption failed: {0}")]
    DecryptionFailed(String),
    
    #[error("App-server not available")]
    AppServerUnavailable,
    
    #[error("Session not found: {0}")]
    SessionNotFound(String),
}
```

### 7.3 手机端错误

```typescript
enum ErrorCode {
    NETWORK_ERROR = "NETWORK_ERROR",
    HOST_OFFLINE = "HOST_OFFLINE",
    AUTH_FAILED = "AUTH_FAILED",
    RPC_TIMEOUT = "RPC_TIMEOUT",
    DECRYPT_FAILED = "DECRYPT_FAILED",
}

class MobileError extends Error {
    constructor(public code: ErrorCode, message: string) {
        super(message);
    }
}
```

---

## 8. 性能优化对象

### 8.1 批量消息（可选，v1.2.41）

```rust
// 合并多条 notification 为一批
pub struct BatchedNotifications {
    session_id: SessionId,
    notifications: Vec<Value>,
    buffered_at: Instant,
}

// 200ms 内的 notification 打包发送，减少 WebSocket 帧数量
```

### 8.2 压缩（可选，v1.2.42）

```
启用 WebSocket permessage-deflate 扩展
适用场景：thread/turns/list 返回大量历史消息时
```

---

## 9. 测试辅助对象

### 9.1 Mock 对象

```rust
// 测试用 fake relay
pub struct MockRelayServer {
    addr: SocketAddr,
    received_messages: Arc<Mutex<Vec<EncryptedEnvelope>>>,
}

impl MockRelayServer {
    pub async fn start() -> Self;
    pub fn get_messages(&self) -> Vec<EncryptedEnvelope>;
    pub async fn send_to_client(&self, envelope: EncryptedEnvelope);
}
```

### 9.2 手机端 Mock

```typescript
class MockAppServerRpc extends AppServerRpc {
    private mockThreads: Thread[];
    
    async call(method: string, params: any): Promise<any> {
        if (method === "thread/list") {
            return { data: this.mockThreads, nextCursor: null };
        }
        // ...
    }
}
```

---

## 总结

对象模型三层完整独立：
1. **中继层** — 无状态转发，只关心 room/role/token，不解析业务消息
2. **桌面 host** — app-server 子进程管理 + 加解密代理，维护会话映射
3. **手机 PWA** — UI 状态机 + RPC 客户端 + 加密层，localStorage 轻量持久化

所有跨层通信均以 JSON 序列化，AES-GCM 加密后透明传输。  
对象生命周期清晰：room 在双方断线后清理，session 在 appServerClose 后清理，手机端状态在重连后重建。
