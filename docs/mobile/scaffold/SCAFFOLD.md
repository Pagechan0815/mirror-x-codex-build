# 基础架构搭建指南

> 基于 Architecture.md + ObjectModel.md  
> 目标：可编译、可运行的最小骨架，核心逻辑桩代码齐全

---

## 阶段一：中继服务脚手架（mirror-x-relay）

### 1.1 修改 `apps/codex-plus-mobile-relay/Cargo.toml`

```toml
[package]
name = "mirror-x-relay"
version = "1.0.0"
edition.workspace = true

[dependencies]
anyhow.workspace = true
futures-util = { workspace = true, features = ["sink"] }
serde.workspace = true
serde_json.workspace = true
tokio = { workspace = true, features = ["net", "signal", "sync", "time"] }
tokio-tungstenite = { workspace = true, features = ["rustls-tls-webpki-roots"] }
# 新增依赖
hkdf = "0.12"
sha2.workspace = true
```

### 1.2 新增 `apps/codex-plus-mobile-relay/src/rate_limiter.rs`

```rust
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    buckets: HashMap<IpAddr, TokenBucket>,
    cleanup_last: Instant,
}

struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
    capacity: f64,
    refill_rate: f64,
}

impl RateLimiter {
    pub fn new(capacity: f64, refill_rate: f64) -> Self {
        Self {
            buckets: HashMap::new(),
            cleanup_last: Instant::now(),
        }
    }

    pub fn check_and_consume(&mut self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let bucket = self.buckets.entry(ip).or_insert_with(|| TokenBucket {
            tokens: 10.0,
            last_refill: now,
            capacity: 10.0,
            refill_rate: 10.0 / 60.0,  // 10 次/分钟
        });

        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * bucket.refill_rate).min(bucket.capacity);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    pub fn cleanup_stale(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.cleanup_last) > Duration::from_secs(300) {
            self.buckets.retain(|_, b| now.duration_since(b.last_refill) < Duration::from_secs(600));
            self.cleanup_last = now;
        }
    }
}
```

### 1.3 修改 `apps/codex-plus-mobile-relay/src/main.rs`

**关键变更：**

1. 在 `RelayState` 加入 `rate_limiter: RateLimiter`
2. 在 `register_peer` 开头加：
```rust
// 限速检查
if !state.rate_limiter.check_and_consume(remote_addr.ip()) {
    bail!("rate limited");
}
```
3. 在 `register_peer` 中，`role == Role::Client` 时检查：
```rust
if room.host.is_none() {
    bail!("host not online");
}
```
4. 修复 token 校验（已有，保持）
5. 移除内嵌 HTML 中的 `token=${room}`，改为读 localStorage

### 1.4 Docker 化

创建 `apps/codex-plus-mobile-relay/Dockerfile`：
```dockerfile
FROM rust:1.83 AS builder
WORKDIR /build
COPY Cargo.* ./
COPY crates ./crates
COPY apps/codex-plus-mobile-relay ./apps/codex-plus-mobile-relay
RUN cargo build --release --bin mirror-x-relay

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/mirror-x-relay /usr/local/bin/
ENV CODEX_PLUS_MOBILE_RELAY_BIND=0.0.0.0:8765
EXPOSE 8765
CMD ["mirror-x-relay"]
```

创建 `apps/codex-plus-mobile-relay/docker-compose.yml`：
```yaml
version: '3.8'
services:
  relay:
    build: .
    image: mirror-x-relay:latest
    restart: unless-stopped
    environment:
      - CODEX_PLUS_MOBILE_RELAY_BIND=0.0.0.0:8765
    ports:
      - "127.0.0.1:8765:8765"
    networks:
      - internal

networks:
  internal:
```

---

## 阶段二：桌面 Host 脚手架

### 2.1 新增 `crates/codex-plus-core/src/mobile_relay_host.rs`

```rust
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, oneshot, mpsc};
use tokio_tungstenite::{connect_async, tungstenite::Message};

pub struct MobileRelayHostConfig {
    pub relay_url: String,
    pub room_id: String,
    pub relay_token: String,
    pub enc_key: [u8; 32],
}

impl MobileRelayHostConfig {
    pub fn from_api_key(api_key: &str, relay_url: String) -> Result<Self> {
        use hkdf::Hkdf;
        use sha2::Sha256;

        let ikm = api_key.as_bytes();
        
        // room_id = HKDF(key, "mirror-x-room-v1", 16 bytes)
        let hk_room = Hkdf::<Sha256>::new(Some(b"mirror-x-room-v1"), ikm);
        let mut room_bytes = [0u8; 16];
        hk_room.expand(&[], &mut room_bytes).unwrap();
        let room_id = hex::encode(room_bytes);

        // relay_token = HKDF(key, "mirror-x-relay-tok-v1", 16 bytes)
        let hk_tok = Hkdf::<Sha256>::new(Some(b"mirror-x-relay-tok-v1"), ikm);
        let mut tok_bytes = [0u8; 16];
        hk_tok.expand(&[], &mut tok_bytes).unwrap();
        let relay_token = hex::encode(tok_bytes);

        // enc_key = HKDF(key, "mirror-x-enc-v1", 32 bytes)
        let hk_enc = Hkdf::<Sha256>::new(Some(b"mirror-x-enc-v1"), ikm);
        let mut enc_key = [0u8; 32];
        hk_enc.expand(&[], &mut enc_key).unwrap();

        Ok(Self {
            relay_url,
            room_id,
            relay_token,
            enc_key,
        })
    }

    pub fn host_url(&self) -> String {
        format!(
            "{}?room={}&token={}&role=host",
            self.relay_url.trim_end_matches('/'),
            self.room_id,
            self.relay_token
        )
    }
}

pub struct MobileRelayHostRuntime {
    pub shutdown_tx: oneshot::Sender<()>,
    pub task: tokio::task::JoinHandle<()>,
}

pub async fn spawn_mobile_relay_host(
    config: MobileRelayHostConfig,
) -> Result<MobileRelayHostRuntime> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(run_mobile_relay_host(config, shutdown_rx));
    Ok(MobileRelayHostRuntime { shutdown_tx, task })
}

async fn run_mobile_relay_host(
    config: MobileRelayHostConfig,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let mut backoff = 1u64;
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,
            result = run_mobile_relay_host_once(&config) => {
                if let Err(e) = result {
                    eprintln!("mobile_relay_host error: {e:?}");
                    tokio::time::sleep(tokio::time::Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(30);
                } else {
                    break;  // 正常关闭
                }
            }
        }
    }
}

async fn run_mobile_relay_host_once(config: &MobileRelayHostConfig) -> Result<()> {
    let url = config.host_url();
    let (ws_stream, _) = connect_async(&url).await.context("connect relay failed")?;
    let (mut write, mut read) = ws_stream.split();

    // TODO: 实现消息处理逻辑
    // - 收到 encrypted envelope → decrypt → RelayMessage
    // - appServerConnect → spawn app-server session
    // - appServerMessage → forward to app-server stdin
    // - app-server stdout → forward back

    while let Some(msg) = read.next().await {
        let msg = msg?;
        // 桩代码：解密 + 打印
        if let Message::Text(text) = msg {
            println!("host received: {}", text);
        }
    }

    Ok(())
}

#[derive(Serialize, Deserialize)]
pub struct EncryptedEnvelope {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub nonce: String,
    pub payload: String,
}

pub fn decrypt_envelope(
    enc_key: &[u8; 32],
    envelope: &EncryptedEnvelope,
) -> Result<Vec<u8>> {
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};

    let cipher = Aes256Gcm::new(enc_key.into());
    let nonce_bytes = base64_url_decode(&envelope.nonce)?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = base64_url_decode(&envelope.payload)?;

    cipher.decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| anyhow::anyhow!("decrypt failed"))
}

pub fn encrypt_message(
    enc_key: &[u8; 32],
    plaintext: &[u8],
) -> Result<EncryptedEnvelope> {
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
    use rand::Rng;

    let cipher = Aes256Gcm::new(enc_key.into());
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher.encrypt(nonce, plaintext)
        .map_err(|_| anyhow::anyhow!("encrypt failed"))?;

    Ok(EncryptedEnvelope {
        msg_type: "encrypted".to_string(),
        nonce: base64_url_encode(&nonce_bytes),
        payload: base64_url_encode(&ciphertext),
    })
}

fn base64_url_encode(data: &[u8]) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD.encode(data)
}

fn base64_url_decode(s: &str) -> Result<Vec<u8>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD.decode(s).context("base64 decode failed")
}
```

### 2.2 在 `crates/codex-plus-core/src/lib.rs` 加入

```rust
pub mod mobile_relay_host;
```

### 2.3 在 `crates/codex-plus-core/Cargo.toml` 补充依赖

```toml
hkdf = "0.12"
hex = "0.4"
base64 = "0.22"
rand = "0.8"
```

### 2.4 在 `crates/codex-plus-core/src/launcher.rs` 集成

```rust
use crate::mobile_relay_host::{spawn_mobile_relay_host, MobileRelayHostConfig, MobileRelayHostRuntime};

pub struct LauncherHooks {
    // ... 现有字段
    mobile_relay_host: Mutex<Option<MobileRelayHostRuntime>>,
}

impl LauncherHooks {
    async fn start_mobile_relay_if_enabled(&self, settings: &BackendSettings) -> anyhow::Result<()> {
        if !settings.mobile_control_enabled {
            return Ok(());
        }

        // 从 relay_profiles[0].api_key 或环境变量读 key
        let api_key = settings.relay_profiles.first()
            .map(|p| p.api_key.as_str())
            .ok_or_else(|| anyhow::anyhow!("no api key configured"))?;

        let relay_url = settings.mobile_control_relay_url.clone();
        if relay_url.is_empty() {
            return Ok(());
        }

        let config = MobileRelayHostConfig::from_api_key(api_key, relay_url)?;
        let runtime = spawn_mobile_relay_host(config).await?;
        *self.mobile_relay_host.lock().await = Some(runtime);

        Ok(())
    }

    async fn stop_mobile_relay(&self) {
        if let Some(runtime) = self.mobile_relay_host.lock().await.take() {
            let _ = runtime.shutdown_tx.send(());
            let _ = runtime.task.await;
        }
    }
}
```

在 `run_launch_lifecycle` 中，`launch_codex` 成功后调用：
```rust
hooks.start_mobile_relay_if_enabled(&settings).await?;
```

在 shutdown 时调用：
```rust
hooks.stop_mobile_relay().await;
```

### 2.5 在 `crates/codex-plus-core/src/settings.rs` 加字段

```rust
#[serde(rename = "mobileControlEnabled", default)]
pub mobile_control_enabled: bool,

#[serde(rename = "mobileControlRelayUrl", default = "default_mobile_relay_url")]
pub mobile_control_relay_url: String,

fn default_mobile_relay_url() -> String {
    "wss://relay.jingziai.com/relay".to_string()
}
```

---

## 阶段三：手机 PWA 脚手架

### 3.1 创建 `apps/codex-plus-mobile-relay/pwa/index.html`

```html
<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0, maximum-scale=1.0, user-scalable=no">
  <title>Mirror X Mobile</title>
  <link rel="manifest" href="/manifest.json">
  <style>
    * { margin: 0; padding: 0; box-sizing: border-box; }
    body {
      font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
      font-size: 14px;
      background: #f5f5f5;
      overflow-x: hidden;
    }
    #app { display: flex; flex-direction: column; height: 100vh; }
    .screen { display: none; flex-direction: column; height: 100%; }
    .screen.active { display: flex; }
    
    /* Setup Screen */
    .setup-screen { justify-content: center; align-items: center; padding: 20px; }
    .setup-screen input { width: 100%; max-width: 400px; padding: 12px; font-size: 16px; border: 1px solid #ccc; border-radius: 8px; }
    .setup-screen button { margin-top: 16px; padding: 12px 24px; font-size: 16px; background: #007aff; color: white; border: none; border-radius: 8px; }

    /* Connecting Screen */
    .connecting-screen { justify-content: center; align-items: center; }
    .connecting-screen .spinner { width: 40px; height: 40px; border: 4px solid #eee; border-top-color: #007aff; border-radius: 50%; animation: spin 1s linear infinite; }
    @keyframes spin { to { transform: rotate(360deg); } }

    /* Project List Screen */
    .project-list { flex: 1; overflow-y: auto; padding: 8px; }
    .project-group { margin-bottom: 8px; background: white; border-radius: 8px; overflow: hidden; }
    .project-header { padding: 12px; font-weight: 600; background: #f9f9f9; cursor: pointer; }
    .thread-item { padding: 12px; border-top: 1px solid #eee; cursor: pointer; }
    .thread-item:active { background: #f0f0f0; }

    /* Thread Screen */
    .thread-screen { padding: 8px; }
    .messages { flex: 1; overflow-y: auto; padding: 8px; background: white; border-radius: 8px; margin-bottom: 8px; }
    .message { margin-bottom: 12px; padding: 8px; border-radius: 6px; }
    .message.user { background: #007aff; color: white; text-align: right; }
    .message.agent { background: #f0f0f0; }
    .send-bar { display: flex; gap: 8px; }
    .send-bar textarea { flex: 1; padding: 8px; border: 1px solid #ccc; border-radius: 6px; resize: none; }
    .send-bar button { padding: 8px 16px; background: #007aff; color: white; border: none; border-radius: 6px; }
  </style>
</head>
<body>
  <div id="app">
    <!-- Setup Screen -->
    <div class="screen setup-screen active">
      <h2>Mirror X Mobile</h2>
      <input id="apiKeyInput" type="password" placeholder="输入 API Key (sk-xxx)" />
      <button id="connectBtn">连接</button>
    </div>

    <!-- Connecting Screen -->
    <div class="screen connecting-screen">
      <div class="spinner"></div>
      <p id="connectStatus">连接中...</p>
    </div>

    <!-- Project List Screen -->
    <div class="screen project-list-screen">
      <div class="project-list" id="projectList"></div>
    </div>

    <!-- Thread Screen -->
    <div class="screen thread-screen">
      <button id="backBtn">← 返回</button>
      <div class="messages" id="messages"></div>
      <div class="send-bar">
        <textarea id="messageInput" placeholder="输入消息..." rows="2"></textarea>
        <button id="sendBtn">发送</button>
      </div>
    </div>
  </div>

  <script type="module">
    // 桩代码，完整实现见 scaffold/pwa/app.js
    const app = {
      phase: 'setup',
      apiKey: null,
      ws: null,
      encKey: null,

      async init() {
        const stored = localStorage.getItem('mirror-x-config');
        if (stored) {
          const config = JSON.parse(stored);
          this.apiKey = config.apiKey;
          this.switchPhase('connecting');
          await this.connect();
        }

        document.getElementById('connectBtn').onclick = async () => {
          this.apiKey = document.getElementById('apiKeyInput').value;
          if (!this.apiKey) return alert('请输入 Key');
          localStorage.setItem('mirror-x-config', JSON.stringify({ apiKey: this.apiKey }));
          this.switchPhase('connecting');
          await this.connect();
        };

        document.getElementById('backBtn').onclick = () => {
          this.switchPhase('project-list');
        };
      },

      switchPhase(phase) {
        this.phase = phase;
        document.querySelectorAll('.screen').forEach(s => s.classList.remove('active'));
        document.querySelector(`.${phase}-screen`).classList.add('active');
      },

      async connect() {
        // TODO: 完整实现
        // - deriveKeys(apiKey) → { roomId, relayToken, encKey }
        // - WebSocket 连接
        // - 加解密
        setTimeout(() => this.switchPhase('project-list'), 2000);
      },
    };

    app.init();
  </script>
</body>
</html>
```

### 3.2 创建 `apps/codex-plus-mobile-relay/pwa/manifest.json`

```json
{
  "name": "Mirror X Mobile",
  "short_name": "MirrorX",
  "start_url": "/",
  "display": "standalone",
  "background_color": "#ffffff",
  "theme_color": "#007aff",
  "icons": [
    {
      "src": "/icon-192.png",
      "sizes": "192x192",
      "type": "image/png"
    },
    {
      "src": "/icon-512.png",
      "sizes": "512x512",
      "type": "image/png"
    }
  ]
}
```

### 3.3 中继服务器静态资源服务

在 `apps/codex-plus-mobile-relay/src/main.rs` 的 HTTP 处理中加：
```rust
if path == "/mobile" => {
    return handle_pwa_page(stream).await;
}

async fn handle_pwa_page(stream: &mut TcpStream) -> Result<()> {
    let html = include_str!("../pwa/index.html");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
        html.len(),
        html
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}
```

---

## 阶段四：Manager UI 脚手架

### 4.1 新增 `apps/codex-plus-manager/src/MobileControlPanel.tsx`

```tsx
import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface MobileControlState {
  enabled: boolean;
  relayUrl: string;
  qrCodeDataUrl: string | null;
}

export function MobileControlPanel() {
  const [state, setState] = useState<MobileControlState | null>(null);

  useEffect(() => {
    loadState();
  }, []);

  async function loadState() {
    const data = await invoke<MobileControlState>('get_mobile_control_status');
    setState(data);
  }

  async function toggleEnable() {
    if (state?.enabled) {
      await invoke('disable_mobile_control');
    } else {
      await invoke('enable_mobile_control');
      await invoke('generate_mobile_qr_code');
    }
    await loadState();
  }

  if (!state) return <div>加载中...</div>;

  return (
    <div className="mobile-control-panel">
      <h3>手机控制</h3>
      <label>
        <input type="checkbox" checked={state.enabled} onChange={toggleEnable} />
        启用手机控制
      </label>
      {state.enabled && state.qrCodeDataUrl && (
        <div>
          <img src={state.qrCodeDataUrl} alt="手机扫码" />
          <p>用手机扫码打开控制页面</p>
        </div>
      )}
    </div>
  );
}
```

### 4.2 新增 Tauri Commands

在 `apps/codex-plus-manager/src-tauri/src/commands.rs` 加：
```rust
#[tauri::command]
pub async fn get_mobile_control_status(state: tauri::State<'_, Arc<Mutex<BackendState>>>) -> Result<MobileControlState, String> {
    let backend = state.lock().await;
    let settings = backend.settings();
    Ok(MobileControlState {
        enabled: settings.mobile_control_enabled,
        relay_url: settings.mobile_control_relay_url.clone(),
        qr_code_data_url: None,  // TODO: 生成二维码
    })
}

#[tauri::command]
pub async fn enable_mobile_control(state: tauri::State<'_, Arc<Mutex<BackendState>>>) -> Result<(), String> {
    // TODO: 保存 settings.mobile_control_enabled = true
    Ok(())
}

#[tauri::command]
pub async fn generate_mobile_qr_code(state: tauri::State<'_, Arc<Mutex<BackendState>>>) -> Result<String, String> {
    // TODO: 用 qrcode crate 生成二维码 data URL
    Ok("data:image/png;base64,...".to_string())
}
```

---

## 验证清单

### 中继服务
- [ ] `cargo build --release --bin mirror-x-relay` 编译通过
- [ ] `docker-compose build` 成功
- [ ] 启动后 `/health` 端点返回 200
- [ ] WebSocket 可握手（用 `wscat -c ws://127.0.0.1:8765`）

### 桌面端
- [ ] `cargo test -p codex-plus-core` 通过
- [ ] Manager 启动不报错
- [ ] 手机控制面板可见
- [ ] 开关切换时日志显示 mobile_relay_host 启动/停止

### 手机 PWA
- [ ] 浏览器打开 `http://127.0.0.1:8765/mobile` 可见页面
- [ ] 输入 key 后可切换到"连接中"画面（即使连不上，状态机逻辑正常）

### 集成测试（可选）
- [ ] 桌面 host 启动 → 中继日志显示 host 注册
- [ ] 手机 client 连接 → 中继日志显示 client 注册
- [ ] host 发送测试消息 → client 收到（加密的，暂时打印日志）

---

## 下一步（v1.2.39 功能完整）

1. 完成 `mobile_relay_host.rs` 的消息处理逻辑（decrypt → dispatch → app-server session）
2. 完成 PWA 的 WebCrypto HKDF + AES-GCM 实现
3. 完成 PWA 的 thread/list RPC 调用 + 渲染
4. 服务器部署 + SSL 配置
