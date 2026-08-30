//! Bridge a mobile client into the App Server connection already owned by
//! Codex Desktop.
//!
//! This deliberately talks to the renderer through CDP instead of starting a
//! second `codex app-server`. The renderer remains the only desktop writer;
//! mobile requests and live notifications travel through the same host
//! connection, so an active desktop turn can be watched without forking it.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

const BINDING_NAME: &str = "mirrorXMobileDesktopSyncV1";
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

pub struct DesktopSyncRuntime {
    pub input_tx: mpsc::UnboundedSender<String>,
    alive: Arc<AtomicBool>,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl DesktopSyncRuntime {
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if tokio::time::timeout(std::time::Duration::from_secs(3), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
            let _ = self.task.await;
        }
    }
}

pub async fn start(
    session_id: &str,
) -> Result<(DesktopSyncRuntime, mpsc::UnboundedReceiver<String>)> {
    let websocket_url = find_codex_desktop_websocket().await?;
    let (mut socket, _) = tokio::time::timeout(CONNECT_TIMEOUT, connect_async(&websocket_url))
        .await
        .context("timed out connecting to Codex Desktop CDP")?
        .context("failed to connect to Codex Desktop CDP")?;

    let script = desktop_bridge_script(session_id)?;
    install_bridge(&mut socket, &script).await?;

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<String>();
    let (output_tx, output_rx) = mpsc::unbounded_channel::<String>();
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let alive = Arc::new(AtomicBool::new(true));
    let task_alive = Arc::clone(&alive);

    let task = tokio::spawn(async move {
        let mut command_id = 100_u64;
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                input = input_rx.recv() => {
                    let Some(input) = input else { break };
                    command_id = command_id.saturating_add(1);
                    let input_literal = match serde_json::to_string(&input) {
                        Ok(value) => value,
                        Err(_) => continue,
                    };
                    let expression = format!(
                        "window.__mirrorXMobileDesktopSync?.request({input_literal})"
                    );
                    let command = json!({
                        "id": command_id,
                        "method": "Runtime.evaluate",
                        "params": {
                            "expression": expression,
                            "awaitPromise": false,
                            "returnByValue": false,
                            "allowUnsafeEvalBlockedByCSP": true
                        }
                    });
                    if socket
                        .send(Message::Text(command.to_string().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                message = socket.next() => {
                    let Some(message) = message else { break };
                    let Ok(message) = message else { break };
                    let Message::Text(text) = message else { continue };
                    let Ok(value) = serde_json::from_str::<Value>(&text) else { continue };
                    if value.get("method").and_then(Value::as_str) != Some("Runtime.bindingCalled") {
                        continue;
                    }
                    if value
                        .get("params")
                        .and_then(|params| params.get("name"))
                        .and_then(Value::as_str)
                        != Some(BINDING_NAME)
                    {
                        continue;
                    }
                    let Some(payload) = value
                        .get("params")
                        .and_then(|params| params.get("payload"))
                        .and_then(Value::as_str)
                    else {
                        continue;
                    };
                    let Ok(event) = serde_json::from_str::<Value>(payload) else { continue };
                    if event.get("kind").and_then(Value::as_str) != Some("rpc") {
                        continue;
                    }
                    let Some(message) = event.get("message").and_then(Value::as_str) else {
                        continue;
                    };
                    if output_tx.send(message.to_string()).is_err() {
                        break;
                    }
                }
            }
        }
        task_alive.store(false, Ordering::Release);
    });

    Ok((
        DesktopSyncRuntime {
            input_tx,
            alive,
            shutdown: Some(shutdown_tx),
            task,
        },
        output_rx,
    ))
}

async fn find_codex_desktop_websocket() -> Result<String> {
    let mut ports = BTreeSet::new();
    if let Ok(Some(status)) = crate::status::StatusStore::default().load_latest()
        && let Some(port) = status.debug_port
    {
        ports.insert(port);
    }
    ports.extend([9229, 9222, 9333]);

    let mut errors = Vec::new();
    for port in ports {
        match crate::cdp::list_targets(port).await {
            Ok(targets) => match crate::cdp::pick_injectable_codex_page_target(&targets) {
                Ok(target) => {
                    if let Some(url) = target.web_socket_debugger_url {
                        return Ok(url);
                    }
                    errors.push(format!("{port}: Codex target has no websocket URL"));
                }
                Err(error) => errors.push(format!("{port}: {error}")),
            },
            Err(error) => errors.push(format!("{port}: {error}")),
        }
    }
    bail!(
        "Codex Desktop CDP target unavailable ({})",
        errors.join("; ")
    )
}

async fn install_bridge<S>(socket: &mut S, script: &str) -> Result<()>
where
    S: SinkExt<Message>
        + StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    send_command(socket, 1, "Runtime.enable", json!({})).await?;
    let _ = send_command(
        socket,
        2,
        "Runtime.removeBinding",
        json!({ "name": BINDING_NAME }),
    )
    .await;
    send_command(
        socket,
        3,
        "Runtime.addBinding",
        json!({ "name": BINDING_NAME }),
    )
    .await?;
    send_command(
        socket,
        4,
        "Page.addScriptToEvaluateOnNewDocument",
        json!({ "source": script }),
    )
    .await?;
    let result = send_command(
        socket,
        5,
        "Runtime.evaluate",
        json!({
            "expression": script,
            "awaitPromise": true,
            "returnByValue": true,
            "allowUnsafeEvalBlockedByCSP": true
        }),
    )
    .await?;
    if result.get("exceptionDetails").is_some() {
        bail!("Codex Desktop sync bridge raised an exception");
    }
    let installed = result
        .get("result")
        .and_then(|result| result.get("value"))
        .and_then(|value| value.get("installed"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !installed {
        let reason = result
            .get("result")
            .and_then(|result| result.get("value"))
            .and_then(|value| value.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("desktop dispatcher not found");
        bail!("Codex Desktop sync bridge unavailable: {reason}");
    }
    Ok(())
}

async fn send_command<S>(socket: &mut S, id: u64, method: &str, params: Value) -> Result<Value>
where
    S: SinkExt<Message>
        + StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    socket
        .send(Message::Text(
            json!({ "id": id, "method": method, "params": params })
                .to_string()
                .into(),
        ))
        .await
        .with_context(|| format!("failed to send CDP command {method}"))?;

    tokio::time::timeout(COMMAND_TIMEOUT, async {
        loop {
            let message = socket
                .next()
                .await
                .ok_or_else(|| anyhow!("CDP socket closed while waiting for {method}"))?
                .context("failed to read CDP command response")?;
            let Message::Text(text) = message else {
                continue;
            };
            let value: Value =
                serde_json::from_str(&text).context("invalid CDP command response")?;
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = value.get("error") {
                    bail!("CDP command {method} failed: {error}");
                }
                return Ok(value.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    })
    .await
    .with_context(|| format!("timed out waiting for CDP command {method}"))?
}

fn desktop_bridge_script(session_id: &str) -> Result<String> {
    let session_id = serde_json::to_string(session_id)?;
    Ok(format!(
        r#"
(async () => {{
  const bindingName = {binding_name:?};
  const sessionId = {session_id};
  const root = window.__codexRoot?._internalRoot;
  if (!root) return {{ installed: false, reason: "Codex React root unavailable" }};

  const bridge = window.__mirrorXMobileDesktopSync || {{
    version: 1,
    sequence: 0,
    pending: new Map(),
    dispatcher: null,
  }};
  window.__mirrorXMobileDesktopSync = bridge;
  bridge.sessionId = sessionId;
  bridge.emit = (payload) => {{
    try {{
      const binding = window[bindingName];
      if (typeof binding === "function") binding(JSON.stringify(payload));
    }} catch (_) {{}}
  }};
  bridge.emitRpc = (message) => bridge.emit({{
    kind: "rpc",
    message: JSON.stringify(message),
  }});

  const syntheticInitialize = (id) => bridge.emitRpc({{
    jsonrpc: "2.0",
    id,
    result: {{
      serverInfo: {{ name: "mirror-x-desktop-sync", version: "1" }},
      capabilities: {{ experimentalApi: true }},
    }},
  }});

  bridge.request = (raw) => {{
    let rpc;
    try {{ rpc = JSON.parse(raw); }} catch (_) {{ return false; }}
    const method = String(rpc?.method || "");
    if (method === "initialize" && rpc.id != null) {{
      syntheticInitialize(rpc.id);
      return true;
    }}
    if (method === "initialized") return true;
    const dispatcher = bridge.dispatcher;
    if (!dispatcher || typeof dispatcher.dispatchMessage !== "function") {{
      if (rpc.id != null) bridge.emitRpc({{
        jsonrpc: "2.0",
        id: rpc.id,
        error: {{ code: -32001, message: "Codex Desktop dispatcher is unavailable" }},
      }});
      return false;
    }}
    if (rpc.id == null) return true;
    const bridgeId = `mirror-mobile-${{sessionId}}-${{++bridge.sequence}}`;
    bridge.pending.set(bridgeId, rpc.id);
    const timeoutMs = method === "turn/start" ? 300000 : 45000;
    dispatcher.dispatchMessage("mcp-request", {{
      hostId: "local",
      request: {{ id: bridgeId, method, params: rpc.params || {{}} }},
      priority: method.startsWith("turn/") ? "interactive" : "background",
      source: "remote_control",
      timeoutMs,
      expiresAtMs: Date.now() + timeoutMs,
    }});
    return true;
  }};

  bridge.handleDesktopMessage = (type, payload) => {{
    if (type === "mcp-notification") {{
      const method = String(payload?.method || "");
      if (method) bridge.emitRpc({{
        jsonrpc: "2.0",
        method,
        params: payload?.params || {{}},
      }});
      return;
    }}
    if (type === "mcp-response") {{
      const response = payload?.message || payload?.response;
      const bridgeId = String(response?.id ?? "");
      if (!bridge.pending.has(bridgeId)) return;
      const originalId = bridge.pending.get(bridgeId);
      bridge.pending.delete(bridgeId);
      const rpc = {{ jsonrpc: "2.0", id: originalId }};
      if (response?.error != null) rpc.error = response.error;
      else rpc.result = response?.result;
      bridge.emitRpc(rpc);
      return;
    }}
    if (type === "codex-app-server-connection-changed") {{
      bridge.emitRpc({{
        jsonrpc: "2.0",
        method: "mirror/desktopConnectionChanged",
        params: {{
          state: payload?.state || "unknown",
          hostId: payload?.hostId || "local",
        }},
      }});
    }}
  }};

  const findDispatcher = () => {{
    const queue = [{{ value: root, depth: 0 }}];
    const seen = new WeakSet();
    let cursor = 0;
    let inspected = 0;
    while (cursor < queue.length && inspected < 100000) {{
      const {{ value, depth }} = queue[cursor++];
      if (!value || (typeof value !== "object" && typeof value !== "function") || seen.has(value)) continue;
      seen.add(value);
      inspected += 1;
      let dispatchMessage;
      let subscribe;
      let deliverMessage;
      try {{
        dispatchMessage = value.dispatchMessage;
        subscribe = value.subscribe;
        deliverMessage = value.deliverMessage;
      }} catch (_) {{}}
      if (typeof dispatchMessage === "function"
          && typeof subscribe === "function"
          && typeof deliverMessage === "function") {{
        return value;
      }}
      if (depth >= 35 || value === window || value === document
          || (typeof Element !== "undefined" && value instanceof Element)) continue;
      if (value instanceof Map) {{
        let count = 0;
        for (const [key, nested] of value) {{
          if (count++ >= 1000) break;
          queue.push({{ value: key, depth: depth + 1 }}, {{ value: nested, depth: depth + 1 }});
        }}
        continue;
      }}
      if (value instanceof Set) {{
        let count = 0;
        for (const nested of value) {{
          if (count++ >= 1000) break;
          queue.push({{ value: nested, depth: depth + 1 }});
        }}
        continue;
      }}
      let keys = [];
      try {{
        keys = [...Object.getOwnPropertyNames(value), ...Object.getOwnPropertySymbols(value)].slice(0, 700);
      }} catch (_) {{}}
      for (const key of keys) {{
        if (["ownerDocument", "parentElement", "parentNode", "children", "childNodes"].includes(String(key))) continue;
        let nested;
        try {{ nested = value[key]; }} catch (_) {{ continue; }}
        if (nested && (typeof nested === "object" || typeof nested === "function")) {{
          queue.push({{ value: nested, depth: depth + 1 }});
        }}
      }}
    }}
    return null;
  }};

  const dispatcher = findDispatcher();
  if (!dispatcher) return {{ installed: false, reason: "Codex dispatcher unavailable" }};
  bridge.dispatcher = dispatcher;
  if (!dispatcher.__mirrorXMobileOriginalDeliverMessage) {{
    const original = dispatcher.deliverMessage.bind(dispatcher);
    dispatcher.__mirrorXMobileOriginalDeliverMessage = original;
    dispatcher.deliverMessage = function mirrorXMobileDeliverMessage(type, payload) {{
      const result = original(type, payload);
      try {{
        window.__mirrorXMobileDesktopSync?.handleDesktopMessage(type, payload);
      }} catch (_) {{}}
      return result;
    }};
  }}
  return {{ installed: true, version: bridge.version }};
}})()
"#,
        binding_name = BINDING_NAME,
        session_id = session_id,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn receive_response(
        output_rx: &mut mpsc::UnboundedReceiver<String>,
        expected_id: u64,
    ) -> Value {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let message = output_rx.recv().await.expect("desktop sync output");
                let value: Value = serde_json::from_str(&message).expect("valid JSON-RPC");
                if value.get("id").and_then(Value::as_u64) == Some(expected_id) {
                    return value;
                }
            }
        })
        .await
        .expect("desktop sync response timeout")
    }

    #[test]
    fn bridge_script_contains_same_writer_protocol() {
        let script = desktop_bridge_script("mobile-test").unwrap();
        assert!(script.contains("\"mcp-request\""));
        assert!(script.contains("\"mcp-notification\""));
        assert!(script.contains("\"mcp-response\""));
        assert!(script.contains("\"turn/start\""));
        assert!(script.contains("remote_control"));
        assert!(script.contains("mirror/desktopConnectionChanged"));
    }

    #[test]
    fn bridge_script_escapes_session_id() {
        let script = desktop_bridge_script("mobile-\"quoted\"").unwrap();
        assert!(script.contains(r#"mobile-\"quoted\""#));
    }

    #[tokio::test]
    #[ignore = "requires a running Codex Desktop instance with CDP enabled"]
    async fn live_bridge_reads_threads_from_desktop_writer() {
        let (runtime, mut output_rx) = start("desktop-sync-live-test")
            .await
            .expect("connect to Codex Desktop");

        runtime
            .input_tx
            .send(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {}
                })
                .to_string(),
            )
            .expect("send initialize");
        let initialize = receive_response(&mut output_rx, 1).await;
        assert_eq!(
            initialize["result"]["serverInfo"]["name"],
            "mirror-x-desktop-sync"
        );

        runtime
            .input_tx
            .send(
                json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "thread/list",
                    "params": {
                        "limit": 5,
                        "sortKey": "updated_at",
                        "sortDirection": "desc",
                        "useStateDbOnly": true
                    }
                })
                .to_string(),
            )
            .expect("send thread/list");
        let threads = receive_response(&mut output_rx, 2).await;
        assert!(
            threads["result"]["data"]
                .as_array()
                .is_some_and(|data| !data.is_empty()),
            "Codex Desktop should return at least one local thread: {threads}"
        );

        runtime.stop().await;
    }
}
