//! Desktop half of the mobile control channel.
//!
//! The host keeps an outbound WebSocket to the relay, decrypts what the phone
//! sends, and proxies it into a locally spawned `codex app-server` over stdio.
//! Responses travel the reverse path re-encrypted. The relay only ever sees
//! ciphertext.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::{SinkExt, StreamExt};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

const ROOM_SALT: &[u8] = b"mirror-x-room-v1";
const TOKEN_SALT: &[u8] = b"mirror-x-relay-tok-v1";
const ENC_SALT: &[u8] = b"mirror-x-enc-v1";
const MAX_UPLOAD_FILE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_UPLOAD_TOTAL_BYTES: u64 = 200 * 1024 * 1024;
const MAX_UPLOAD_FILES: usize = 20;
const MAX_UPLOAD_CHUNK_BYTES: usize = 256 * 1024;
const MAX_DOWNLOAD_FILE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_DOWNLOAD_CHUNK_BYTES: usize = 256 * 1024;
const DESKTOP_SYNC_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub type MobileRelayStatusReporter = Arc<dyn Fn(MobileRelayHostStatus) + Send + Sync + 'static>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MobileRelayHostPhase {
    Starting,
    RelayConnecting,
    WaitingPhone,
    StartingCodex,
    Ready,
    Reconnecting,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MobileRelayHostStatus {
    pub phase: MobileRelayHostPhase,
    pub message: String,
    pub session_id: Option<String>,
    pub relay_connected: bool,
    pub codex_connected: bool,
}

impl MobileRelayHostStatus {
    fn new(
        phase: MobileRelayHostPhase,
        message: impl Into<String>,
        session_id: Option<String>,
        relay_connected: bool,
        codex_connected: bool,
    ) -> Self {
        Self {
            phase,
            message: message.into(),
            session_id,
            relay_connected,
            codex_connected,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MobileRelayPairingBundle {
    pub version: u8,
    pub room_id: String,
    pub relay_token: String,
    pub enc_key: String,
}

/// Everything the host needs to reach its room, derived purely from the API key.
#[derive(Clone)]
pub struct MobileRelayHostConfig {
    pub relay_url: String,
    pub room_id: String,
    pub relay_token: String,
    pub enc_key: [u8; 32],
}

impl std::fmt::Debug for MobileRelayHostConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never leak the derived key material into logs.
        f.debug_struct("MobileRelayHostConfig")
            .field("relay_url", &self.relay_url)
            .field("room_id", &self.room_id)
            .field("relay_token", &"<redacted>")
            .field("enc_key", &"<redacted>")
            .finish()
    }
}

impl MobileRelayHostConfig {
    pub fn from_api_key(api_key: &str, relay_url: &str) -> Result<Self> {
        let trimmed = api_key.trim();
        if trimmed.is_empty() {
            bail!("mobile relay requires a non-empty api key");
        }
        let relay_url = relay_url.trim();
        if relay_url.is_empty() {
            bail!("mobile relay requires a relay url");
        }

        let ikm = trimmed.as_bytes();
        Ok(Self {
            relay_url: relay_url.to_string(),
            room_id: hex::encode(derive::<16>(ikm, ROOM_SALT)?),
            relay_token: hex::encode(derive::<16>(ikm, TOKEN_SALT)?),
            enc_key: derive::<32>(ikm, ENC_SALT)?,
        })
    }

    /// Websocket URL including role and credentials as query parameters.
    pub fn host_url(&self) -> String {
        let base = self.relay_url.trim_end_matches('/');
        let base = if base.ends_with("/ws") {
            base.to_string()
        } else {
            format!("{base}/ws")
        };
        format!(
            "{base}?room={}&token={}&role=host",
            self.room_id, self.relay_token
        )
    }

    pub fn pairing_bundle(&self) -> MobileRelayPairingBundle {
        MobileRelayPairingBundle {
            version: 1,
            room_id: self.room_id.clone(),
            relay_token: self.relay_token.clone(),
            enc_key: URL_SAFE_NO_PAD.encode(self.enc_key),
        }
    }

    fn mobile_fragment(&self) -> String {
        let payload =
            serde_json::to_vec(&self.pairing_bundle()).expect("pairing bundle must serialize");
        format!("mx={}", URL_SAFE_NO_PAD.encode(payload))
    }

    /// URL the user opens on the phone. It contains only derived relay
    /// credentials, never the upstream API key itself.
    pub fn mobile_url(&self) -> String {
        let https = self
            .relay_url
            .trim_end_matches('/')
            .trim_end_matches("/ws")
            .replacen("wss://", "https://", 1)
            .replacen("ws://", "http://", 1);
        format!("{https}/mobile#{}", self.mobile_fragment())
    }
}

fn derive<const N: usize>(ikm: &[u8], salt: &[u8]) -> Result<[u8; N]> {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut out = [0_u8; N];
    hk.expand(&[], &mut out)
        .map_err(|_| anyhow!("hkdf expand failed"))?;
    Ok(out)
}

fn masked_room_id(room_id: &str) -> String {
    if room_id.len() <= 10 {
        return "<redacted>".to_string();
    }
    format!("{}...{}", &room_id[..6], &room_id[room_id.len() - 4..])
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EncryptedEnvelope {
    #[serde(rename = "type")]
    pub envelope_type: String,
    pub nonce: String,
    pub payload: String,
}

/// Plaintext the phone sends us.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum InboundMessage {
    #[serde(rename = "appServerConnect", rename_all = "camelCase")]
    Connect { session_id: String },
    #[serde(rename = "appServerMessage", rename_all = "camelCase")]
    Rpc { session_id: String, message: String },
    #[serde(rename = "appServerClose", rename_all = "camelCase")]
    Close { session_id: String },
    #[serde(rename = "fileUploadStart", rename_all = "camelCase")]
    FileUploadStart {
        session_id: String,
        request_id: String,
        upload_id: String,
        file_name: String,
        mime_type: String,
        size: u64,
    },
    #[serde(rename = "fileUploadChunk", rename_all = "camelCase")]
    FileUploadChunk {
        session_id: String,
        request_id: String,
        upload_id: String,
        index: u32,
        data: String,
    },
    #[serde(rename = "fileUploadFinish", rename_all = "camelCase")]
    FileUploadFinish {
        session_id: String,
        request_id: String,
        upload_id: String,
    },
    #[serde(rename = "fileUploadCancel", rename_all = "camelCase")]
    FileUploadCancel {
        session_id: String,
        request_id: String,
        upload_id: String,
    },
    #[serde(rename = "fileDownloadRequest", rename_all = "camelCase")]
    FileDownloadRequest {
        session_id: String,
        request_id: String,
        path: String,
        max_bytes: u64,
    },
}

/// Plaintext we send back to the phone.
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum OutboundMessage {
    #[serde(rename = "appServerConnected", rename_all = "camelCase")]
    Connected {
        session_id: String,
        resumed: bool,
        mode: String,
        capabilities: Vec<&'static str>,
    },
    #[serde(rename = "appServerMessage", rename_all = "camelCase")]
    Rpc { session_id: String, message: String },
    #[serde(rename = "appServerClosed", rename_all = "camelCase")]
    Closed { session_id: String, reason: String },
    #[serde(rename = "fileUploadResponse", rename_all = "camelCase")]
    FileUploadResponse {
        session_id: String,
        request_id: String,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        upload_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        received_bytes: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    #[serde(rename = "fileDownloadResponse", rename_all = "camelCase")]
    FileDownloadResponse {
        session_id: String,
        request_id: String,
        ok: bool,
        phase: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        index: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        received_bytes: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

pub fn encrypt(enc_key: &[u8; 32], plaintext: &[u8]) -> Result<EncryptedEnvelope> {
    use rand::RngCore;

    let cipher = Aes256Gcm::new(enc_key.into());
    let mut nonce_bytes = [0_u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext)
        .map_err(|_| anyhow!("aes-gcm encrypt failed"))?;
    Ok(EncryptedEnvelope {
        envelope_type: "encrypted".to_string(),
        nonce: URL_SAFE_NO_PAD.encode(nonce_bytes),
        payload: URL_SAFE_NO_PAD.encode(ciphertext),
    })
}

pub fn decrypt(enc_key: &[u8; 32], envelope: &EncryptedEnvelope) -> Result<Vec<u8>> {
    if envelope.envelope_type != "encrypted" {
        bail!("unexpected envelope type: {}", envelope.envelope_type);
    }
    let nonce = URL_SAFE_NO_PAD
        .decode(&envelope.nonce)
        .context("nonce is not base64url")?;
    if nonce.len() != 12 {
        bail!("nonce must be 12 bytes");
    }
    let ciphertext = URL_SAFE_NO_PAD
        .decode(&envelope.payload)
        .context("payload is not base64url")?;
    let cipher = Aes256Gcm::new(enc_key.into());
    cipher
        .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
        .map_err(|_| anyhow!("aes-gcm decrypt failed"))
}

/// Handle returned to the launcher so it can stop the host on shutdown.
pub struct MobileRelayHostRuntime {
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl MobileRelayHostRuntime {
    pub async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if tokio::time::timeout(std::time::Duration::from_secs(5), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
            let _ = self.task.await;
        }
    }
}

pub fn spawn(config: MobileRelayHostConfig) -> MobileRelayHostRuntime {
    spawn_with_reporter(config, None)
}

pub fn spawn_with_reporter(
    config: MobileRelayHostConfig,
    reporter: Option<MobileRelayStatusReporter>,
) -> MobileRelayHostRuntime {
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(run(config, shutdown_rx, reporter));
    MobileRelayHostRuntime {
        shutdown: Some(shutdown_tx),
        task,
    }
}

fn report_status(reporter: &Option<MobileRelayStatusReporter>, status: MobileRelayHostStatus) {
    if let Some(reporter) = reporter {
        reporter(status);
    }
}

type RelaySender = mpsc::UnboundedSender<Message>;
type RelaySink = Arc<Mutex<Option<RelaySender>>>;
type SessionMap = Arc<Mutex<HashMap<String, SessionHandle>>>;

async fn run(
    config: MobileRelayHostConfig,
    mut shutdown_rx: oneshot::Receiver<()>,
    reporter: Option<MobileRelayStatusReporter>,
) {
    let mut backoff_secs = 1_u64;
    let sessions: SessionMap = Arc::new(Mutex::new(HashMap::new()));
    let relay_sink: RelaySink = Arc::new(Mutex::new(None));
    report_status(
        &reporter,
        MobileRelayHostStatus::new(
            MobileRelayHostPhase::Starting,
            "desktop bridge starting",
            None,
            false,
            false,
        ),
    );
    loop {
        match run_once(
            &config,
            &mut shutdown_rx,
            &sessions,
            &relay_sink,
            reporter.clone(),
        )
        .await
        {
            Ok(RunOnceExit::Completed) => backoff_secs = 1,
            Ok(RunOnceExit::Shutdown) => {
                shutdown_sessions(&sessions).await;
                report_status(
                    &reporter,
                    MobileRelayHostStatus::new(
                        MobileRelayHostPhase::Stopped,
                        "desktop bridge stopped",
                        None,
                        false,
                        false,
                    ),
                );
                return;
            }
            Err(error) => {
                let _ = crate::diagnostic_log::append_diagnostic_log(
                    "mobile_relay.host_error",
                    serde_json::json!({
                        "room": masked_room_id(&config.room_id),
                        "error": error.to_string(),
                    }),
                );
                report_status(
                    &reporter,
                    MobileRelayHostStatus::new(
                        MobileRelayHostPhase::Reconnecting,
                        error.to_string(),
                        None,
                        false,
                        false,
                    ),
                );
                let wait = std::time::Duration::from_secs(backoff_secs);
                backoff_secs = (backoff_secs * 2).min(30);
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        shutdown_sessions(&sessions).await;
                        report_status(
                            &reporter,
                            MobileRelayHostStatus::new(
                                MobileRelayHostPhase::Stopped,
                                "desktop bridge stopped",
                                None,
                                false,
                                false,
                            ),
                        );
                        return;
                    },
                    _ = tokio::time::sleep(wait) => {}
                }
            }
        }
    }
}

enum RunOnceExit {
    Completed,
    Shutdown,
}

async fn run_once(
    config: &MobileRelayHostConfig,
    shutdown_rx: &mut oneshot::Receiver<()>,
    sessions: &SessionMap,
    relay_sink: &RelaySink,
    reporter: Option<MobileRelayStatusReporter>,
) -> Result<RunOnceExit> {
    report_status(
        &reporter,
        MobileRelayHostStatus::new(
            MobileRelayHostPhase::RelayConnecting,
            "connecting to relay",
            None,
            false,
            false,
        ),
    );
    let (stream, _) = tokio::select! {
        _ = &mut *shutdown_rx => return Ok(RunOnceExit::Shutdown),
        result = connect_async(config.host_url()) => {
            result.context("failed to connect mobile relay")?
        }
    };
    report_status(
        &reporter,
        MobileRelayHostStatus::new(
            MobileRelayHostPhase::WaitingPhone,
            "waiting for phone",
            None,
            true,
            false,
        ),
    );
    let (mut writer, mut reader) = stream.split();

    // Fan-in channel so app-server readers and the message loop share one sink.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
    *relay_sink.lock().await = Some(out_tx.clone());
    let pump = tokio::spawn(async move {
        while let Some(message) = out_rx.recv().await {
            if writer.send(message).await.is_err() {
                break;
            }
        }
    });

    let result = message_loop(
        config,
        &mut reader,
        &out_tx,
        sessions,
        relay_sink,
        shutdown_rx,
        reporter.clone(),
    )
    .await;

    {
        let mut sink = relay_sink.lock().await;
        let is_current = sink
            .as_ref()
            .map(|current| current.same_channel(&out_tx))
            .unwrap_or(false);
        if is_current {
            *sink = None;
        }
    }
    pump.abort();
    match result? {
        MessageLoopExit::Completed => Ok(RunOnceExit::Completed),
        MessageLoopExit::Shutdown => Ok(RunOnceExit::Shutdown),
    }
}

enum MessageLoopExit {
    Completed,
    Shutdown,
}

async fn message_loop<R>(
    config: &MobileRelayHostConfig,
    reader: &mut R,
    out_tx: &mpsc::UnboundedSender<Message>,
    sessions: &SessionMap,
    relay_sink: &RelaySink,
    shutdown_rx: &mut oneshot::Receiver<()>,
    reporter: Option<MobileRelayStatusReporter>,
) -> Result<MessageLoopExit>
where
    R: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let next = tokio::select! {
            _ = &mut *shutdown_rx => return Ok(MessageLoopExit::Shutdown),
            message = reader.next() => message,
        };
        let Some(message) = next else {
            return Ok(MessageLoopExit::Completed);
        };
        let message = message.context("relay read failed")?;
        let text = match message {
            Message::Text(text) => text.to_string(),
            Message::Binary(bytes) => match String::from_utf8(bytes.to_vec()) {
                Ok(text) => text,
                Err(_) => continue,
            },
            Message::Close(_) => return Ok(MessageLoopExit::Completed),
            _ => continue,
        };

        // Relay control frames (registered / error) are plaintext; skip them.
        let envelope: EncryptedEnvelope = match serde_json::from_str(&text) {
            Ok(envelope) => envelope,
            Err(_) => continue,
        };

        let plaintext = match decrypt(&config.enc_key, &envelope) {
            Ok(plaintext) => plaintext,
            Err(error) => {
                // A wrong key is the expected cause; drop the frame and keep the
                // socket so a correct client can still take over the room.
                let _ = crate::diagnostic_log::append_diagnostic_log(
                    "mobile_relay.decrypt_failed",
                    serde_json::json!({ "error": error.to_string() }),
                );
                continue;
            }
        };

        let inbound: InboundMessage = match serde_json::from_slice(&plaintext) {
            Ok(inbound) => inbound,
            Err(_) => continue,
        };

        handle_inbound(
            config,
            inbound,
            out_tx,
            sessions,
            relay_sink,
            reporter.clone(),
        )
        .await?;
    }
}

async fn handle_inbound(
    config: &MobileRelayHostConfig,
    inbound: InboundMessage,
    out_tx: &mpsc::UnboundedSender<Message>,
    sessions: &SessionMap,
    relay_sink: &RelaySink,
    reporter: Option<MobileRelayStatusReporter>,
) -> Result<()> {
    match inbound {
        InboundMessage::Connect { session_id } => {
            let connect_started = std::time::Instant::now();
            let _ = crate::diagnostic_log::append_diagnostic_log(
                "mobile_relay.connect_received",
                serde_json::json!({ "sessionId": session_id }),
            );
            let existing = {
                let sessions = sessions.lock().await;
                sessions.get(&session_id).map(|session| {
                    (
                        session.child.as_ref().map(Arc::clone),
                        session
                            .desktop
                            .as_ref()
                            .map(crate::desktop_sync::DesktopSyncRuntime::is_alive),
                        session.mode.clone(),
                    )
                })
            };
            let (session_is_running, existing_mode) = match existing {
                Some((Some(child), _, mode)) => {
                    (matches!(child.lock().await.try_wait(), Ok(None)), mode)
                }
                Some((None, Some(alive), mode)) => (alive, mode),
                _ => (false, "standalone".to_string()),
            };
            if session_is_running {
                send_encrypted(
                    &config.enc_key,
                    out_tx,
                    &OutboundMessage::Connected {
                        session_id: session_id.clone(),
                        resumed: true,
                        mode: existing_mode,
                        capabilities: vec!["fileDownloadChunks"],
                    },
                )?;
                report_status(
                    &reporter,
                    MobileRelayHostStatus::new(
                        MobileRelayHostPhase::Ready,
                        "phone session resumed",
                        Some(session_id),
                        true,
                        true,
                    ),
                );
                return Ok(());
            }
            if let Some(stale) = sessions.lock().await.remove(&session_id) {
                stale.shutdown().await;
            }

            report_status(
                &reporter,
                MobileRelayHostStatus::new(
                    MobileRelayHostPhase::StartingCodex,
                    "connecting to Codex Desktop",
                    Some(session_id.clone()),
                    true,
                    false,
                ),
            );
            let previous_sessions = {
                let mut sessions = sessions.lock().await;
                sessions
                    .drain()
                    .map(|(_, session)| session)
                    .collect::<Vec<_>>()
            };
            for previous in previous_sessions {
                previous.shutdown().await;
            }
            let handle = match SessionHandle::start(
                session_id.clone(),
                config.enc_key,
                Arc::clone(relay_sink),
                reporter.clone(),
            )
            .await
            {
                Ok(handle) => handle,
                Err(error) => {
                    let reason = format!("Codex initialization failed: {error}");
                    let _ = crate::diagnostic_log::append_diagnostic_log(
                        "mobile_relay.session_start_failed",
                        serde_json::json!({
                            "sessionId": session_id,
                            "elapsedMs": connect_started.elapsed().as_millis(),
                            "error": error.to_string()
                        }),
                    );
                    send_encrypted(
                        &config.enc_key,
                        out_tx,
                        &OutboundMessage::Closed {
                            session_id: session_id.clone(),
                            reason: reason.clone(),
                        },
                    )?;
                    report_status(
                        &reporter,
                        MobileRelayHostStatus::new(
                            MobileRelayHostPhase::WaitingPhone,
                            reason,
                            None,
                            true,
                            false,
                        ),
                    );
                    return Ok(());
                }
            };
            let connected_mode = handle.mode.clone();
            sessions.lock().await.insert(session_id.clone(), handle);
            send_encrypted(
                &config.enc_key,
                out_tx,
                &OutboundMessage::Connected {
                    session_id: session_id.clone(),
                    resumed: false,
                    mode: connected_mode.clone(),
                    capabilities: vec!["fileDownloadChunks"],
                },
            )?;
            report_status(
                &reporter,
                MobileRelayHostStatus::new(
                    MobileRelayHostPhase::Ready,
                    if connected_mode == "desktopSync" {
                        "phone connected to Codex Desktop"
                    } else {
                        "phone connected in standalone fallback"
                    },
                    Some(session_id.clone()),
                    true,
                    true,
                ),
            );
            let _ = crate::diagnostic_log::append_diagnostic_log(
                "mobile_relay.session_connected",
                serde_json::json!({
                    "sessionId": session_id,
                    "mode": connected_mode,
                    "elapsedMs": connect_started.elapsed().as_millis()
                }),
            );
        }
        InboundMessage::Rpc {
            session_id,
            message,
        } => {
            let sender = sessions
                .lock()
                .await
                .get(&session_id)
                .map(|session| session.stdin_tx.clone());
            match sender {
                Some(sender) => {
                    let _ = sender.send(message);
                }
                None => send_encrypted(
                    &config.enc_key,
                    out_tx,
                    &OutboundMessage::Closed {
                        session_id,
                        reason: "session not found".to_string(),
                    },
                )?,
            }
        }
        InboundMessage::FileUploadStart {
            session_id,
            request_id,
            upload_id,
            file_name,
            mime_type,
            size,
        } => {
            let uploads = sessions
                .lock()
                .await
                .get(&session_id)
                .map(|session| session.uploads.clone());
            let result = match uploads {
                Some(uploads) => uploads
                    .start(&upload_id, &file_name, &mime_type, size)
                    .await
                    .map(|_| (None, Some(0))),
                None => Err(anyhow!("手机会话不存在，请重新连接")),
            };
            send_file_upload_result(
                &config.enc_key,
                out_tx,
                session_id,
                request_id,
                Some(upload_id),
                result,
            )?;
        }
        InboundMessage::FileUploadChunk {
            session_id,
            request_id,
            upload_id,
            index,
            data,
        } => {
            let uploads = sessions
                .lock()
                .await
                .get(&session_id)
                .map(|session| session.uploads.clone());
            let result = match uploads {
                Some(uploads) => uploads
                    .append_chunk(&upload_id, index, &data)
                    .await
                    .map(|received| (None, Some(received))),
                None => Err(anyhow!("手机会话不存在，请重新连接")),
            };
            send_file_upload_result(
                &config.enc_key,
                out_tx,
                session_id,
                request_id,
                Some(upload_id),
                result,
            )?;
        }
        InboundMessage::FileUploadFinish {
            session_id,
            request_id,
            upload_id,
        } => {
            let uploads = sessions
                .lock()
                .await
                .get(&session_id)
                .map(|session| session.uploads.clone());
            let result = match uploads {
                Some(uploads) => uploads
                    .finish(&upload_id)
                    .await
                    .map(|path| (Some(path.to_string_lossy().to_string()), None)),
                None => Err(anyhow!("手机会话不存在，请重新连接")),
            };
            send_file_upload_result(
                &config.enc_key,
                out_tx,
                session_id,
                request_id,
                Some(upload_id),
                result,
            )?;
        }
        InboundMessage::FileUploadCancel {
            session_id,
            request_id,
            upload_id,
        } => {
            let uploads = sessions
                .lock()
                .await
                .get(&session_id)
                .map(|session| session.uploads.clone());
            let result = match uploads {
                Some(uploads) => uploads.cancel(&upload_id).await.map(|_| (None, None)),
                None => Err(anyhow!("手机会话不存在，请重新连接")),
            };
            send_file_upload_result(
                &config.enc_key,
                out_tx,
                session_id,
                request_id,
                Some(upload_id),
                result,
            )?;
        }
        InboundMessage::FileDownloadRequest {
            session_id,
            request_id,
            path,
            max_bytes,
        } => {
            let session_exists = sessions.lock().await.contains_key(&session_id);
            let result = if session_exists {
                send_file_download(
                    &config.enc_key,
                    out_tx,
                    &session_id,
                    &request_id,
                    &path,
                    max_bytes,
                )
                .await
            } else {
                Err(anyhow!("手机会话不存在，请重新连接"))
            };
            if let Err(error) = result {
                send_encrypted(
                    &config.enc_key,
                    out_tx,
                    &OutboundMessage::FileDownloadResponse {
                        session_id,
                        request_id,
                        ok: false,
                        phase: "error",
                        size: None,
                        index: None,
                        data: None,
                        received_bytes: None,
                        error: Some(error.to_string()),
                    },
                )?;
            }
        }
        InboundMessage::Close { session_id } => {
            if let Some(session) = sessions.lock().await.remove(&session_id) {
                session.shutdown().await;
            }
            report_status(
                &reporter,
                MobileRelayHostStatus::new(
                    MobileRelayHostPhase::WaitingPhone,
                    "waiting for phone",
                    None,
                    true,
                    false,
                ),
            );
            send_encrypted(
                &config.enc_key,
                out_tx,
                &OutboundMessage::Closed {
                    session_id,
                    reason: "closed by client".to_string(),
                },
            )?;
        }
    }
    Ok(())
}

fn send_file_upload_result(
    enc_key: &[u8; 32],
    out_tx: &mpsc::UnboundedSender<Message>,
    session_id: String,
    request_id: String,
    upload_id: Option<String>,
    result: Result<(Option<String>, Option<u64>)>,
) -> Result<()> {
    let outbound = match result {
        Ok((path, received_bytes)) => OutboundMessage::FileUploadResponse {
            session_id,
            request_id,
            ok: true,
            upload_id,
            path,
            received_bytes,
            error: None,
        },
        Err(error) => OutboundMessage::FileUploadResponse {
            session_id,
            request_id,
            ok: false,
            upload_id,
            path: None,
            received_bytes: None,
            error: Some(error.to_string()),
        },
    };
    send_encrypted(enc_key, out_tx, &outbound)
}

async fn send_file_download(
    enc_key: &[u8; 32],
    out_tx: &mpsc::UnboundedSender<Message>,
    session_id: &str,
    request_id: &str,
    path: &str,
    max_bytes: u64,
) -> Result<()> {
    if max_bytes == 0 || max_bytes > MAX_DOWNLOAD_FILE_BYTES {
        bail!("手机端文件预览上限无效");
    }
    let canonical = tokio::fs::canonicalize(path)
        .await
        .with_context(|| format!("无法读取文件: {path}"))?;
    let metadata = tokio::fs::metadata(&canonical)
        .await
        .with_context(|| format!("无法读取文件信息: {}", canonical.display()))?;
    if !metadata.is_file() {
        bail!("所选路径不是文件");
    }
    if metadata.len() > max_bytes {
        bail!("文件超过 {} MB，手机端不预览", max_bytes / 1024 / 1024);
    }

    send_encrypted(
        enc_key,
        out_tx,
        &OutboundMessage::FileDownloadResponse {
            session_id: session_id.to_string(),
            request_id: request_id.to_string(),
            ok: true,
            phase: "start",
            size: Some(metadata.len()),
            index: None,
            data: None,
            received_bytes: Some(0),
            error: None,
        },
    )?;

    let mut file = tokio::fs::File::open(&canonical)
        .await
        .with_context(|| format!("无法打开文件: {}", canonical.display()))?;
    let mut buffer = vec![0_u8; MAX_DOWNLOAD_CHUNK_BYTES];
    let mut index = 0_u32;
    let mut received_bytes = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .with_context(|| format!("读取文件失败: {}", canonical.display()))?;
        if read == 0 {
            break;
        }
        received_bytes += read as u64;
        if received_bytes > metadata.len() || received_bytes > max_bytes {
            bail!("文件在读取过程中发生变化，请重试");
        }
        send_encrypted(
            enc_key,
            out_tx,
            &OutboundMessage::FileDownloadResponse {
                session_id: session_id.to_string(),
                request_id: request_id.to_string(),
                ok: true,
                phase: "chunk",
                size: None,
                index: Some(index),
                data: Some(URL_SAFE_NO_PAD.encode(&buffer[..read])),
                received_bytes: Some(received_bytes),
                error: None,
            },
        )?;
        index += 1;
    }
    if received_bytes != metadata.len() {
        bail!("文件在读取过程中发生变化，请重试");
    }
    send_encrypted(
        enc_key,
        out_tx,
        &OutboundMessage::FileDownloadResponse {
            session_id: session_id.to_string(),
            request_id: request_id.to_string(),
            ok: true,
            phase: "finish",
            size: Some(metadata.len()),
            index: Some(index),
            data: None,
            received_bytes: Some(received_bytes),
            error: None,
        },
    )
}

fn send_encrypted(
    enc_key: &[u8; 32],
    out_tx: &mpsc::UnboundedSender<Message>,
    outbound: &OutboundMessage,
) -> Result<()> {
    let plaintext = serde_json::to_vec(outbound)?;
    let envelope = encrypt(enc_key, &plaintext)?;
    let text = serde_json::to_string(&envelope)?;
    out_tx
        .send(Message::Text(text.into()))
        .map_err(|_| anyhow!("relay writer closed"))
}

async fn send_encrypted_to_sink(
    enc_key: &[u8; 32],
    relay_sink: &RelaySink,
    outbound: &OutboundMessage,
) -> Result<bool> {
    let sender = relay_sink.lock().await.clone();
    let Some(sender) = sender else {
        return Ok(false);
    };
    send_encrypted(enc_key, &sender, outbound)?;
    Ok(true)
}

async fn shutdown_sessions(sessions: &SessionMap) {
    let handles = {
        let mut sessions = sessions.lock().await;
        sessions
            .drain()
            .map(|(_, session)| session)
            .collect::<Vec<_>>()
    };
    for session in handles {
        session.shutdown().await;
    }
}

#[derive(Clone)]
struct SessionUploads {
    root: PathBuf,
    state: Arc<Mutex<UploadStore>>,
}

struct UploadStore {
    pending: HashMap<String, PendingUpload>,
    completed_bytes: u64,
    completed_files: usize,
}

struct PendingUpload {
    path: PathBuf,
    file: tokio::fs::File,
    expected_size: u64,
    received_bytes: u64,
    next_index: u32,
}

impl SessionUploads {
    async fn create(session_id: &str) -> Result<Self> {
        let session_hash = hex::encode(Sha256::digest(session_id.as_bytes()));
        let root = std::env::temp_dir()
            .join("mirror-x-codex")
            .join("mobile-uploads")
            .join(&session_hash[..24]);
        tokio::fs::create_dir_all(&root).await.with_context(|| {
            format!(
                "failed to prepare mobile upload directory: {}",
                root.display()
            )
        })?;
        Ok(Self {
            root,
            state: Arc::new(Mutex::new(UploadStore {
                pending: HashMap::new(),
                completed_bytes: 0,
                completed_files: 0,
            })),
        })
    }

    async fn start(
        &self,
        upload_id: &str,
        file_name: &str,
        _mime_type: &str,
        size: u64,
    ) -> Result<()> {
        validate_upload_id(upload_id)?;
        if size == 0 {
            bail!("附件为空");
        }
        if size > MAX_UPLOAD_FILE_BYTES {
            bail!("单个附件不能超过 25 MB");
        }
        let safe_name = sanitize_upload_file_name(file_name);
        let path = self.root.join(format!("{upload_id}-{safe_name}"));
        let mut state = self.state.lock().await;
        if state.pending.contains_key(upload_id) {
            bail!("附件上传编号重复");
        }
        let pending_bytes = state
            .pending
            .values()
            .map(|upload| upload.expected_size)
            .sum::<u64>();
        if state.completed_files + state.pending.len() >= MAX_UPLOAD_FILES {
            bail!("一次连接最多保留 20 个附件");
        }
        if state.completed_bytes + pending_bytes + size > MAX_UPLOAD_TOTAL_BYTES {
            bail!("本次连接的附件总量不能超过 200 MB");
        }
        let file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .await
            .with_context(|| format!("无法创建本机附件文件: {}", path.display()))?;
        state.pending.insert(
            upload_id.to_string(),
            PendingUpload {
                path,
                file,
                expected_size: size,
                received_bytes: 0,
                next_index: 0,
            },
        );
        Ok(())
    }

    async fn append_chunk(&self, upload_id: &str, index: u32, data: &str) -> Result<u64> {
        validate_upload_id(upload_id)?;
        if data.len() > (MAX_UPLOAD_CHUNK_BYTES * 4 / 3) + 16 {
            bail!("附件分块过大");
        }
        let decoded = URL_SAFE_NO_PAD
            .decode(data)
            .context("附件分块不是有效的 base64url")?;
        if decoded.is_empty() || decoded.len() > MAX_UPLOAD_CHUNK_BYTES {
            bail!("附件分块大小无效");
        }
        let mut state = self.state.lock().await;
        let upload = state
            .pending
            .get_mut(upload_id)
            .context("附件上传不存在或已结束")?;
        if index != upload.next_index {
            bail!("附件分块顺序错误");
        }
        let next_size = upload.received_bytes + decoded.len() as u64;
        if next_size > upload.expected_size {
            bail!("附件内容超过声明大小");
        }
        upload
            .file
            .write_all(&decoded)
            .await
            .context("写入本机附件失败")?;
        upload.received_bytes = next_size;
        upload.next_index += 1;
        Ok(upload.received_bytes)
    }

    async fn finish(&self, upload_id: &str) -> Result<PathBuf> {
        validate_upload_id(upload_id)?;
        let mut upload = {
            let mut state = self.state.lock().await;
            state
                .pending
                .remove(upload_id)
                .context("附件上传不存在或已结束")?
        };
        if upload.received_bytes != upload.expected_size {
            let _ = tokio::fs::remove_file(&upload.path).await;
            bail!(
                "附件大小不完整：应为 {} 字节，实际 {} 字节",
                upload.expected_size,
                upload.received_bytes
            );
        }
        upload.file.flush().await.context("刷新附件文件失败")?;
        upload.file.sync_data().await.context("保存附件文件失败")?;
        drop(upload.file);
        let canonical = tokio::fs::canonicalize(&upload.path)
            .await
            .context("无法确认附件本机路径")?;
        let mut state = self.state.lock().await;
        state.completed_bytes += upload.received_bytes;
        state.completed_files += 1;
        Ok(canonical)
    }

    async fn cancel(&self, upload_id: &str) -> Result<()> {
        validate_upload_id(upload_id)?;
        let upload = self.state.lock().await.pending.remove(upload_id);
        if let Some(upload) = upload {
            drop(upload.file);
            let _ = tokio::fs::remove_file(upload.path).await;
        }
        Ok(())
    }

    async fn cleanup(self) {
        let root = self.root;
        let expected_parent = std::env::temp_dir()
            .join("mirror-x-codex")
            .join("mobile-uploads");
        if root.starts_with(&expected_parent) && root != expected_parent {
            let _ = tokio::fs::remove_dir_all(root).await;
        }
    }
}

fn validate_upload_id(upload_id: &str) -> Result<()> {
    if !(8..=96).contains(&upload_id.len())
        || !upload_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        bail!("附件上传编号无效");
    }
    Ok(())
}

fn sanitize_upload_file_name(file_name: &str) -> String {
    let base = Path::new(file_name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("attachment.bin");
    let sanitized = base
        .chars()
        .filter(|ch| !ch.is_control() && !r#"<>:"/\|?*"#.contains(*ch))
        .take(120)
        .collect::<String>();
    let sanitized = sanitized.trim_matches([' ', '.']);
    if sanitized.is_empty() {
        "attachment.bin".to_string()
    } else {
        sanitized.to_string()
    }
}

/// One phone session. Prefer the existing Codex Desktop App Server connection;
/// retain the previous child-process backend only as a compatibility fallback
/// when Desktop was not launched with a reachable CDP endpoint.
struct SessionHandle {
    stdin_tx: mpsc::UnboundedSender<String>,
    mode: String,
    child: Option<Arc<Mutex<Child>>>,
    desktop: Option<crate::desktop_sync::DesktopSyncRuntime>,
    reader_task: tokio::task::JoinHandle<()>,
    writer_task: Option<tokio::task::JoinHandle<()>>,
    uploads: SessionUploads,
}

impl SessionHandle {
    async fn start(
        session_id: String,
        enc_key: [u8; 32],
        relay_sink: RelaySink,
        reporter: Option<MobileRelayStatusReporter>,
    ) -> Result<Self> {
        let uploads = SessionUploads::create(&session_id).await?;
        let desktop_start = tokio::time::timeout(
            DESKTOP_SYNC_START_TIMEOUT,
            crate::desktop_sync::start(&session_id),
        )
        .await;
        match desktop_start {
            Ok(Ok((desktop, mut output_rx))) => {
                let stdin_tx = desktop.input_tx.clone();
                let reader_session = session_id.clone();
                let reader_reporter = reporter.clone();
                let reader_task = tokio::spawn(async move {
                    while let Some(line) = output_rx.recv().await {
                        if line.trim().is_empty() {
                            continue;
                        }
                        let outbound = OutboundMessage::Rpc {
                            session_id: reader_session.clone(),
                            message: line,
                        };
                        // Relay restarts are transient. Keep consuming the
                        // Desktop stream while no relay sink exists so the
                        // same CDP session can resume after reconnection.
                        let _ = send_encrypted_to_sink(&enc_key, &relay_sink, &outbound).await;
                    }
                    let _ = send_encrypted_to_sink(
                        &enc_key,
                        &relay_sink,
                        &OutboundMessage::Closed {
                            session_id: reader_session,
                            reason: "Codex Desktop sync disconnected".to_string(),
                        },
                    )
                    .await;
                    report_status(
                        &reader_reporter,
                        MobileRelayHostStatus::new(
                            MobileRelayHostPhase::WaitingPhone,
                            "Codex Desktop sync disconnected",
                            None,
                            true,
                            false,
                        ),
                    );
                });
                let _ = crate::diagnostic_log::append_diagnostic_log(
                    "mobile_relay.desktop_sync_connected",
                    serde_json::json!({ "sessionId": session_id }),
                );
                return Ok(Self {
                    stdin_tx,
                    mode: "desktopSync".to_string(),
                    child: None,
                    desktop: Some(desktop),
                    reader_task,
                    writer_task: None,
                    uploads,
                });
            }
            Ok(Err(error)) => {
                let _ = crate::diagnostic_log::append_diagnostic_log(
                    "mobile_relay.desktop_sync_unavailable",
                    serde_json::json!({
                        "sessionId": session_id,
                        "error": error.to_string()
                    }),
                );
            }
            Err(_) => {
                let _ = crate::diagnostic_log::append_diagnostic_log(
                    "mobile_relay.desktop_sync_unavailable",
                    serde_json::json!({
                        "sessionId": session_id,
                        "error": format!(
                            "desktop sync startup timed out after {} seconds",
                            DESKTOP_SYNC_START_TIMEOUT.as_secs()
                        )
                    }),
                );
            }
        }

        Self::start_standalone(session_id, enc_key, relay_sink, reporter, uploads).await
    }

    async fn start_standalone(
        session_id: String,
        enc_key: [u8; 32],
        relay_sink: RelaySink,
        reporter: Option<MobileRelayStatusReporter>,
        uploads: SessionUploads,
    ) -> Result<Self> {
        let codex = resolve_codex_cli()?;
        let mut command = Command::new(&codex);
        command
            .arg("app-server")
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            command.creation_flags(crate::windows_create_no_window());
        }

        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start codex app-server: {codex}"))?;
        let stdin = child.stdin.take().context("app-server stdin missing")?;
        let stdout = child.stdout.take().context("app-server stdout missing")?;

        let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<String>();
        let writer_task = tokio::spawn(async move {
            let mut stdin: ChildStdin = stdin;
            while let Some(line) = stdin_rx.recv().await {
                let payload = format!("{}\n", line.trim_end());
                if stdin.write_all(payload.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.flush().await.is_err() {
                    break;
                }
            }
        });

        let reader_session = session_id.clone();
        let reader_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        let outbound = OutboundMessage::Rpc {
                            session_id: reader_session.clone(),
                            message: line,
                        };
                        let _ = send_encrypted_to_sink(&enc_key, &relay_sink, &outbound).await;
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            let _ = send_encrypted_to_sink(
                &enc_key,
                &relay_sink,
                &OutboundMessage::Closed {
                    session_id: reader_session,
                    reason: "app-server exited".to_string(),
                },
            )
            .await;
            report_status(
                &reporter,
                MobileRelayHostStatus::new(
                    MobileRelayHostPhase::WaitingPhone,
                    "codex app-server exited",
                    None,
                    true,
                    false,
                ),
            );
        });

        Ok(Self {
            stdin_tx,
            mode: "standalone".to_string(),
            child: Some(Arc::new(Mutex::new(child))),
            desktop: None,
            reader_task,
            writer_task: Some(writer_task),
            uploads,
        })
    }

    async fn shutdown(self) {
        self.reader_task.abort();
        if let Some(writer_task) = self.writer_task {
            writer_task.abort();
        }
        if let Some(desktop) = self.desktop {
            desktop.stop().await;
        }
        if let Some(child) = self.child {
            let _ = child.lock().await.kill().await;
        }
        self.uploads.cleanup().await;
    }
}

pub(crate) fn resolve_codex_cli() -> Result<String> {
    if let Some(path) = std::env::var_os("CODEX_CLI_PATH") {
        let path = std::path::PathBuf::from(path);
        if path.as_os_str().to_string_lossy().trim().is_empty() {
            bail!("CODEX_CLI_PATH is set but empty");
        }
        if !path.is_file() {
            bail!(
                "CODEX_CLI_PATH does not point to a file: {}",
                path.display()
            );
        }
        return Ok(path.to_string_lossy().to_string());
    }
    #[cfg(windows)]
    if let Some(path) = find_windows_codex_cli()? {
        return Ok(path);
    }
    #[cfg(not(windows))]
    if let Some(app_dir) = crate::app_paths::resolve_codex_app_dir(None) {
        if let Some(candidate) = crate::app_paths::find_bundled_codex_cli(&app_dir) {
            return Ok(candidate.to_string_lossy().to_string());
        }
    }
    Ok("codex".to_string())
}

#[cfg(windows)]
fn find_windows_codex_cli() -> Result<Option<String>> {
    let saved_app_path = crate::settings::SettingsStore::default()
        .load()
        .ok()
        .map(|settings| settings.codex_app_path);
    if let Some(app_dir) =
        crate::app_paths::resolve_codex_app_dir_with_saved(None, saved_app_path.as_deref())
    {
        let executable = materialize_codex_cli_for_app(&app_dir)?;
        return Ok(Some(executable.to_string_lossy().to_string()));
    }
    if let Some(path) = find_private_windows_codex_cli() {
        return Ok(Some(path.to_string_lossy().to_string()));
    }
    for command in ["codex.cmd", "codex.exe", "codex"] {
        if let Some(path) = first_where_candidate(command) {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

#[cfg(windows)]
pub(crate) fn materialize_codex_cli_for_app(app_dir: &Path) -> Result<std::path::PathBuf> {
    let source = crate::app_paths::find_bundled_codex_cli(app_dir).with_context(|| {
        format!(
            "Codex App does not contain a readable bundled CLI: {}",
            app_dir.display()
        )
    })?;
    if !path_is_windowsapps_resource(&source) {
        return Ok(source);
    }

    prepare_windowsapps_codex_cli_cache(app_dir).with_context(|| {
        format!(
            "failed to create an executable private copy of the selected Codex CLI: {}",
            source.display()
        )
    })
}

#[cfg(windows)]
fn find_private_windows_codex_cli() -> Option<std::path::PathBuf> {
    private_windows_codex_cli_candidates()
        .into_iter()
        .find(|path| path.is_file())
}

#[cfg(windows)]
fn private_windows_codex_cli_candidates() -> Vec<std::path::PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(current_exe) = std::env::current_exe()
        && let Some(parent) = current_exe.parent()
    {
        candidates.push(parent.join("runtime").join("codex-mobile-host.exe"));
        candidates.push(parent.join("codex-mobile-host.exe"));
    }
    candidates.push(
        crate::paths::default_app_state_dir()
            .join("runtime")
            .join("codex-mobile-host.exe"),
    );
    candidates
}

#[cfg(windows)]
fn prepare_windowsapps_codex_cli_cache(app_dir: &Path) -> Option<std::path::PathBuf> {
    let source = crate::app_paths::find_bundled_codex_cli(app_dir)?;
    if !source.is_file() || !path_is_windowsapps_resource(&source) {
        return None;
    }

    let source_fingerprint = cli_fingerprint(&source)?;
    if let Some(candidate) = private_windows_codex_cli_candidates()
        .into_iter()
        .find(|candidate| cli_fingerprint(candidate).as_ref() == Some(&source_fingerprint))
    {
        return Some(candidate);
    }

    let target = crate::paths::default_app_state_dir()
        .join("runtime")
        .join("codex-mobile-host.exe");
    refresh_cached_codex_cli(&source, &target)
}

#[cfg(windows)]
fn refresh_cached_codex_cli(
    source: &std::path::Path,
    target: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let source_fingerprint = cli_fingerprint(source)?;
    if cli_fingerprint(target).as_ref() == Some(&source_fingerprint) {
        return Some(target.to_path_buf());
    }

    let parent = target.parent()?;
    std::fs::create_dir_all(parent).ok()?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let staging = parent.join(format!(
        "codex-mobile-host-{}-{nonce}.tmp",
        std::process::id()
    ));
    if std::fs::copy(source, &staging).is_err() {
        let _ = std::fs::remove_file(&staging);
        return None;
    }
    if cli_fingerprint(&staging).as_ref() != Some(&source_fingerprint) {
        let _ = std::fs::remove_file(&staging);
        return None;
    }

    if std::fs::rename(&staging, &target).is_err() {
        if target.is_file()
            && std::fs::remove_file(target).is_err()
            && cli_fingerprint(target).as_ref() == Some(&source_fingerprint)
        {
            let _ = std::fs::remove_file(&staging);
            return Some(target.to_path_buf());
        }
        if std::fs::rename(&staging, &target).is_err() {
            let _ = std::fs::remove_file(&staging);
            return None;
        }
    }
    if cli_fingerprint(target).as_ref() != Some(&source_fingerprint) {
        let _ = std::fs::remove_file(target);
        let _ = std::fs::remove_file(&staging);
        return None;
    }
    Some(target.to_path_buf())
}

#[cfg(all(windows, test))]
fn cached_cli_matches_source(source: &std::path::Path, target: &std::path::Path) -> bool {
    cli_fingerprint(source)
        .zip(cli_fingerprint(target))
        .is_some_and(|(source_fingerprint, target_fingerprint)| {
            source_fingerprint == target_fingerprint
        })
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CliFingerprint {
    length: u64,
    sha256: [u8; 32],
}

#[cfg(windows)]
fn cli_fingerprint(path: &std::path::Path) -> Option<CliFingerprint> {
    let length = std::fs::metadata(path).ok()?.len();
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut hash = [0_u8; 32];
    hash.copy_from_slice(&digest);
    Some(CliFingerprint {
        length,
        sha256: hash,
    })
}

#[cfg(windows)]
fn first_where_candidate(command: &str) -> Option<String> {
    let output = std::process::Command::new("where")
        .arg(command)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    select_windows_codex_candidate(stdout.lines()).map(|path| path.to_string_lossy().to_string())
}

#[cfg(windows)]
fn select_windows_codex_candidate<'a>(
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<std::path::PathBuf> {
    candidates
        .into_iter()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .find(|path| path.is_file() && !path_is_windowsapps_resource(path))
}

fn path_is_windowsapps_resource(path: &std::path::Path) -> bool {
    let rendered = path
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    rendered.contains("\\windowsapps\\") && rendered.ends_with("\\app\\resources\\codex.exe")
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk-mirror-test-key-0123456789abcdef";
    const RELAY: &str = "wss://relay.example.club/relay";

    #[test]
    fn derives_stable_distinct_values() {
        let a = MobileRelayHostConfig::from_api_key(KEY, RELAY).unwrap();
        let b = MobileRelayHostConfig::from_api_key(KEY, RELAY).unwrap();
        assert_eq!(a.room_id, b.room_id);
        assert_eq!(a.relay_token, b.relay_token);
        assert_eq!(a.enc_key, b.enc_key);
        assert_ne!(a.room_id, a.relay_token);
        assert_eq!(a.room_id.len(), 32);
    }

    #[test]
    fn different_keys_land_in_different_rooms() {
        let a = MobileRelayHostConfig::from_api_key(KEY, RELAY).unwrap();
        let b = MobileRelayHostConfig::from_api_key("sk-other-key-9876543210", RELAY).unwrap();
        assert_ne!(a.room_id, b.room_id);
        assert_ne!(a.enc_key, b.enc_key);
    }

    #[test]
    fn rejects_blank_inputs() {
        assert!(MobileRelayHostConfig::from_api_key("   ", RELAY).is_err());
        assert!(MobileRelayHostConfig::from_api_key(KEY, " ").is_err());
    }

    #[test]
    fn host_url_carries_role_and_credentials() {
        let config = MobileRelayHostConfig::from_api_key(KEY, RELAY).unwrap();
        let url = config.host_url();
        assert!(url.starts_with("wss://relay.example.club/relay/ws?"));
        assert!(url.contains(&format!("room={}", config.room_id)));
        assert!(url.contains("role=host"));
    }

    #[test]
    fn host_url_does_not_double_ws_suffix() {
        let config =
            MobileRelayHostConfig::from_api_key(KEY, "wss://relay.example.club/relay/ws").unwrap();
        assert_eq!(config.host_url().matches("/ws").count(), 1);
    }

    #[test]
    fn mobile_url_uses_https_and_fragment() {
        let config = MobileRelayHostConfig::from_api_key(KEY, RELAY).unwrap();
        let url = config.mobile_url();
        assert!(url.starts_with("https://relay.example.club/relay/mobile#"));
        assert!(url.contains("#mx="));
        assert!(!url.contains("?key="));
        assert!(!url.contains(KEY));
    }

    #[test]
    fn pairing_bundle_holds_only_derived_credentials() {
        let config = MobileRelayHostConfig::from_api_key(KEY, RELAY).unwrap();
        let bundle = config.pairing_bundle();
        assert_eq!(bundle.version, 1);
        assert_eq!(bundle.room_id, config.room_id);
        assert_eq!(bundle.relay_token, config.relay_token);
        assert!(!bundle.enc_key.is_empty());
        assert!(!bundle.enc_key.contains(KEY));
    }

    #[cfg(windows)]
    #[test]
    fn windows_candidate_selection_prefers_non_windowsapps_entries() {
        let selected = select_windows_codex_candidate([
            r"C:\Program Files\WindowsApps\OpenAI.Codex_26.803.10989.0_x64__2p2nqsd0c76g0\app\resources\codex.exe",
            r"C:\Users\Administrator\.trae-cn\binaries\node\versions\22.17.1\codex.cmd",
        ])
        .unwrap();
        assert_eq!(
            selected,
            std::path::PathBuf::from(
                r"C:\Users\Administrator\.trae-cn\binaries\node\versions\22.17.1\codex.cmd"
            )
        );
    }

    #[cfg(windows)]
    #[test]
    fn private_cli_candidates_include_app_state_cache() {
        let candidates = private_windows_codex_cli_candidates();
        assert!(candidates.iter().any(|path| {
            path.ends_with(
                std::path::Path::new(".mirrorplus")
                    .join("runtime")
                    .join("codex-mobile-host.exe"),
            )
        }));
    }

    #[cfg(windows)]
    #[test]
    fn cached_cli_rejects_same_length_different_content() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.exe");
        let target = temp.path().join("target.exe");
        std::fs::write(&source, b"new-cli-a").unwrap();
        std::fs::write(&target, b"old-cli-b").unwrap();

        assert_eq!(
            std::fs::metadata(&source).unwrap().len(),
            std::fs::metadata(&target).unwrap().len()
        );
        assert!(!cached_cli_matches_source(&source, &target));
    }

    #[cfg(windows)]
    #[test]
    fn cached_cli_refresh_replaces_stale_same_length_binary_and_cleans_staging() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.exe");
        let target = temp.path().join("runtime").join("codex-mobile-host.exe");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&source, b"new-cli-a").unwrap();
        std::fs::write(&target, b"old-cli-b").unwrap();

        let refreshed = refresh_cached_codex_cli(&source, &target).unwrap();

        assert_eq!(refreshed, target);
        assert_eq!(std::fs::read(&target).unwrap(), b"new-cli-a");
        assert!(cached_cli_matches_source(&source, &target));
        assert!(
            std::fs::read_dir(target.parent().unwrap())
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| entry.path() == target)
        );
    }

    #[cfg(windows)]
    #[test]
    fn cached_cli_refresh_keeps_existing_target_when_source_copy_fails() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("missing-source.exe");
        let target = temp.path().join("runtime").join("codex-mobile-host.exe");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"existing-cli").unwrap();

        assert!(refresh_cached_codex_cli(&source, &target).is_none());
        assert_eq!(std::fs::read(&target).unwrap(), b"existing-cli");
        assert!(
            std::fs::read_dir(target.parent().unwrap())
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| entry.path() == target)
        );
    }

    #[test]
    fn encrypt_then_decrypt_roundtrips() {
        let config = MobileRelayHostConfig::from_api_key(KEY, RELAY).unwrap();
        let envelope = encrypt(&config.enc_key, b"hello mirror").unwrap();
        assert_eq!(envelope.envelope_type, "encrypted");
        let plaintext = decrypt(&config.enc_key, &envelope).unwrap();
        assert_eq!(plaintext, b"hello mirror");
    }

    #[test]
    fn decrypt_rejects_foreign_key() {
        let mine = MobileRelayHostConfig::from_api_key(KEY, RELAY).unwrap();
        let theirs = MobileRelayHostConfig::from_api_key("sk-someone-else-000", RELAY).unwrap();
        let envelope = encrypt(&mine.enc_key, b"secret").unwrap();
        assert!(decrypt(&theirs.enc_key, &envelope).is_err());
    }

    #[test]
    fn nonce_is_unique_per_message() {
        let config = MobileRelayHostConfig::from_api_key(KEY, RELAY).unwrap();
        let first = encrypt(&config.enc_key, b"same").unwrap();
        let second = encrypt(&config.enc_key, b"same").unwrap();
        assert_ne!(first.nonce, second.nonce);
        assert_ne!(first.payload, second.payload);
    }

    #[test]
    fn inbound_messages_parse_from_camel_case() {
        let connect: InboundMessage =
            serde_json::from_str(r#"{"type":"appServerConnect","sessionId":"s1"}"#).unwrap();
        assert!(matches!(connect, InboundMessage::Connect { session_id } if session_id == "s1"));

        let rpc: InboundMessage = serde_json::from_str(
            r#"{"type":"appServerMessage","sessionId":"s1","message":"{\"id\":1}"}"#,
        )
        .unwrap();
        assert!(matches!(rpc, InboundMessage::Rpc { .. }));

        let upload: InboundMessage = serde_json::from_str(
            r#"{"type":"fileUploadStart","sessionId":"s1","requestId":"r1","uploadId":"upload-12345678","fileName":"photo.png","mimeType":"image/png","size":5}"#,
        )
        .unwrap();
        assert!(matches!(
            upload,
            InboundMessage::FileUploadStart { size: 5, .. }
        ));

        let download: InboundMessage = serde_json::from_str(
            r#"{"type":"fileDownloadRequest","sessionId":"s1","requestId":"r2","path":"C:\\work\\demo.png","maxBytes":26214400}"#,
        )
        .unwrap();
        assert!(matches!(
            download,
            InboundMessage::FileDownloadRequest {
                max_bytes: MAX_DOWNLOAD_FILE_BYTES,
                ..
            }
        ));
    }

    #[test]
    fn outbound_messages_serialize_to_camel_case() {
        let json = serde_json::to_string(&OutboundMessage::Connected {
            session_id: "s1".to_string(),
            resumed: true,
            mode: "desktopSync".to_string(),
            capabilities: vec!["fileDownloadChunks"],
        })
        .unwrap();
        assert!(json.contains("\"type\":\"appServerConnected\""));
        assert!(json.contains("\"sessionId\":\"s1\""));
        assert!(json.contains("\"resumed\":true"));
        assert!(json.contains("\"mode\":\"desktopSync\""));
        assert!(json.contains("\"capabilities\":[\"fileDownloadChunks\"]"));
    }

    #[test]
    fn debug_output_hides_secrets() {
        let config = MobileRelayHostConfig::from_api_key(KEY, RELAY).unwrap();
        let rendered = format!("{config:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains(&config.relay_token));
    }

    #[test]
    fn desktop_sync_fallback_finishes_before_phone_open_timeout() {
        assert!(
            DESKTOP_SYNC_START_TIMEOUT < std::time::Duration::from_secs(30),
            "desktop sync must fall back before the deployed phone client times out"
        );
    }

    #[test]
    fn diagnostic_room_identifier_is_masked() {
        let config = MobileRelayHostConfig::from_api_key(KEY, RELAY).unwrap();
        let masked = masked_room_id(&config.room_id);
        assert_eq!(
            masked,
            format!("{}...{}", &config.room_id[..6], &config.room_id[28..])
        );
        assert!(!masked.contains(&config.room_id));
    }

    #[test]
    fn upload_file_name_strips_paths_and_windows_reserved_characters() {
        assert_eq!(sanitize_upload_file_name(r"..\..\坏:名?.png"), "坏名.png");
        assert_eq!(sanitize_upload_file_name("../../"), "attachment.bin");
    }

    #[tokio::test]
    async fn chunked_upload_writes_exact_bytes_to_session_temp_directory() {
        let uploads = SessionUploads::create(&format!("test-{}", uuid::Uuid::new_v4()))
            .await
            .unwrap();
        let upload_id = "upload-12345678";
        uploads
            .start(upload_id, r"..\photo.png", "image/png", 5)
            .await
            .unwrap();
        let received = uploads
            .append_chunk(upload_id, 0, &URL_SAFE_NO_PAD.encode(b"hello"))
            .await
            .unwrap();
        assert_eq!(received, 5);
        let path = uploads.finish(upload_id).await.unwrap();
        let canonical_root = tokio::fs::canonicalize(&uploads.root).await.unwrap();
        assert!(path.starts_with(canonical_root));
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"hello");
        uploads.cleanup().await;
    }

    #[tokio::test]
    async fn chunked_upload_rejects_out_of_order_chunks() {
        let uploads = SessionUploads::create(&format!("test-{}", uuid::Uuid::new_v4()))
            .await
            .unwrap();
        let upload_id = "upload-87654321";
        uploads
            .start(upload_id, "file.txt", "text/plain", 5)
            .await
            .unwrap();
        let error = uploads
            .append_chunk(upload_id, 1, &URL_SAFE_NO_PAD.encode(b"hello"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("分块顺序"));
        uploads.cancel(upload_id).await.unwrap();
        uploads.cleanup().await;
    }

    #[tokio::test]
    async fn chunked_download_keeps_every_encrypted_frame_below_relay_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large-preview.bin");
        let bytes = vec![0x5a; (2 * 1024 * 1024) + 17];
        tokio::fs::write(&path, &bytes).await.unwrap();
        let config = MobileRelayHostConfig::from_api_key(KEY, RELAY).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();

        send_file_download(
            &config.enc_key,
            &tx,
            "session-download",
            "request-download",
            path.to_str().unwrap(),
            MAX_DOWNLOAD_FILE_BYTES,
        )
        .await
        .unwrap();
        drop(tx);

        let mut frames = Vec::new();
        while let Some(Message::Text(text)) = rx.recv().await {
            assert!(
                text.len() < 2 * 1024 * 1024,
                "encrypted download frame exceeded Relay limit"
            );
            let envelope: EncryptedEnvelope = serde_json::from_str(&text).unwrap();
            let plaintext = decrypt(&config.enc_key, &envelope).unwrap();
            frames.push(serde_json::from_slice::<serde_json::Value>(&plaintext).unwrap());
        }
        assert_eq!(frames.first().unwrap()["phase"], "start");
        assert_eq!(frames.last().unwrap()["phase"], "finish");
        assert!(
            frames
                .iter()
                .filter(|frame| frame["phase"] == "chunk")
                .count()
                > 1
        );
        assert_eq!(
            frames.last().unwrap()["receivedBytes"],
            serde_json::json!(bytes.len())
        );
    }

    #[tokio::test]
    async fn chunked_download_rejects_oversized_file_before_sending_frames() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("too-large.bin");
        tokio::fs::write(&path, vec![0x31; 1025]).await.unwrap();
        let config = MobileRelayHostConfig::from_api_key(KEY, RELAY).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();

        let error = send_file_download(
            &config.enc_key,
            &tx,
            "session-download",
            "request-download",
            path.to_str().unwrap(),
            1024,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("文件超过"));
        assert!(
            rx.try_recv().is_err(),
            "oversized file must not leak a partial frame"
        );
    }

    #[test]
    fn windowsapps_resource_path_is_detected() {
        let path = std::path::Path::new(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_26.803.10989.0_x64__id\app\resources\codex.exe",
        );
        assert!(path_is_windowsapps_resource(path));
    }
}
