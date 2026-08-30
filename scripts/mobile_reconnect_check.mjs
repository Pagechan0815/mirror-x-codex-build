import assert from "node:assert/strict";

const storageData = new Map();
globalThis.localStorage = {
  getItem: (key) => storageData.get(key) ?? null,
  setItem: (key, value) => storageData.set(key, String(value)),
  removeItem: (key) => storageData.delete(key),
};
globalThis.location = {
  protocol: "https:",
  host: "relay.example.test",
};

const {
  AppServerRpc,
  createSessionStorage,
  RelayConnection,
  forgetSessionId,
} = await import("../apps/codex-plus-mobile-relay/pwa/relay.js");

const originalStorageDescriptor = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
for (const storageError of ["QuotaError", "SecurityError"]) {
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    get() { throw new DOMException("blocked", storageError); },
  });
  const isolatedRelay = await import(
    `../apps/codex-plus-mobile-relay/pwa/relay.js?storage-failure=${storageError}`
  );
  assert.doesNotThrow(
    () => new isolatedRelay.RelayConnection({
      roomId: `isolated-${storageError}`,
      relayToken: "token-fallback",
      encKey: new Uint8Array(32),
    }),
    `${storageError} from global localStorage must not break RelayConnection construction`,
  );
}
Object.defineProperty(globalThis, "localStorage", originalStorageDescriptor);

for (const storageError of ["QuotaError", "SecurityError"]) {
  const throwingStorage = {
    getItem() { throw new DOMException("blocked", storageError); },
    setItem() { throw new DOMException("blocked", storageError); },
    removeItem() { throw new DOMException("blocked", storageError); },
  };
  const fallbackStorage = createSessionStorage(throwingStorage);
  const fallbackKeys = {
    roomId: `room-${storageError}`,
    relayToken: "token-fallback",
    encKey: new Uint8Array(32),
    sessionStorage: fallbackStorage,
  };
  let fallbackConnection;
  assert.doesNotThrow(() => {
    fallbackConnection = new RelayConnection(fallbackKeys);
  });
  assert.equal(
    new RelayConnection(fallbackKeys).sessionId,
    fallbackConnection.sessionId,
    `${storageError} must fall back to an in-memory session without breaking connection construction`,
  );
}

const keys = { roomId: "room-a", relayToken: "token-a", encKey: new Uint8Array(32) };
const firstConnection = new RelayConnection(keys);
const reloadedConnection = new RelayConnection(keys);
const otherRoomConnection = new RelayConnection({ ...keys, roomId: "room-b" });
assert.equal(
  reloadedConnection.sessionId,
  firstConnection.sessionId,
  "same pairing room must preserve the app-server session across reloads",
);
assert.notEqual(
  otherRoomConnection.sessionId,
  firstConnection.sessionId,
  "different rooms must not share a session",
);
forgetSessionId(keys.roomId);
const afterForgetConnection = new RelayConnection(keys);
assert.notEqual(
  afterForgetConnection.sessionId,
  firstConnection.sessionId,
  "explicit disconnect must start a fresh session next time",
);

class FakeWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  static instances = [];

  constructor(url) {
    this.url = url;
    this.readyState = FakeWebSocket.CONNECTING;
    this.closeCalls = 0;
    FakeWebSocket.instances.push(this);
  }

  close() {
    this.closeCalls += 1;
    this.readyState = FakeWebSocket.CLOSING;
  }

  send() {}
}

globalThis.WebSocket = FakeWebSocket;
const guardedConnection = new RelayConnection({ ...keys, roomId: "room-socket-guard" });
const guardedStatuses = [];
guardedConnection.on("status", ({ state }) => guardedStatuses.push(state));
guardedConnection.connect();
const staleSocket = FakeWebSocket.instances.at(-1);
guardedConnection.connect();
const activeSocket = FakeWebSocket.instances.at(-1);
assert.notEqual(activeSocket, staleSocket);
assert.equal(staleSocket.closeCalls, 1, "replacing a connection should close the old socket");
let handledSocketMessages = 0;
guardedConnection.handleRawMessage = () => { handledSocketMessages += 1; };
staleSocket.onmessage({ data: "stale" });
staleSocket.onopen();
staleSocket.onclose();
assert.equal(
  guardedConnection.socket,
  activeSocket,
  "a delayed close from an old socket must not clear the active socket",
);
assert.equal(
  guardedConnection.reconnectTimer,
  null,
  "a delayed close from an old socket must not schedule another reconnect",
);
assert.equal(
  guardedStatuses.filter((state) => state === "socketOpen").length,
  0,
  "events from an old socket must be ignored",
);
assert.equal(handledSocketMessages, 0, "messages from an old socket must be ignored");
activeSocket.readyState = FakeWebSocket.OPEN;
activeSocket.onopen();
activeSocket.onmessage({ data: "active" });
assert.equal(guardedStatuses.at(-1), "socketOpen");
assert.equal(handledSocketMessages, 1, "messages from the active socket must be delivered");
activeSocket.readyState = FakeWebSocket.CLOSED;
activeSocket.onclose();
assert.equal(guardedConnection.socket, null);
assert.ok(guardedConnection.reconnectTimer, "the active socket should still schedule recovery");
guardedConnection.connect();
const recoveredSocket = FakeWebSocket.instances.at(-1);
assert.equal(guardedConnection.reconnectTimer, null, "manual recovery should cancel the old timer");
activeSocket.onclose();
assert.equal(guardedConnection.socket, recoveredSocket);
assert.equal(
  guardedConnection.reconnectTimer,
  null,
  "a repeated close callback must not create a duplicate reconnect timer",
);
guardedConnection.disconnect();

class FakeConnection {
  constructor() {
    this.sessionId = "mobile-reconnect-test";
    this.handlers = new Map();
    this.sent = [];
  }

  on(type, handler) {
    if (!this.handlers.has(type)) this.handlers.set(type, new Set());
    this.handlers.get(type).add(handler);
  }

  emit(type, payload) {
    for (const handler of this.handlers.get(type) || []) handler(payload);
  }

  async send(payload) {
    this.sent.push(payload);
  }
}

const connection = new FakeConnection();
const rpc = new AppServerRpc(connection);

const firstOpen = rpc.openSession();
assert.equal(connection.sent.at(-1).type, "appServerConnect");
connection.emit("message", {
  type: "appServerConnected",
  sessionId: connection.sessionId,
  resumed: false,
  mode: "desktopSync",
  capabilities: ["fileDownloadChunks"],
});
assert.deepEqual(await firstOpen, { resumed: false, mode: "desktopSync" });
assert.equal(rpc.mode, "desktopSync");
assert.equal(rpc.capabilities.has("fileDownloadChunks"), true);

rpc.initialized = true;
const resumedOpen = rpc.openSession();
connection.emit("message", {
  type: "appServerConnected",
  sessionId: connection.sessionId,
  resumed: true,
  mode: "desktopSync",
  capabilities: ["fileDownloadChunks"],
});
assert.deepEqual(await resumedOpen, { resumed: true, mode: "desktopSync" });
assert.equal(rpc.initialized, true);

const sourceBytes = Uint8Array.from({ length: 600_123 }, (_, index) => index % 251);
const download = rpc.downloadFile("D:\\project\\preview.mp4", 25 * 1024 * 1024);
const request = connection.sent.at(-1);
assert.equal(request.type, "fileDownloadRequest");
connection.emit("message", {
  type: "fileDownloadResponse",
  sessionId: connection.sessionId,
  requestId: request.requestId,
  ok: true,
  phase: "start",
  size: sourceBytes.length,
  receivedBytes: 0,
});
let receivedBytes = 0;
let chunkIndex = 0;
for (let offset = 0; offset < sourceBytes.length; offset += 256 * 1024) {
  const chunk = sourceBytes.subarray(offset, offset + (256 * 1024));
  let binary = "";
  for (let cursor = 0; cursor < chunk.length; cursor += 0x8000) {
    binary += String.fromCharCode(...chunk.subarray(cursor, cursor + 0x8000));
  }
  const data = btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
  receivedBytes += chunk.length;
  connection.emit("message", {
    type: "fileDownloadResponse",
    sessionId: connection.sessionId,
    requestId: request.requestId,
    ok: true,
    phase: "chunk",
    index: chunkIndex,
    data,
    receivedBytes,
  });
  chunkIndex += 1;
}
connection.emit("message", {
  type: "fileDownloadResponse",
  sessionId: connection.sessionId,
  requestId: request.requestId,
  ok: true,
  phase: "finish",
  size: sourceBytes.length,
  index: chunkIndex,
  receivedBytes,
});
assert.deepEqual(await download, sourceBytes);

const oldHostConnection = new FakeConnection();
const oldHostRpc = new AppServerRpc(oldHostConnection);
await assert.rejects(
  oldHostRpc.downloadFile("D:\\project\\preview.png", 25 * 1024 * 1024),
  /电脑端版本过旧/,
);

const malformedConnection = new FakeConnection();
const malformedRpc = new AppServerRpc(malformedConnection);
malformedConnection.emit("message", {
  type: "appServerConnected",
  sessionId: malformedConnection.sessionId,
  resumed: false,
  mode: "desktopSync",
  capabilities: ["fileDownloadChunks"],
});
const malformedDownload = malformedRpc.downloadFile("D:\\project\\broken.png", 1024);
const malformedRequest = malformedConnection.sent.at(-1);
malformedConnection.emit("message", {
  type: "fileDownloadResponse",
  sessionId: malformedConnection.sessionId,
  requestId: malformedRequest.requestId,
  ok: true,
  phase: "start",
  size: 2,
});
malformedConnection.emit("message", {
  type: "fileDownloadResponse",
  sessionId: malformedConnection.sessionId,
  requestId: malformedRequest.requestId,
  ok: true,
  phase: "chunk",
  index: 1,
  data: "YQ",
});
await assert.rejects(malformedDownload, /文件分块顺序错误/);

const rpcErrorConnection = new FakeConnection();
const rpcErrorClient = new AppServerRpc(rpcErrorConnection);
const steerFailure = rpcErrorClient.call("turn/steer", {
  threadId: "thread-active",
  expectedTurnId: "turn-active",
  input: [{ type: "text", text: "调整方向" }],
});
const steerRequest = JSON.parse(rpcErrorConnection.sent.at(-1).message);
rpcErrorConnection.emit("message", {
  type: "appServerMessage",
  sessionId: rpcErrorConnection.sessionId,
  message: JSON.stringify({
    jsonrpc: "2.0",
    id: steerRequest.id,
    error: {
      code: -32000,
      message: "active turn cannot accept same-turn steering",
      data: { codexErrorInfo: { activeTurnNotSteerable: { turnKind: "compact" } } },
    },
  }),
});
await assert.rejects(
  steerFailure,
  (error) => (
    error.code === -32000
    && error.data.codexErrorInfo.activeTurnNotSteerable.turnKind === "compact"
  ),
);

console.log("mobile reconnect transport check passed");
