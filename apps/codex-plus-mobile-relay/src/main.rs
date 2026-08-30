use std::collections::HashMap;
use std::collections::HashSet;
use std::env;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::str;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::Message;

mod protocol;
mod rate_limiter;

use protocol::RelayErrorCode;
use rate_limiter::RateLimiter;

const DERIVED_CREDENTIAL_HEX_LEN: usize = 32;
const MAX_RELAY_MESSAGE_BYTES: u64 = 2 * 1024 * 1024;

/// Registration failure carrying the code we owe the peer before disconnecting.
#[derive(Debug)]
struct RegistrationRejected(RelayErrorCode);

impl std::fmt::Display for RegistrationRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.0.as_str(), self.0.message())
    }
}

impl std::error::Error for RegistrationRejected {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Host,
    Client,
}

impl Role {
    fn from_str(value: &str) -> Option<Self> {
        match value {
            "host" => Some(Self::Host),
            "client" => Some(Self::Client),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Client => "client",
        }
    }
}

#[derive(Debug, Clone)]
struct Registration {
    role: Role,
    room: String,
    token: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct RegisterMessage {
    #[serde(rename = "type")]
    message_type: String,
    role: String,
    room: String,
    token: String,
}

#[derive(Default)]
struct RelayState {
    rooms: HashMap<String, RoomState>,
    started_at: Option<Instant>,
    total_connections: u64,
    active_connections: u64,
    forwarded_messages: u64,
    forwarded_bytes: u64,
    rate_limiter: RateLimiter,
    rejected_connections: u64,
}

struct RoomState {
    token: String,
    host: Option<mpsc::UnboundedSender<Message>>,
    client: Option<mpsc::UnboundedSender<Message>>,
    connected_at: Instant,
    forwarded_messages: u64,
    forwarded_bytes: u64,
}

impl RoomState {
    fn new(token: String) -> Self {
        Self {
            token,
            host: None,
            client: None,
            connected_at: Instant::now(),
            forwarded_messages: 0,
            forwarded_bytes: 0,
        }
    }

    fn sender_for(&self, role: Role) -> Option<mpsc::UnboundedSender<Message>> {
        match role {
            Role::Host => self.host.clone(),
            Role::Client => self.client.clone(),
        }
    }

    fn set_sender(
        &mut self,
        role: Role,
        sender: mpsc::UnboundedSender<Message>,
    ) -> Option<mpsc::UnboundedSender<Message>> {
        let slot = match role {
            Role::Host => &mut self.host,
            Role::Client => &mut self.client,
        };
        slot.replace(sender)
    }

    fn clear_sender(&mut self, role: Role) {
        match role {
            Role::Host => self.host = None,
            Role::Client => self.client = None,
        }
    }

    fn is_empty(&self) -> bool {
        self.host.is_none() && self.client.is_none()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RelayStatus {
    status: &'static str,
    service: &'static str,
    version: &'static str,
    uptime_seconds: u64,
    rooms: usize,
    active_connections: u64,
    total_connections: u64,
    forwarded_messages: u64,
    forwarded_bytes: u64,
    room_details: Vec<RoomStatus>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RoomStatus {
    room: String,
    host_online: bool,
    client_online: bool,
    connections: u8,
    age_seconds: u64,
    forwarded_messages: u64,
    forwarded_bytes: u64,
}

#[derive(Clone)]
struct RegisteredPeer {
    room: String,
    role: Role,
    sender: mpsc::UnboundedSender<Message>,
}

#[derive(Debug, Clone, Default)]
struct TrustedProxies {
    addresses: HashSet<IpAddr>,
}

impl TrustedProxies {
    fn from_env() -> anyhow::Result<Self> {
        let raw = env::var("CODEX_PLUS_MOBILE_RELAY_TRUSTED_PROXIES").unwrap_or_default();
        Self::parse(&raw)
    }

    fn parse(raw: &str) -> anyhow::Result<Self> {
        let mut addresses = HashSet::new();
        for value in raw
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let address = value
                .parse::<IpAddr>()
                .with_context(|| format!("invalid trusted proxy IP: {value}"))?;
            addresses.insert(address);
        }
        Ok(Self { addresses })
    }

    fn resolve_client_ip(
        &self,
        peer_ip: IpAddr,
        headers: &tokio_tungstenite::tungstenite::http::HeaderMap,
    ) -> IpAddr {
        if !self.addresses.contains(&peer_ip) {
            return peer_ip;
        }

        header_ip(headers, "x-real-ip")
            .or_else(|| {
                headers
                    .get("x-forwarded-for")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.split(',').next())
                    .and_then(parse_forwarded_ip)
            })
            .unwrap_or(peer_ip)
    }
}

fn header_ip(
    headers: &tokio_tungstenite::tungstenite::http::HeaderMap,
    name: &'static str,
) -> Option<IpAddr> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_forwarded_ip)
}

fn parse_forwarded_ip(value: &str) -> Option<IpAddr> {
    value.trim().parse::<IpAddr>().ok()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bind = env::var("CODEX_PLUS_MOBILE_RELAY_BIND")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "0.0.0.0:57323".to_string());
    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("failed to bind mobile relay server on {bind}"))?;
    let local_addr = listener.local_addr()?;
    println!("Mirror X Codex mobile relay listening on ws://{local_addr}");
    println!(
        "Clients must send first message: {{\"type\":\"register\",\"role\":\"host|client\",\"room\":\"...\",\"token\":\"...\"}}"
    );
    let trusted_proxies = Arc::new(TrustedProxies::from_env()?);
    if !trusted_proxies.addresses.is_empty() {
        println!(
            "Trusting forwarded client IP headers from {} configured proxy address(es)",
            trusted_proxies.addresses.len()
        );
    }

    let state = Arc::new(Mutex::new(RelayState {
        started_at: Some(Instant::now()),
        ..RelayState::default()
    }));
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, addr) = accepted?;
                let state = Arc::clone(&state);
                let trusted_proxies = Arc::clone(&trusted_proxies);
                tokio::spawn(async move {
                    if let Err(error) =
                        handle_tcp_connection(stream, addr, state, trusted_proxies).await
                    {
                        eprintln!("relay connection {addr} closed: {error:#}");
                    }
                });
            }
            signal = tokio::signal::ctrl_c() => {
                signal.context("failed to wait for Ctrl+C")?;
                break;
            }
        }
    }
    Ok(())
}

async fn handle_tcp_connection(
    stream: TcpStream,
    addr: SocketAddr,
    state: Arc<Mutex<RelayState>>,
    trusted_proxies: Arc<TrustedProxies>,
) -> anyhow::Result<()> {
    if !looks_like_websocket(&stream).await? {
        return handle_http_connection(stream, state).await;
    }
    handle_websocket_connection(stream, addr, state, trusted_proxies).await
}

async fn handle_websocket_connection(
    stream: TcpStream,
    addr: SocketAddr,
    state: Arc<Mutex<RelayState>>,
    trusted_proxies: Arc<TrustedProxies>,
) -> anyhow::Result<()> {
    let url_registration = Arc::new(StdMutex::new(None::<Registration>));
    let callback_registration = Arc::clone(&url_registration);
    let resolved_client_ip = Arc::new(StdMutex::new(addr.ip()));
    let callback_client_ip = Arc::clone(&resolved_client_ip);
    let websocket = accept_hdr_async(
        stream,
        move |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
            if let Some(registration) =
                registration_from_uri(request.uri().path(), request.uri().query())
            {
                if let Ok(mut slot) = callback_registration.lock() {
                    *slot = Some(registration);
                }
            }
            if let Ok(mut slot) = callback_client_ip.lock() {
                *slot = trusted_proxies.resolve_client_ip(addr.ip(), request.headers());
            }
            Ok(response)
        },
    )
    .await
    .context("failed to accept websocket")?;
    let (mut outgoing, mut incoming) = websocket.split();

    let registration = match url_registration.lock().ok().and_then(|slot| slot.clone()) {
        Some(registration) => registration,
        None => {
            let first = match tokio::time::timeout(Duration::from_secs(10), incoming.next()).await {
                Ok(Some(Ok(message))) => message,
                other => {
                    let _ = outgoing
                        .send(Message::Text(
                            RelayErrorCode::InvalidRegistration.to_json().into(),
                        ))
                        .await;
                    let _ = outgoing.send(Message::Close(None)).await;
                    match other {
                        Ok(Some(Err(error))) => {
                            return Err(error).context("failed to read registration");
                        }
                        Ok(None) => bail!("connection closed before registration"),
                        Err(_) => bail!("registration timed out"),
                        Ok(Some(Ok(_))) => unreachable!("matched above"),
                    }
                }
            };
            match parse_registration(first) {
                Ok(registration) => registration,
                Err(error) => {
                    let _ = outgoing
                        .send(Message::Text(
                            RelayErrorCode::InvalidRegistration.to_json().into(),
                        ))
                        .await;
                    let _ = outgoing.send(Message::Close(None)).await;
                    return Err(error);
                }
            }
        }
    };

    let client_ip = resolved_client_ip
        .lock()
        .map(|value| *value)
        .unwrap_or_else(|_| addr.ip());
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let peer = match register_peer(&state, registration, tx.clone(), client_ip).await {
        Ok(peer) => peer,
        Err(error) => {
            // Tell the peer why before closing so the PWA can show a precise
            // message instead of a generic disconnect.
            if let Some(rejected) = error.downcast_ref::<RegistrationRejected>() {
                let _ = outgoing
                    .send(Message::Text(rejected.0.to_json().into()))
                    .await;
            }
            let _ = outgoing.send(Message::Close(None)).await;
            return Err(error);
        }
    };
    let mut writer = tokio::spawn(async move {
        let mut keepalive = tokio::time::interval(Duration::from_secs(25));
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let message = tokio::select! {
                message = rx.recv() => {
                    let Some(message) = message else { break };
                    message
                }
                _ = keepalive.tick() => Message::Ping(Vec::new().into()),
            };
            let closes_connection = message.is_close();
            if outgoing.send(message).await.is_err() || closes_connection {
                break;
            }
        }
    });

    println!(
        "relay registered {} room={} addr={}",
        peer.role.as_str(),
        peer.room,
        addr
    );

    let read_result = loop {
        tokio::select! {
            writer_result = &mut writer => {
                break writer_result
                    .context("relay writer task failed")
                    .map(|_| ());
            }
            incoming_message = incoming.next() => match incoming_message {
                Some(Ok(message)) => {
                    if message.is_close() {
                        break Ok(());
                    }
                    if matches!(message, Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) {
                        continue;
                    }
                    if message_len(&message) > MAX_RELAY_MESSAGE_BYTES {
                        break Err(anyhow::anyhow!("relay message exceeds 2 MiB limit"));
                    }
                    forward_message(&state, &peer, message).await;
                }
                Some(Err(error)) => break Err(error).context("failed to read websocket message"),
                None => break Ok(()),
            }
        }
    };

    unregister_peer(&state, &peer).await;
    writer.abort();
    println!(
        "relay disconnected {} room={} addr={}",
        peer.role.as_str(),
        peer.room,
        addr
    );
    read_result
}

async fn looks_like_websocket(stream: &TcpStream) -> anyhow::Result<bool> {
    let mut buffer = [0_u8; 2048];
    let read = stream.peek(&mut buffer).await?;
    let head = String::from_utf8_lossy(&buffer[..read]).to_ascii_lowercase();
    Ok(head.contains("\r\nupgrade: websocket") || head.contains("\r\nsec-websocket-key:"))
}

async fn handle_http_connection(
    mut stream: TcpStream,
    state: Arc<Mutex<RelayState>>,
) -> anyhow::Result<()> {
    let mut buffer = vec![0_u8; 8192];
    let read = stream.read(&mut buffer).await?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let request_line = request.lines().next().unwrap_or_default();
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/");
    let (status, content_type, body) = match path {
        "/" | "/index.html" => (
            "200 OK",
            "text/html; charset=utf-8",
            relay_test_page().into_bytes(),
        ),
        "/mobile" | "/relay/mobile" => (
            "200 OK",
            "text/html; charset=utf-8",
            include_str!("../pwa/index.html").as_bytes().to_vec(),
        ),
        "/style.css" | "/relay/style.css" => (
            "200 OK",
            "text/css; charset=utf-8",
            include_str!("../pwa/style.css").as_bytes().to_vec(),
        ),
        "/app.js" | "/relay/app.js" => (
            "200 OK",
            "text/javascript; charset=utf-8",
            include_str!("../pwa/app.js").as_bytes().to_vec(),
        ),
        "/relay.js" | "/relay/relay.js" => (
            "200 OK",
            "text/javascript; charset=utf-8",
            include_str!("../pwa/relay.js").as_bytes().to_vec(),
        ),
        "/crypto.js" | "/relay/crypto.js" => (
            "200 OK",
            "text/javascript; charset=utf-8",
            include_str!("../pwa/crypto.js").as_bytes().to_vec(),
        ),
        "/manifest.json" | "/relay/manifest.json" => (
            "200 OK",
            "application/manifest+json; charset=utf-8",
            include_str!("../pwa/manifest.json").as_bytes().to_vec(),
        ),
        "/icon.svg" | "/relay/icon.svg" => (
            "200 OK",
            "image/svg+xml; charset=utf-8",
            include_str!("../pwa/icon.svg").as_bytes().to_vec(),
        ),
        "/health" => (
            "200 OK",
            "application/json; charset=utf-8",
            serde_json::json!({
                "status": "ok",
                "service": "codex-plus-mobile-relay",
                "version": env!("CARGO_PKG_VERSION")
            })
            .to_string()
            .into_bytes(),
        ),
        "/status" => (
            "200 OK",
            "application/json; charset=utf-8",
            serde_json::to_string(&relay_status(&state).await)?.into_bytes(),
        ),
        _ => (
            "404 Not Found",
            "application/json; charset=utf-8",
            serde_json::json!({
                "status": "failed",
                "message": "not found"
            })
            .to_string()
            .into_bytes(),
        ),
    };
    let response = format!(
        concat!(
            "HTTP/1.1 {}\r\n",
            "Content-Type: {}\r\n",
            "Cache-Control: no-store, no-cache, must-revalidate, max-age=0\r\n",
            "Pragma: no-cache\r\n",
            "Expires: 0\r\n",
            "Content-Length: {}\r\n",
            "Connection: close\r\n\r\n"
        ),
        status,
        content_type,
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.shutdown().await?;
    Ok(())
}

fn parse_registration(message: Message) -> anyhow::Result<Registration> {
    let text = match message {
        Message::Text(text) => text.to_string(),
        Message::Binary(bytes) => {
            String::from_utf8(bytes.to_vec()).context("binary registration must be utf-8 json")?
        }
        _ => bail!("first message must be registration json"),
    };
    let registration: RegisterMessage =
        serde_json::from_str(&text).context("registration is not valid json")?;
    if registration.message_type != "register" {
        bail!("registration type must be register");
    }
    if !is_derived_credential(&registration.room) {
        bail!("room must be a 32-character lowercase hexadecimal value");
    }
    if !is_derived_credential(&registration.token) {
        bail!("token must be a 32-character lowercase hexadecimal value");
    }
    let role = Role::from_str(&registration.role).context("role must be host or client")?;
    Ok(Registration {
        role,
        room: registration.room,
        token: registration.token,
    })
}

fn registration_from_uri(path: &str, query: Option<&str>) -> Option<Registration> {
    let query = query?;
    // Nginx proxies `/relay/...` without stripping the prefix, so both the bare
    // and prefixed forms must resolve.
    let normalized = path.strip_prefix("/relay").unwrap_or(path);
    let role = match normalized {
        "/host" => Some(Role::Host),
        "/client" => Some(Role::Client),
        "/ws" => query_value(query, "role").and_then(|role| Role::from_str(&role)),
        _ => None,
    }?;
    let room = query_value(query, "room")?;
    let token = query_value(query, "token")?;
    if !is_derived_credential(&room) || !is_derived_credential(&token) {
        return None;
    }
    Some(Registration { role, room, token })
}

fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then(|| percent_decode(value))
    })
}

#[cfg(test)]
mod uri_tests {
    use super::*;
    use std::path::PathBuf;

    const ROOM: &str = "0123456789abcdef0123456789abcdef";
    const TOKEN: &str = "abcdef0123456789abcdef0123456789";

    fn app_js_source() -> String {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("pwa")
            .join("app.js")
            .to_string_lossy()
            .to_string()
    }

    #[test]
    fn accepts_bare_ws_path() {
        let query = format!("room={ROOM}&token={TOKEN}&role=host");
        let registration = registration_from_uri("/ws", Some(&query)).expect("registration");
        assert_eq!(registration.role, Role::Host);
        assert_eq!(registration.room, ROOM);
        assert_eq!(registration.token, TOKEN);
    }

    /// Nginx forwards `/relay/ws` verbatim, so the prefixed form must work too.
    #[test]
    fn accepts_nginx_prefixed_path() {
        let query = format!("room={ROOM}&token={TOKEN}&role=client");
        let registration = registration_from_uri("/relay/ws", Some(&query)).expect("registration");
        assert_eq!(registration.role, Role::Client);
    }

    #[test]
    fn accepts_prefixed_role_paths() {
        let query = format!("room={ROOM}&token={TOKEN}");
        assert_eq!(
            registration_from_uri("/relay/host", Some(&query))
                .unwrap()
                .role,
            Role::Host
        );
        assert_eq!(
            registration_from_uri("/relay/client", Some(&query))
                .unwrap()
                .role,
            Role::Client
        );
    }

    #[test]
    fn rejects_missing_credentials_and_unknown_paths() {
        assert!(registration_from_uri("/ws", Some("room=abc&role=host")).is_none());
        assert!(registration_from_uri("/ws", Some("room=&token=t&role=host")).is_none());
        assert!(registration_from_uri("/relay/nope", Some("room=r&token=t")).is_none());
        assert!(registration_from_uri("/ws", None).is_none());
    }

    #[test]
    fn rejects_non_derived_or_uppercase_credentials() {
        assert!(registration_from_uri("/ws", Some("room=abc&token=def&role=host")).is_none());
        assert!(registration_from_uri(
            "/ws",
            Some(
                "room=0123456789ABCDEF0123456789ABCDEF&token=abcdef0123456789abcdef0123456789&role=host"
            )
        )
        .is_none());
    }

    #[test]
    fn masks_room_ids_for_public_status() {
        assert_eq!(masked_room_id(ROOM), "012345...cdef");
        assert!(!masked_room_id(ROOM).contains(ROOM));
    }

    #[test]
    fn forwarded_ip_headers_are_ignored_from_untrusted_peers() {
        let proxies = TrustedProxies::default();
        let peer = "203.0.113.10".parse().unwrap();
        let mut headers = tokio_tungstenite::tungstenite::http::HeaderMap::new();
        headers.insert("x-real-ip", "198.51.100.20".parse().unwrap());

        assert_eq!(proxies.resolve_client_ip(peer, &headers), peer);
    }

    #[test]
    fn trusted_proxy_resolves_real_ip_and_forwarded_for_fallback() {
        let proxies = TrustedProxies::parse("127.0.0.1, ::1").unwrap();
        let peer = "127.0.0.1".parse().unwrap();
        let mut headers = tokio_tungstenite::tungstenite::http::HeaderMap::new();
        headers.insert("x-real-ip", "198.51.100.20".parse().unwrap());
        headers.insert("x-forwarded-for", "192.0.2.9, 127.0.0.1".parse().unwrap());
        assert_eq!(
            proxies.resolve_client_ip(peer, &headers),
            "198.51.100.20".parse::<IpAddr>().unwrap()
        );

        headers.insert("x-real-ip", "not-an-ip".parse().unwrap());
        assert_eq!(
            proxies.resolve_client_ip(peer, &headers),
            "192.0.2.9".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn trusted_proxy_configuration_rejects_invalid_addresses() {
        assert!(TrustedProxies::parse("127.0.0.1,not-an-ip").is_err());
    }

    #[test]
    fn mobile_app_consumes_pairing_hash_and_supports_paged_history_and_files() {
        let app_js = std::fs::read_to_string(app_js_source()).expect("read mobile app.js");
        assert!(app_js.contains("window.addEventListener(\"hashchange\""));
        assert!(app_js.contains("useStateDbOnly: true"));
        assert!(app_js.contains("const complete = await loadThreadPage({}"));
        assert!(app_js.contains("\"thread/read\""));
        assert!(app_js.contains("{ threadId: thread.id, includeTurns: false }"));
        assert!(app_js.contains("\"thread/turns/list\""));
        assert!(app_js.contains("limit: TURN_PAGE_SIZE"));
        assert!(app_js.contains("sortDirection: \"desc\""));
        assert!(app_js.contains("turnTimelineSegments"));
        assert!(app_js.contains("\"item/reasoning/summaryTextDelta\""));
        assert!(app_js.contains("STREAM_RENDER_INTERVAL_MS"));
        assert!(app_js.contains("state.connectionMode === \"desktopSync\""));
        assert!(app_js.contains("runtime.pendingSubmission?.state === \"queued\""));
        assert!(app_js.contains("turnId: runtime.turnId"));
        assert!(app_js.contains("baselineUserTextCount"));
        assert!(app_js.contains("scheduleQueuedSubmissionCheck"));
        assert!(app_js.contains("retryConnectBtn"));
        assert!(app_js.contains("function setDrawer("));
        assert!(app_js.contains("\"item/agentMessage/delta\""));
        assert!(app_js.contains("visualViewport"));
        assert!(app_js.contains("presentConversationText"));
        assert!(app_js.contains("normalizeMessageSyntax"));
        assert!(app_js.contains("keyboard-open"));
        assert!(app_js.contains("attachmentBtn"));
        assert!(app_js.contains("refreshSelectedThread"));
        assert!(app_js.contains("if (!resumed || !rpc.initialized) await rpc.initialize()"));
        assert!(app_js.contains("const active = threadIsActive(thread);"));
        assert!(app_js.contains("reconcileResumedThread"));
        assert!(app_js.contains("\"host-offline\""));
        assert!(app_js.contains("配对信息已保留并将继续重试"));
        assert!(app_js.contains("activityLabel"));
        assert!(!app_js.contains("path: thread.path || null"));
        assert!(app_js.contains("friendlyBootstrapError"));
        assert!(app_js.contains("\"fs/readDirectory\""));
        assert!(app_js.contains("rpc.downloadFile"));
        assert!(!app_js.contains("\"fs/readFile\""));
        assert!(app_js.contains("filePreviewKind"));
        assert!(app_js.contains("generation !== state.fileViewerGeneration"));
        assert!(app_js.contains("resolveLocalFilePath"));
        assert!(app_js.contains("fileSourceBtn"));
        assert!(app_js.contains("collapsibleTool"));
        assert!(app_js.contains("updateGlobalRuntimeState"));
        assert!(app_js.contains("updateSyncStatus"));
        assert!(app_js.contains("renderConversationSkeleton"));
    }

    #[tokio::test]
    async fn current_host_disconnect_notifies_and_closes_client() {
        let room_id = ROOM.to_string();
        let token = TOKEN.to_string();
        let (host_sender, _host_receiver) = mpsc::unbounded_channel();
        let (client_sender, mut client_receiver) = mpsc::unbounded_channel();
        let mut room = RoomState::new(token);
        let _ = room.set_sender(Role::Host, host_sender.clone());
        let _ = room.set_sender(Role::Client, client_sender);
        let mut relay_state = RelayState {
            active_connections: 2,
            ..RelayState::default()
        };
        relay_state.rooms.insert(room_id.clone(), room);
        let state = Arc::new(Mutex::new(relay_state));
        let peer = RegisteredPeer {
            room: room_id.clone(),
            role: Role::Host,
            sender: host_sender,
        };

        unregister_peer(&state, &peer).await;

        assert_eq!(
            client_receiver.recv().await,
            Some(Message::Text(RelayErrorCode::HostOffline.to_json().into()))
        );
        assert_eq!(client_receiver.recv().await, Some(Message::Close(None)));
        let state = state.lock().await;
        assert_eq!(state.active_connections, 1);
        assert!(!state.rooms.contains_key(&room_id));
    }

    #[tokio::test]
    async fn replaced_host_disconnect_does_not_close_client() {
        let room_id = ROOM.to_string();
        let token = TOKEN.to_string();
        let (old_host_sender, _old_host_receiver) = mpsc::unbounded_channel();
        let (new_host_sender, _new_host_receiver) = mpsc::unbounded_channel();
        let (client_sender, mut client_receiver) = mpsc::unbounded_channel();
        let mut room = RoomState::new(token);
        let _ = room.set_sender(Role::Host, new_host_sender.clone());
        let _ = room.set_sender(Role::Client, client_sender.clone());
        let mut relay_state = RelayState {
            active_connections: 3,
            ..RelayState::default()
        };
        relay_state.rooms.insert(room_id.clone(), room);
        let state = Arc::new(Mutex::new(relay_state));
        let stale_peer = RegisteredPeer {
            room: room_id,
            role: Role::Host,
            sender: old_host_sender,
        };

        unregister_peer(&state, &stale_peer).await;

        assert!(client_receiver.try_recv().is_err());
        let state = state.lock().await;
        assert_eq!(state.active_connections, 2);
        let room = state.rooms.get(ROOM).expect("room remains registered");
        assert!(
            room.sender_for(Role::Host)
                .is_some_and(|sender| sender.same_channel(&new_host_sender))
        );
        assert!(
            room.sender_for(Role::Client)
                .is_some_and(|sender| sender.same_channel(&client_sender))
        );
    }
}

fn percent_decode(value: &str) -> String {
    let mut output = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = &value[index + 1..index + 3];
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    output.push(byte);
                    index += 3;
                } else {
                    output.push(bytes[index]);
                    index += 1;
                }
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&output).to_string()
}

fn is_derived_credential(value: &str) -> bool {
    value.len() == DERIVED_CREDENTIAL_HEX_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn masked_room_id(room: &str) -> String {
    if room.len() <= 10 {
        return "<redacted>".to_string();
    }
    format!("{}...{}", &room[..6], &room[room.len() - 4..])
}

async fn register_peer(
    state: &Arc<Mutex<RelayState>>,
    registration: Registration,
    sender: mpsc::UnboundedSender<Message>,
    client_ip: IpAddr,
) -> anyhow::Result<RegisteredPeer> {
    let mut state = state.lock().await;
    state.rate_limiter.cleanup_stale();
    if !state.rate_limiter.check_and_consume(client_ip) {
        state.rejected_connections = state.rejected_connections.saturating_add(1);
        return Err(RegistrationRejected(RelayErrorCode::RateLimited).into());
    }
    state.total_connections = state.total_connections.saturating_add(1);
    state.active_connections = state.active_connections.saturating_add(1);
    let room = state
        .rooms
        .entry(registration.room.clone())
        .or_insert_with(|| RoomState::new(registration.token.clone()));
    if room.token != registration.token {
        let room_is_empty = room.is_empty();
        if room_is_empty {
            // Nobody holds the room; let the new credential take ownership so a
            // stale token from a previous process cannot lock a user out.
            room.token = registration.token.clone();
        } else {
            state.active_connections = state.active_connections.saturating_sub(1);
            state.rejected_connections = state.rejected_connections.saturating_add(1);
            return Err(RegistrationRejected(RelayErrorCode::TokenMismatch).into());
        }
    }
    let host_missing = {
        let room = state
            .rooms
            .get(&registration.room)
            .expect("room inserted above");
        registration.role == Role::Client && room.host.is_none()
    };
    if host_missing {
        // A client is useless without its desktop half, and letting it linger
        // would keep an empty room alive as a probe target.
        state.active_connections = state.active_connections.saturating_sub(1);
        state.rejected_connections = state.rejected_connections.saturating_add(1);
        let drop_room = state
            .rooms
            .get(&registration.room)
            .map(RoomState::is_empty)
            .unwrap_or(false);
        if drop_room {
            state.rooms.remove(&registration.room);
        }
        return Err(RegistrationRejected(RelayErrorCode::HostOffline).into());
    }
    let room = state
        .rooms
        .get_mut(&registration.room)
        .expect("room inserted above");
    let previous = room.set_sender(registration.role, sender.clone());
    if registration.role == Role::Client {
        if let Some(previous) = previous {
            let _ = previous.send(Message::Text(
                RelayErrorCode::ClientReplaced.to_json().into(),
            ));
            let _ = previous.send(Message::Close(None));
        }
    } else if let Some(previous) = previous {
        let _ = previous.send(Message::Close(None));
    }
    let _ = sender.send(Message::Text(
        serde_json::json!({
            "type": "registered",
            "role": registration.role.as_str(),
            "room": registration.room
        })
        .to_string()
        .into(),
    ));
    Ok(RegisteredPeer {
        room: registration.room,
        role: registration.role,
        sender,
    })
}

async fn forward_message(state: &Arc<Mutex<RelayState>>, peer: &RegisteredPeer, message: Message) {
    let message_bytes = message_len(&message);
    let target = {
        let mut state = state.lock().await;
        state.forwarded_messages = state.forwarded_messages.saturating_add(1);
        state.forwarded_bytes = state.forwarded_bytes.saturating_add(message_bytes);
        let Some(room) = state.rooms.get_mut(&peer.room) else {
            return;
        };
        room.forwarded_messages = room.forwarded_messages.saturating_add(1);
        room.forwarded_bytes = room.forwarded_bytes.saturating_add(message_bytes);
        let target_role = match peer.role {
            Role::Host => Role::Client,
            Role::Client => Role::Host,
        };
        room.sender_for(target_role)
    };
    if let Some(target) = target {
        let _ = target.send(message);
    }
}

async fn unregister_peer(state: &Arc<Mutex<RelayState>>, peer: &RegisteredPeer) {
    let mut state = state.lock().await;
    state.active_connections = state.active_connections.saturating_sub(1);
    let Some(room) = state.rooms.get_mut(&peer.room) else {
        return;
    };
    let still_same_sender = room
        .sender_for(peer.role)
        .as_ref()
        .map(|sender| sender.same_channel(&peer.sender))
        .unwrap_or(false);
    if still_same_sender {
        room.clear_sender(peer.role);
        if peer.role == Role::Host {
            if let Some(client) = room.sender_for(Role::Client) {
                room.clear_sender(Role::Client);
                let _ = client.send(Message::Text(RelayErrorCode::HostOffline.to_json().into()));
                let _ = client.send(Message::Close(None));
            }
        }
    }
    if room.is_empty() {
        state.rooms.remove(&peer.room);
    }
}

fn message_len(message: &Message) -> u64 {
    match message {
        Message::Text(text) => text.len() as u64,
        Message::Binary(bytes) => bytes.len() as u64,
        Message::Ping(bytes) | Message::Pong(bytes) => bytes.len() as u64,
        Message::Close(_) | Message::Frame(_) => 0,
    }
}

async fn relay_status(state: &Arc<Mutex<RelayState>>) -> RelayStatus {
    let state = state.lock().await;
    let now = Instant::now();
    let mut room_details = state
        .rooms
        .iter()
        .map(|(room, detail)| {
            let host_online = detail.host.is_some();
            let client_online = detail.client.is_some();
            RoomStatus {
                room: masked_room_id(room),
                host_online,
                client_online,
                connections: u8::from(host_online) + u8::from(client_online),
                age_seconds: now.saturating_duration_since(detail.connected_at).as_secs(),
                forwarded_messages: detail.forwarded_messages,
                forwarded_bytes: detail.forwarded_bytes,
            }
        })
        .collect::<Vec<_>>();
    room_details.sort_by(|left, right| left.room.cmp(&right.room));
    RelayStatus {
        status: "ok",
        service: "codex-plus-mobile-relay",
        version: env!("CARGO_PKG_VERSION"),
        uptime_seconds: state
            .started_at
            .map(|started| now.saturating_duration_since(started).as_secs())
            .unwrap_or_default(),
        rooms: state.rooms.len(),
        active_connections: state.active_connections,
        total_connections: state.total_connections,
        forwarded_messages: state.forwarded_messages,
        forwarded_bytes: state.forwarded_bytes,
        room_details,
    }
}

fn relay_test_page() -> String {
    // Public landing page. It deliberately exposes no relay controls: pairing
    // happens through the desktop app, which hands the phone a key-bearing link.
    r##"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>Mirror X Codex 手机中继</title>
<style>
  body { margin: 0; min-height: 100vh; display: grid; place-items: center;
         background: #0b0f0c; color: #edf2e5;
         font: 15px/1.7 system-ui, -apple-system, "Segoe UI", sans-serif; }
  main { width: min(100% - 32px, 460px); padding: 28px;
         border: 1px solid #242a20; border-radius: 10px; background: #101510; }
  h1 { margin: 0 0 6px; font-size: 19px; }
  p { margin: 10px 0 0; color: #98a49a; font-size: 13px; }
  ol { margin: 14px 0 0 18px; padding: 0; color: #b9c4b6; font-size: 13px; }
  li { margin-bottom: 6px; }
  a { color: #c0ad69; }
  code { color: #e2d59e; font-family: ui-monospace, monospace; font-size: 12px; }
</style>
</head>
<body>
<main>
  <h1>Mirror X Codex 手机中继</h1>
  <p>本服务只负责转发加密数据，不保存、也无法解密你的会话内容。</p>
  <ol>
    <li>在电脑上打开 Mirror X Codex，填入镜子AI Key 并应用接入。</li>
    <li>在“手机远程控制”里打开开关，点击“显示手机二维码”。</li>
    <li>用手机相机扫码，浏览器打开后加入主屏幕。</li>
  </ol>
  <p>手机端地址：<code>/relay/mobile</code>。需要电脑保持开机并运行 Mirror X Codex。</p>
  <p><a href="https://api.jingziai.club/">前往镜子AI中转站</a></p>
</main>
</body>
</html>"##
        .to_string()
}
