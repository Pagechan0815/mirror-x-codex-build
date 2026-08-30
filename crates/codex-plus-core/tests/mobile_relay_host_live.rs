//! Live proxy test: relay + real host module + real `codex app-server`.
//!
//! Ignored by default because it needs a local Codex install and a built relay
//! binary. Run with:
//!   cargo test -p codex-plus-core --test mobile_relay_host_live -- --ignored --nocapture

use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use codex_plus_core::mobile_relay_host::{
    EncryptedEnvelope, MobileRelayHostConfig, MobileRelayHostStatus, decrypt, encrypt,
};

const API_KEY: &str = "sk-mirror-live-host-check-000111";
const PORT: u16 = 8792;

struct RelayProcess(Child);

impl Drop for RelayProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_relay(binary: &std::path::Path) -> RelayProcess {
    RelayProcess(
        Command::new(binary)
            .env("CODEX_PLUS_MOBILE_RELAY_BIND", format!("127.0.0.1:{PORT}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn relay"),
    )
}

fn relay_binary() -> Option<std::path::PathBuf> {
    let mut root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.pop();
    root.pop();
    for name in ["codex-plus-mobile-relay.exe", "codex-plus-mobile-relay"] {
        let candidate = root.join("target").join("debug").join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a local Codex install and a built relay binary"]
async fn host_proxies_initialize_and_thread_list() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let Some(binary) = relay_binary() else {
        eprintln!("relay binary missing; run cargo build -p codex-plus-mobile-relay");
        return;
    };

    let mut relay = Some(spawn_relay(&binary));
    tokio::time::sleep(Duration::from_millis(700)).await;

    let relay_url = format!("ws://127.0.0.1:{PORT}/relay");
    let config = MobileRelayHostConfig::from_api_key(API_KEY, &relay_url).expect("config");
    let enc_key = config.enc_key;
    let room = config.room_id.clone();
    let token = config.relay_token.clone();
    let statuses: Arc<Mutex<Vec<MobileRelayHostStatus>>> = Arc::new(Mutex::new(Vec::new()));
    let reporter_statuses = statuses.clone();
    let reporter = Arc::new(move |status: MobileRelayHostStatus| {
        eprintln!("host status: {:?}", status);
        reporter_statuses.lock().unwrap().push(status);
    });

    let host = codex_plus_core::mobile_relay_host::spawn_with_reporter(config, Some(reporter));
    tokio::time::sleep(Duration::from_millis(700)).await;

    let client_url =
        format!("ws://127.0.0.1:{PORT}/relay/ws?room={room}&token={token}&role=client");
    let (stream, _) = tokio_tungstenite::connect_async(client_url.clone())
        .await
        .expect("client connect");
    let (mut writer, mut reader) = stream.split();

    // First frame is the plaintext registration ack.
    let registered = reader.next().await.expect("frame").expect("message");
    assert!(registered.to_text().unwrap().contains("registered"));

    let send = |payload: serde_json::Value| {
        let envelope = encrypt(&enc_key, &serde_json::to_vec(&payload).unwrap()).unwrap();
        Message::Text(serde_json::to_string(&envelope).unwrap().into())
    };

    writer
        .send(send(serde_json::json!({
            "type": "appServerConnect",
            "sessionId": "live"
        })))
        .await
        .expect("send connect");

    writer
        .send(send(serde_json::json!({
            "type": "appServerMessage",
            "sessionId": "live",
            "message": serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientInfo": { "name": "mirror-x-live-test", "title": "Live", "version": "1.0.0" },
                    "capabilities": { "experimentalApi": true }
                }
            }).to_string()
        })))
        .await
        .expect("send initialize");

    let mut saw_connected = false;
    let mut saw_resumed = false;
    let mut saw_initialize_result = false;
    let mut sent_thread_list = false;
    let mut sent_reconnect = false;
    let mut saw_thread_list_result = false;
    let mut saw_post_reconnect_thread_list_result = false;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    while tokio::time::Instant::now() < deadline
        && !(saw_connected
            && saw_resumed
            && saw_initialize_result
            && saw_thread_list_result
            && saw_post_reconnect_thread_list_result)
    {
        let Ok(Some(Ok(frame))) =
            tokio::time::timeout(Duration::from_secs(10), reader.next()).await
        else {
            break;
        };
        let Ok(text) = frame.into_text() else {
            continue;
        };
        let Ok(envelope) = serde_json::from_str::<EncryptedEnvelope>(&text) else {
            continue;
        };
        let plaintext = decrypt(&enc_key, &envelope).expect("decrypt");
        let value: serde_json::Value = serde_json::from_slice(&plaintext).expect("json");
        match value["type"].as_str() {
            Some("appServerConnected") => {
                assert_eq!(value["mode"], "desktopSync");
                if value["resumed"] == true {
                    saw_resumed = true;
                } else {
                    saw_connected = true;
                }
            }
            Some("appServerMessage") => {
                let inner: serde_json::Value =
                    serde_json::from_str(value["message"].as_str().unwrap_or("{}")).unwrap();
                if inner["id"] == 1 && inner.get("result").is_some() {
                    saw_initialize_result = true;
                    if !sent_thread_list {
                        writer
                            .send(send(serde_json::json!({
                                "type": "appServerMessage",
                                "sessionId": "live",
                                "message": serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "method": "initialized",
                                    "params": {}
                                }).to_string()
                            })))
                            .await
                            .expect("send initialized");
                        writer
                            .send(send(serde_json::json!({
                                "type": "appServerMessage",
                                "sessionId": "live",
                                "message": serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": 2,
                                    "method": "thread/list",
                                    "params": {
                                        "limit": 1,
                                        "useStateDbOnly": true
                                    }
                                }).to_string()
                            })))
                            .await
                            .expect("send thread/list");
                        sent_thread_list = true;
                    }
                }
                if inner["id"] == 2 && inner.get("result").is_some() {
                    saw_thread_list_result = true;
                    if !sent_reconnect {
                        writer
                            .send(send(serde_json::json!({
                                "type": "appServerConnect",
                                "sessionId": "live"
                            })))
                            .await
                            .expect("send reconnect");
                        writer
                            .send(send(serde_json::json!({
                                "type": "appServerMessage",
                                "sessionId": "live",
                                "message": serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": 3,
                                    "method": "thread/list",
                                    "params": {
                                        "limit": 1,
                                        "useStateDbOnly": true
                                    }
                                }).to_string()
                            })))
                            .await
                            .expect("send post-reconnect thread/list");
                        sent_reconnect = true;
                    }
                }
                if inner["id"] == 3 && inner.get("result").is_some() {
                    saw_post_reconnect_thread_list_result = true;
                }
            }
            Some("appServerClosed") => {
                panic!("app-server closed early: {}", value["reason"]);
            }
            _ => {}
        }
    }

    eprintln!(
        "pre-stop: connected={} resumed={} initialize={} thread_list={} post_reconnect={}",
        saw_connected,
        saw_resumed,
        saw_initialize_result,
        saw_thread_list_result,
        saw_post_reconnect_thread_list_result
    );

    // Simulate the production failure mode: the desktop Host stays alive while
    // the relay process disappears and comes back. The same mobile session
    // must resume the existing app-server without another initialize call.
    drop(writer);
    drop(reader);
    drop(relay.take());
    tokio::time::sleep(Duration::from_secs(2)).await;
    relay = Some(spawn_relay(&binary));
    tokio::time::sleep(Duration::from_secs(3)).await;

    let reconnect_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let (stream, _) = loop {
        match tokio_tungstenite::connect_async(client_url.clone()).await {
            Ok(result) => break result,
            Err(error) if tokio::time::Instant::now() < reconnect_deadline => {
                eprintln!("waiting for relay/host reconnect: {error}");
                tokio::time::sleep(Duration::from_millis(800)).await;
            }
            Err(error) => panic!("client failed to reconnect after relay restart: {error}"),
        }
    };
    let (mut writer, mut reader) = stream.split();
    let registered = reader.next().await.expect("frame").expect("message");
    assert!(registered.to_text().unwrap().contains("registered"));
    writer
        .send(send(serde_json::json!({
            "type": "appServerConnect",
            "sessionId": "live"
        })))
        .await
        .expect("send relay-restart reconnect");

    let mut saw_relay_restart_resume = false;
    let mut saw_relay_restart_rpc = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline
        && !(saw_relay_restart_resume && saw_relay_restart_rpc)
    {
        let Ok(Some(Ok(frame))) =
            tokio::time::timeout(Duration::from_secs(10), reader.next()).await
        else {
            continue;
        };
        let Ok(text) = frame.into_text() else {
            continue;
        };
        let Ok(envelope) = serde_json::from_str::<EncryptedEnvelope>(&text) else {
            continue;
        };
        let plaintext = decrypt(&enc_key, &envelope).expect("decrypt after relay restart");
        let value: serde_json::Value = serde_json::from_slice(&plaintext).expect("json");
        match value["type"].as_str() {
            Some("appServerConnected") if value["resumed"] == true => {
                assert_eq!(value["mode"], "desktopSync");
                saw_relay_restart_resume = true;
                writer
                    .send(send(serde_json::json!({
                        "type": "appServerMessage",
                        "sessionId": "live",
                        "message": serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": 4,
                            "method": "thread/list",
                            "params": {
                                "limit": 1,
                                "useStateDbOnly": true
                            }
                        }).to_string()
                    })))
                    .await
                    .expect("send thread/list after relay restart");
            }
            Some("appServerMessage") => {
                let inner: serde_json::Value =
                    serde_json::from_str(value["message"].as_str().unwrap_or("{}")).unwrap();
                if inner["id"] == 4 && inner.get("result").is_some() {
                    saw_relay_restart_rpc = true;
                }
            }
            Some("appServerClosed") => {
                panic!(
                    "app-server closed across relay restart: {}",
                    value["reason"]
                );
            }
            _ => {}
        }
    }

    let stop_result = tokio::time::timeout(Duration::from_secs(15), host.stop()).await;
    let status_snapshot = statuses.lock().unwrap().clone();
    eprintln!("host statuses captured: {:?}", status_snapshot);
    assert!(
        stop_result.is_ok(),
        "host.stop() timed out; statuses: {:?}",
        status_snapshot
    );

    assert!(saw_connected, "host never acknowledged the session");
    assert!(saw_resumed, "host did not resume the existing session");
    assert!(
        saw_initialize_result,
        "app-server never answered initialize through the relay"
    );
    assert!(
        saw_thread_list_result,
        "app-server never answered thread/list through the relay"
    );
    assert!(
        saw_post_reconnect_thread_list_result,
        "app-server stopped answering after the same session reconnected"
    );
    assert!(
        saw_relay_restart_resume,
        "host did not preserve the app-server across a relay process restart"
    );
    assert!(
        saw_relay_restart_rpc,
        "preserved app-server stopped answering after relay process restart"
    );
    drop(relay);
}
