// WebSocket transport plus JSON-RPC plumbing over the encrypted relay.

import { decryptJson, encryptJson } from "./crypto.js?v=20260815-3";

const RPC_TIMEOUT_MS = 30000;
const TURN_TIMEOUT_MS = 300000;
const FILE_UPLOAD_TIMEOUT_MS = 45000;
const FILE_DOWNLOAD_TIMEOUT_MS = 90000;
const FILE_CHUNK_BYTES = 256 * 1024;
const SESSION_STORAGE_PREFIX = "mirror-x-mobile-session:";

export function createSafeStorage(storage, onFallback = () => {}) {
  const memory = new Map();
  let fallbackReported = false;
  const reportFallback = (error) => {
    if (fallbackReported) return;
    fallbackReported = true;
    try { onFallback(error); } catch {}
  };
  return {
    getItem(key) {
      if (memory.has(key)) return memory.get(key);
      try {
        return storage?.getItem(key) ?? null;
      } catch (error) {
        reportFallback(error);
        return null;
      }
    },
    setItem(key, value) {
      const normalized = String(value);
      memory.set(key, normalized);
      try {
        storage?.setItem(key, normalized);
        if (!storage) reportFallback(new Error("persistent storage unavailable"));
      } catch (error) {
        reportFallback(error);
      }
    },
    removeItem(key) {
      memory.delete(key);
      try {
        storage?.removeItem(key);
        if (!storage) reportFallback(new Error("persistent storage unavailable"));
      } catch (error) {
        reportFallback(error);
      }
    },
  };
}

export const createSessionStorage = createSafeStorage;

let browserStorage = null;
try { browserStorage = globalThis.localStorage || null; } catch {}
const sessionStorage = createSafeStorage(browserStorage);

function newSessionId() {
  const random = globalThis.crypto?.randomUUID?.()
    || `${Date.now()}-${Math.random().toString(36).slice(2, 12)}`;
  return `mobile-${random}`;
}

export function sessionStorageKey(roomId) {
  return `${SESSION_STORAGE_PREFIX}${roomId}`;
}

export function restoreOrCreateSessionId(roomId, storage = sessionStorage) {
  const key = sessionStorageKey(roomId);
  const existing = storage?.getItem(key)?.trim();
  if (existing) return existing;
  const created = newSessionId();
  storage?.setItem(key, created);
  return created;
}

export function forgetSessionId(roomId, storage = sessionStorage) {
  storage.removeItem(sessionStorageKey(roomId));
}

export class RelayConnection {
  constructor({ roomId, relayToken, encKey, sessionStorage: sessionStore = sessionStorage }) {
    this.roomId = roomId;
    this.relayToken = relayToken;
    this.encKey = encKey;
    this.socket = null;
    this.backoffMs = 1000;
    this.closedByUser = false;
    this.handlers = new Map();
    this.sessionStorage = sessionStore;
    // The desktop app-server belongs to the pairing room, not to one page
    // lifetime. Keeping this identifier across reloads prevents a refresh,
    // lock-screen resume, or browser restart from killing the active task.
    this.sessionId = restoreOrCreateSessionId(roomId, sessionStore);
    this.reconnectTimer = null;
    this.terminalClose = false;
    this.connectionGeneration = 0;
  }

  on(type, handler) {
    if (!this.handlers.has(type)) this.handlers.set(type, new Set());
    this.handlers.get(type).add(handler);
    return () => this.handlers.get(type)?.delete(handler);
  }

  emit(type, payload) {
    for (const handler of this.handlers.get(type) || []) handler(payload);
  }

  wsUrl() {
    const scheme = location.protocol === "https:" ? "wss" : "ws";
    const params = new URLSearchParams({
      room: this.roomId,
      token: this.relayToken,
      role: "client",
    });
    return `${scheme}://${location.host}/relay/ws?${params.toString()}`;
  }

  connect() {
    this.closedByUser = false;
    this.terminalClose = false;
    const generation = ++this.connectionGeneration;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    const previousSocket = this.socket;
    this.socket = null;
    if (previousSocket && previousSocket.readyState !== WebSocket.CLOSED) {
      previousSocket.close();
    }
    this.emit("status", { state: "connecting" });
    const socket = new WebSocket(this.wsUrl());
    this.socket = socket;
    const isCurrent = () => (
      generation === this.connectionGeneration && this.socket === socket
    );

    socket.onopen = () => {
      if (!isCurrent()) return;
      this.emit("status", { state: "socketOpen" });
    };

    socket.onmessage = (event) => {
      if (!isCurrent()) return;
      this.handleRawMessage(event.data, isCurrent);
    };

    socket.onerror = () => {
      if (!isCurrent()) return;
      this.emit("status", { state: "error", message: "Network error" });
    };

    socket.onclose = () => {
      if (!isCurrent()) return;
      this.socket = null;
      if (this.closedByUser || this.terminalClose) return;
      this.emit("status", { state: "offline" });
      const reconnectTimer = setTimeout(() => {
        if (
          this.reconnectTimer !== reconnectTimer
          || generation !== this.connectionGeneration
          || this.closedByUser
          || this.terminalClose
        ) return;
        this.reconnectTimer = null;
        this.connect();
      }, this.backoffMs);
      this.reconnectTimer = reconnectTimer;
      this.backoffMs = Math.min(this.backoffMs * 2, 15000);
    };
  }

  disconnect({ forgetSession = false } = {}) {
    this.closedByUser = true;
    this.connectionGeneration += 1;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    const socket = this.socket;
    this.socket = null;
    if (socket) socket.close();
    if (forgetSession) forgetSessionId(this.roomId, this.sessionStorage);
  }

  async handleRawMessage(raw, isCurrent = () => true) {
    if (!isCurrent()) return;
    let parsed;
    try {
      parsed = JSON.parse(raw);
    } catch {
      return;
    }
    if (!isCurrent()) return;

    if (parsed.type === "registered") {
      this.backoffMs = 1000;
      this.emit("registered", parsed);
      return;
    }
    if (parsed.type === "error") {
      if (parsed.code === "CLIENT_REPLACED") {
        this.terminalClose = true;
      }
      this.emit("relayError", parsed);
      return;
    }
    if (parsed.type !== "encrypted") return;

    let message;
    try {
      message = await decryptJson(this.encKey, parsed);
    } catch {
      if (!isCurrent()) return;
      this.emit("relayError", {
        code: "DECRYPT_FAILED",
        message: "The phone and desktop are not using the same pairing data.",
      });
      return;
    }
    if (!isCurrent()) return;
    this.emit("message", message);
  }

  async send(payload) {
    if (!this.socket || this.socket.readyState !== WebSocket.OPEN) {
      throw new Error("Relay connection is not ready");
    }
    const envelope = await encryptJson(this.encKey, payload);
    this.socket.send(JSON.stringify(envelope));
  }
}

export class AppServerRpc {
  constructor(connection) {
    this.connection = connection;
    this.nextId = 1;
    this.pending = new Map();
    this.filePending = new Map();
    this.fileDownloadPending = new Map();
    this.notificationHandler = null;
    this.connected = false;
    this.initialized = false;
    this.mode = "standalone";
    this.capabilities = new Set();
    this.openWaiters = [];

    connection.on("message", (message) => this.handleRelayMessage(message));
    connection.on("status", ({ state, message }) => {
      if (state === "offline" || state === "appServerClosed") {
        this.connected = false;
        this.rejectAll(new Error(message || "连接已中断，正在恢复"));
      }
    });
  }

  onNotification(handler) {
    this.notificationHandler = handler;
  }

  handleRelayMessage(message) {
    if (message.sessionId && message.sessionId !== this.connection.sessionId) return;
    if (message.type === "fileDownloadResponse") {
      this.handleFileDownloadResponse(message);
      return;
    }
    if (message.type === "fileUploadResponse") {
      const waiter = this.filePending.get(String(message.requestId || ""));
      if (!waiter) return;
      this.filePending.delete(String(message.requestId));
      clearTimeout(waiter.timer);
      if (message.ok) waiter.resolve(message);
      else waiter.reject(new Error(message.error || "附件上传失败"));
      return;
    }
    if (message.type === "appServerConnected") {
      this.connected = true;
      this.mode = message.mode || "standalone";
      this.capabilities = new Set(Array.isArray(message.capabilities) ? message.capabilities : []);
      const result = {
        resumed: message.resumed === true,
        mode: this.mode,
      };
      for (const waiter of this.openWaiters.splice(0)) {
        clearTimeout(waiter.timer);
        waiter.resolve(result);
      }
      this.connection.emit("status", { state: "online" });
      return;
    }
    if (message.type === "appServerClosed") {
      this.connected = false;
      this.initialized = false;
      this.rejectAll(new Error(message.reason || "Codex app-server closed"));
      this.connection.emit("status", { state: "appServerClosed", message: message.reason });
      return;
    }
    if (message.type !== "appServerMessage") return;

    let rpc;
    try {
      rpc = JSON.parse(message.message);
    } catch {
      return;
    }

    if (rpc.id !== undefined && rpc.id !== null) {
      const waiter = this.pending.get(String(rpc.id));
      if (waiter) {
        this.pending.delete(String(rpc.id));
        clearTimeout(waiter.timer);
        if (rpc.error) {
          const error = new Error(rpc.error.message || "RPC failed");
          error.code = rpc.error.code;
          error.data = rpc.error.data;
          waiter.reject(error);
        } else {
          waiter.resolve(rpc.result);
        }
        return;
      }
    }

    if (rpc.method && this.notificationHandler) {
      this.notificationHandler(rpc);
    }
  }

  rejectAll(error) {
    for (const waiter of this.pending.values()) {
      clearTimeout(waiter.timer);
      waiter.reject(error);
    }
    this.pending.clear();
    for (const waiter of this.filePending.values()) {
      clearTimeout(waiter.timer);
      waiter.reject(error);
    }
    this.filePending.clear();
    for (const waiter of this.fileDownloadPending.values()) {
      clearTimeout(waiter.timer);
      waiter.reject(error);
    }
    this.fileDownloadPending.clear();
    for (const waiter of this.openWaiters.splice(0)) {
      clearTimeout(waiter.timer);
      waiter.reject(error);
    }
  }

  async fileRequest(payload, timeoutMs = FILE_UPLOAD_TIMEOUT_MS) {
    const requestId = `file-${globalThis.crypto?.randomUUID?.() || `${Date.now()}-${Math.random()}`}`;
    const promise = new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.filePending.delete(requestId);
        reject(new Error("附件上传等待电脑响应超时"));
      }, timeoutMs);
      this.filePending.set(requestId, { resolve, reject, timer });
    });
    try {
      await this.connection.send({
        ...payload,
        requestId,
        sessionId: this.connection.sessionId,
      });
    } catch (error) {
      const waiter = this.filePending.get(requestId);
      if (waiter) {
        clearTimeout(waiter.timer);
        this.filePending.delete(requestId);
      }
      throw error;
    }
    return promise;
  }

  async uploadFile(file, onProgress = () => {}) {
    const uploadId = `upload-${globalThis.crypto?.randomUUID?.() || `${Date.now()}-${Math.random()}`}`;
    await this.fileRequest({
      type: "fileUploadStart",
      uploadId,
      fileName: file.name,
      mimeType: file.type || "application/octet-stream",
      size: file.size,
    });
    let offset = 0;
    let index = 0;
    try {
      while (offset < file.size) {
        const bytes = new Uint8Array(await file.slice(offset, offset + FILE_CHUNK_BYTES).arrayBuffer());
        let binary = "";
        for (let cursor = 0; cursor < bytes.length; cursor += 0x8000) {
          binary += String.fromCharCode(...bytes.subarray(cursor, cursor + 0x8000));
        }
        const data = btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
        await this.fileRequest({
          type: "fileUploadChunk",
          uploadId,
          index,
          data,
        });
        offset += bytes.length;
        index += 1;
        onProgress(file.size ? offset / file.size : 1);
      }
      const completed = await this.fileRequest({
        type: "fileUploadFinish",
        uploadId,
      });
      onProgress(1);
      return {
        id: uploadId,
        name: file.name,
        mimeType: file.type || "application/octet-stream",
        size: file.size,
        path: completed.path,
      };
    } catch (error) {
      this.fileRequest({ type: "fileUploadCancel", uploadId }, 5000).catch(() => {});
      throw error;
    }
  }

  handleFileDownloadResponse(message) {
    const requestId = String(message.requestId || "");
    const waiter = this.fileDownloadPending.get(requestId);
    if (!waiter) return;
    const fail = (error) => {
      clearTimeout(waiter.timer);
      this.fileDownloadPending.delete(requestId);
      waiter.reject(error instanceof Error ? error : new Error(String(error)));
    };
    if (!message.ok || message.phase === "error") {
      fail(new Error(message.error || "文件读取失败"));
      return;
    }
    clearTimeout(waiter.timer);
    waiter.timer = setTimeout(
      () => fail(new Error("文件读取等待电脑响应超时")),
      FILE_DOWNLOAD_TIMEOUT_MS,
    );
    try {
      if (message.phase === "start") {
        const size = Number(message.size);
        if (!Number.isSafeInteger(size) || size < 0 || size > waiter.maxBytes) {
          throw new Error("电脑返回的文件大小无效");
        }
        waiter.expectedSize = size;
        return;
      }
      if (message.phase === "chunk") {
        if (Number(message.index) !== waiter.nextIndex) throw new Error("文件分块顺序错误");
        const encoded = String(message.data || "");
        const padded = encoded.replace(/-/g, "+").replace(/_/g, "/")
          + "=".repeat((4 - (encoded.length % 4)) % 4);
        const binary = atob(padded);
        const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));
        waiter.receivedBytes += bytes.length;
        if (waiter.receivedBytes > waiter.maxBytes) throw new Error("文件超过手机端预览上限");
        waiter.chunks.push(bytes);
        waiter.nextIndex += 1;
        waiter.onProgress(waiter.expectedSize ? waiter.receivedBytes / waiter.expectedSize : 0);
        return;
      }
      if (message.phase !== "finish") throw new Error("未知的文件传输状态");
      if (waiter.expectedSize === null) throw new Error("文件传输缺少大小信息");
      if (waiter.receivedBytes !== waiter.expectedSize) throw new Error("文件传输不完整，请重试");
      const bytes = new Uint8Array(waiter.receivedBytes);
      let offset = 0;
      for (const chunk of waiter.chunks) {
        bytes.set(chunk, offset);
        offset += chunk.length;
      }
      clearTimeout(waiter.timer);
      this.fileDownloadPending.delete(requestId);
      waiter.onProgress(1);
      waiter.resolve(bytes);
    } catch (error) {
      fail(error);
    }
  }

  async downloadFile(path, maxBytes, onProgress = () => {}) {
    if (!this.capabilities.has("fileDownloadChunks")) {
      throw new Error("电脑端版本过旧，请更新 Mirror X Codex 后再预览文件");
    }
    const requestId = `download-${globalThis.crypto?.randomUUID?.() || `${Date.now()}-${Math.random()}`}`;
    const promise = new Promise((resolve, reject) => {
      const waiter = {
        resolve,
        reject,
        timer: null,
        maxBytes,
        expectedSize: null,
        receivedBytes: 0,
        nextIndex: 0,
        chunks: [],
        onProgress,
      };
      waiter.timer = setTimeout(() => {
        this.fileDownloadPending.delete(requestId);
        reject(new Error("文件读取等待电脑响应超时"));
      }, FILE_DOWNLOAD_TIMEOUT_MS);
      this.fileDownloadPending.set(requestId, waiter);
    });
    try {
      await this.connection.send({
        type: "fileDownloadRequest",
        requestId,
        sessionId: this.connection.sessionId,
        path,
        maxBytes,
      });
    } catch (error) {
      const waiter = this.fileDownloadPending.get(requestId);
      if (waiter) {
        clearTimeout(waiter.timer);
        this.fileDownloadPending.delete(requestId);
      }
      throw error;
    }
    return promise;
  }

  async openSession() {
    this.connected = false;
    const connected = new Promise((resolve, reject) => {
      const waiter = { resolve, reject, timer: null };
      waiter.timer = setTimeout(() => {
        const index = this.openWaiters.indexOf(waiter);
        if (index >= 0) this.openWaiters.splice(index, 1);
        reject(new Error("Codex app-server connection timed out"));
      }, RPC_TIMEOUT_MS);
      this.openWaiters.push(waiter);
    });
    await this.connection.send({
      type: "appServerConnect",
      id: `open-${Date.now()}`,
      sessionId: this.connection.sessionId,
    });
    return connected;
  }

  async call(method, params, timeoutMs) {
    const id = this.nextId;
    this.nextId += 1;
    const rpc = { jsonrpc: "2.0", id, method, params: params ?? {} };

    const promise = new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(String(id));
        reject(new Error(`${method} timed out`));
      }, timeoutMs ?? (method === "turn/start" ? TURN_TIMEOUT_MS : RPC_TIMEOUT_MS));
      this.pending.set(String(id), { resolve, reject, timer });
    });

    await this.connection.send({
      type: "appServerMessage",
      sessionId: this.connection.sessionId,
      message: JSON.stringify(rpc),
    });
    return promise;
  }

  async notify(method, params) {
    await this.connection.send({
      type: "appServerMessage",
      sessionId: this.connection.sessionId,
      message: JSON.stringify({ jsonrpc: "2.0", method, params: params ?? {} }),
    });
  }

  async initialize() {
    const result = await this.call("initialize", {
      clientInfo: { name: "mirror-x-mobile", title: "Mirror X Mobile", version: "1.0.0" },
      capabilities: { experimentalApi: true },
    });
    await this.notify("initialized", {});
    this.initialized = true;
    return result;
  }
}
