import {
  AppServerRpc,
  createSafeStorage,
  RelayConnection,
} from "./relay.js?v=20260821-15";
import {
  b64urlEncode,
  decodePairingFragment,
  deriveKeys,
  restoreKeys,
} from "./crypto.js?v=20260815-6";

const STORAGE_KEY = "mirror-x-mobile-pairing";
const LEGACY_STORAGE_KEY = "mirror-x-mobile-config";
const HISTORY_PAGE_SIZE = 80;
const FULL_HISTORY_TIMEOUT_MS = 90000;
const TURN_PAGE_SIZE = 20;
const MAX_MESSAGE_CHARS = 12000;
const STREAM_RENDER_INTERVAL_MS = 90;
const MAX_TEXT_FILE_BYTES = 2 * 1024 * 1024;
const MAX_MEDIA_FILE_BYTES = 25 * 1024 * 1024;
const MAX_ATTACHMENT_FILES = 5;
const MAX_ATTACHMENT_BYTES = 25 * 1024 * 1024;
const MAX_ATTACHMENT_TOTAL_BYTES = 50 * 1024 * 1024;
const PENDING_SEND_PREFIX = "mirror-x-mobile-pending:";
const SELECTED_THREAD_PREFIX = "mirror-x-mobile-selected-thread:";
const DRAFT_PREFIX = "mirror-x-mobile-draft:";
const OVERLAY_HISTORY_KEY = "mirrorXOverlay";
const OVERLAY_HISTORY_SESSION = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
const FOCUSABLE_SELECTOR = [
  "button:not([disabled])",
  "a[href]",
  "input:not([disabled])",
  "textarea:not([disabled])",
  "select:not([disabled])",
  "[tabindex]:not([tabindex='-1'])",
].join(",");
export const MOBILE_FULL_ACCESS = Object.freeze({
  approvalPolicy: "never",
  sandbox: "dangerFullAccess",
  sandboxPolicy: Object.freeze({ type: "dangerFullAccess" }),
});

export { createSafeStorage };

export function shouldSubmitComposerKey(event) {
  return event?.key === "Enter"
    && !event.shiftKey
    && !event.isComposing
    && event.keyCode !== 229;
}

export function conversationRefreshScrollMode(followOutput) {
  return followOutput ? "bottom" : "retain";
}

export function buildTurnSteerParams({
  threadId,
  turnId,
  clientUserMessageId = null,
  input = [],
}) {
  if (!String(threadId || "").trim()) throw new Error("引导缺少会话 ID");
  if (!String(turnId || "").trim()) throw new Error("引导缺少当前任务 ID");
  return {
    threadId,
    expectedTurnId: turnId,
    clientUserMessageId,
    input,
  };
}

export function turnSteerFailureMessage(error) {
  const message = String(error?.message || error || "");
  let dataText = "";
  try {
    dataText = JSON.stringify(error?.data || {});
  } catch {
    dataText = "";
  }
  const detail = `${message} ${dataText}`.toLowerCase();
  if (
    detail.includes("activeturnnotsteerable")
    || detail.includes("active turn not steerable")
    || detail.includes("cannot accept same-turn steering")
    || detail.includes("manual /compact")
    || detail.includes("turnkind")
  ) {
    return "当前任务正处于压缩或审查阶段，暂时不能接受引导。请等待该阶段结束后再发送。";
  }
  if (
    Number(error?.code) === -32601
    || detail.includes("method not found")
    || detail.includes("unknown method")
  ) {
    return "当前电脑上的 Codex 版本不支持执行中引导，请更新 Codex，或先停止当前任务后再发送。";
  }
  return `引导当前任务失败：${message || "电脑端未接受本次引导"}`;
}

const state = {
  phase: "setup",
  hasEnteredWorkspace: false,
  threads: [],
  nextCursor: null,
  historyMode: "loading",
  selectedThreadId: null,
  selectedCwd: "",
  fileRoot: "",
  streamingNode: null,
  turnActive: false,
  activePanel: "history",
  threadOpenGeneration: 0,
  threadReady: false,
  threadResumePromise: null,
  threadTurns: [],
  threadTurnsCursor: null,
  loadingOlderTurns: false,
  mobileForkSourceId: null,
  drawerOpen: false,
  reconnecting: false,
  activityLog: [],
  activeItems: new Map(),
  lastCompletedTurnId: null,
  pendingThreadRefresh: false,
  threadRuntime: new Map(),
  historyRefreshGeneration: 0,
  connectionMode: "standalone",
  desktopActiveThreadId: null,
  autoOpeningThreadId: null,
  sendInFlight: false,
  interruptInFlight: false,
  bootstrapRetryCount: 0,
  bootstrapRetryTimer: null,
  initialPairingIssue: "",
  selectedAttachments: [],
  mobileDeviceLayout: false,
  maxVisualViewportHeight: 0,
  visualViewportWidth: 0,
  fileViewerRawText: "",
  fileViewerMode: "preview",
  fileViewerObjectUrl: "",
  fileViewerGeneration: 0,
  fileViewerPath: "",
  fileViewerName: "",
  fileTreeGeneration: 0,
  fileTreeLoadingRoot: "",
  fileTreeLoadedRoot: "",
  fileTreeLoadPromise: null,
  historySyncing: false,
  lastHistorySyncAt: 0,
  followOutput: true,
  newContentPending: false,
  completedWhileHidden: false,
};

let connection = null;
let rpc = null;
let typingEl = null;
let appServerRecovery = null;
let drawerFocusOrigin = null;
let fileViewerFocusOrigin = null;
let overlayHistoryClosing = "";
const el = (id) => document.getElementById(id);
let nativeStorage = null;
try { nativeStorage = globalThis.localStorage || null; } catch {}
const safeStorage = createSafeStorage(nativeStorage, (error) => {
  console.warn("persistent browser storage unavailable; using memory fallback", error);
  if (typeof document !== "undefined") {
    queueMicrotask(() => showToast(
      "浏览器无法永久保存配对与草稿，本次仍可继续；关闭页面后可能需要重新配对",
    ));
  }
});

function messagesNearBottom(threshold = 96) {
  const messages = el("messages");
  if (!messages || messages.hidden) return true;
  return messages.scrollHeight - messages.scrollTop - messages.clientHeight <= threshold;
}

function updateJumpLatestButton() {
  const button = el("jumpLatestBtn");
  if (!button) return;
  button.hidden = !state.newContentPending || state.followOutput;
}

function markNewContent() {
  if (state.followOutput || messagesNearBottom()) {
    state.followOutput = true;
    state.newContentPending = false;
    const messages = el("messages");
    if (messages && !messages.hidden) messages.scrollTop = messages.scrollHeight;
  } else {
    state.newContentPending = true;
  }
  updateJumpLatestButton();
}

function jumpToLatest() {
  const messages = el("messages");
  if (!messages || messages.hidden) return;
  state.followOutput = true;
  state.newContentPending = false;
  messages.scrollTop = messages.scrollHeight;
  updateJumpLatestButton();
}

function detectMobileDeviceLayout() {
  const screenWidth = Number(globalThis.screen?.width || 0);
  const screenHeight = Number(globalThis.screen?.height || 0);
  const shortSide = Math.min(screenWidth || Number.MAX_SAFE_INTEGER, screenHeight || Number.MAX_SAFE_INTEGER);
  // A Windows laptop may expose touch points while still using a precise mouse.
  // Pointer precision is a better layout signal than touch capability alone.
  const touchDevice = globalThis.matchMedia?.("(pointer: coarse)")?.matches === true;
  const mobileUserAgent = navigator.userAgentData?.mobile === true
    || /Android|iPhone|iPod|Mobile|HarmonyOS|OpenHarmony/i.test(navigator.userAgent || "");
  return mobileUserAgent || (touchDevice && shortSide <= 820);
}

function updateDeviceLayoutMode() {
  const mobile = detectMobileDeviceLayout();
  const changed = mobile !== state.mobileDeviceLayout;
  state.mobileDeviceLayout = mobile;
  document.documentElement.classList.toggle("mobile-device-layout", mobile);
  if (changed && mobile && state.drawerOpen) setDrawer(false);
  syncOverlayAccessibility();
  updateVisualViewportMetrics();
}

function isMobileLayout() {
  return state.mobileDeviceLayout || globalThis.matchMedia?.("(max-width: 900px)")?.matches === true;
}

function updateVisualViewportMetrics() {
  const viewport = window.visualViewport;
  const width = Math.min(viewport?.width || window.innerWidth, window.innerWidth);
  const height = Math.min(viewport?.height || window.innerHeight, window.innerHeight);
  const offsetTop = Math.max(0, viewport?.offsetTop || 0);
  if (!state.visualViewportWidth || Math.abs(state.visualViewportWidth - width) > 80) {
    state.maxVisualViewportHeight = height;
  }
  state.visualViewportWidth = width;
  state.maxVisualViewportHeight = Math.max(state.maxVisualViewportHeight, height);
  const composerFocused = document.activeElement === el("messageInput");
  const keyboardOpen = composerFocused
    && state.maxVisualViewportHeight - height > Math.min(100, state.maxVisualViewportHeight * 0.18);
  const scale = 1;
  document.documentElement.classList.remove("mobile-wide-viewport");
  document.documentElement.classList.toggle("keyboard-open", keyboardOpen);
  document.documentElement.style.setProperty("--mobile-ui-scale", String(scale));
  document.documentElement.style.setProperty("--mobile-layout-width", `${width / scale}px`);
  document.documentElement.style.setProperty("--app-height", `${height / scale}px`);
  document.documentElement.style.setProperty("--visual-top", `${offsetTop / scale}px`);
  if (composerFocused && state.followOutput) {
    requestAnimationFrame(() => {
      const messages = el("messages");
      if (messages && !messages.hidden) messages.scrollTop = messages.scrollHeight;
    });
  }
}

function hasComposerContent() {
  return Boolean(el("messageInput")?.value.trim()) || state.selectedAttachments.length > 0;
}

function runtimeFor(threadId, create = true) {
  if (!threadId) return null;
  let runtime = state.threadRuntime.get(threadId);
  if (!runtime && create) {
    runtime = {
      turnActive: false,
      turnId: null,
      streamingNode: null,
      streamingText: "",
      liveStreams: new Map(),
      liveProcessDetails: null,
      liveProcessBody: null,
      liveProcessCount: 0,
      activeItems: new Map(),
      activityLog: [],
      pendingRefresh: false,
      lastCompletedTurnId: null,
      pendingSubmission: null,
      threadReady: false,
      resumePromise: null,
      queuedRetryTimer: null,
      syncing: false,
      lastSyncAt: 0,
      lastActivityAt: 0,
      syncIssue: "",
      turnStartedAt: 0,
      currentActivityLabel: "",
      currentActivityDetail: "",
      draftText: "",
      draftAttachments: [],
      turns: [],
      turnsCursor: null,
      hasTurnSnapshot: false,
    };
    state.threadRuntime.set(threadId, runtime);
  }
  return runtime || null;
}

function selectedRuntime() {
  return runtimeFor(state.selectedThreadId);
}

function notificationThreadId(params = {}) {
  return params.threadId
    || params.turn?.threadId
    || params.item?.threadId
    || null;
}

function pendingSendKey(threadId) {
  return `${PENDING_SEND_PREFIX}${connection?.roomId || "unknown"}:${threadId}`;
}

function selectedThreadKey() {
  return `${SELECTED_THREAD_PREFIX}${connection?.roomId || "unknown"}`;
}

function draftKey(threadId) {
  return `${DRAFT_PREFIX}${connection?.roomId || "unknown"}:${threadId}`;
}

function readDraftText(threadId) {
  if (!threadId) return "";
  return safeStorage.getItem(draftKey(threadId)) || "";
}

function persistDraftText(threadId, text) {
  if (!threadId) return;
  const key = draftKey(threadId);
  if (text) safeStorage.setItem(key, text);
  else safeStorage.removeItem(key);
}

function saveComposerDraft(threadId = state.selectedThreadId) {
  if (!threadId || typeof document === "undefined") return;
  const runtime = runtimeFor(threadId);
  const text = el("messageInput")?.value || "";
  runtime.draftText = text;
  runtime.draftAttachments = [...state.selectedAttachments];
  persistDraftText(threadId, text);
}

function restoreComposerDraft(threadId) {
  if (!threadId || typeof document === "undefined") return;
  const runtime = runtimeFor(threadId);
  if (!runtime.draftText) runtime.draftText = readDraftText(threadId);
  const input = el("messageInput");
  input.value = runtime.draftText || "";
  input.style.height = "auto";
  input.style.height = `${Math.min(input.scrollHeight, 150)}px`;
  state.selectedAttachments = [...(runtime.draftAttachments || [])];
  renderAttachmentList();
}

function clearComposerDraft(threadId) {
  if (!threadId) return;
  const runtime = runtimeFor(threadId, false);
  if (runtime) {
    runtime.draftText = "";
    runtime.draftAttachments = [];
  }
  persistDraftText(threadId, "");
}

function readSelectedThreadId() {
  return safeStorage.getItem(selectedThreadKey()) || null;
}

function persistSelectedThreadId(threadId) {
  if (threadId) safeStorage.setItem(selectedThreadKey(), threadId);
}

function clearSelectedThreadId() {
  safeStorage.removeItem(selectedThreadKey());
}

function readPendingSubmission(threadId) {
  try {
    const raw = safeStorage.getItem(pendingSendKey(threadId));
    return raw ? JSON.parse(raw) : null;
  } catch {
    return null;
  }
}

function writePendingSubmission(submission) {
  safeStorage.setItem(pendingSendKey(submission.threadId), JSON.stringify(submission));
}

function clearPendingSubmission(threadId) {
  safeStorage.removeItem(pendingSendKey(threadId));
}

function newClientMessageId() {
  return `mobile-${globalThis.crypto?.randomUUID?.() || `${Date.now()}-${Math.random().toString(36).slice(2, 10)}`}`;
}

function syncSelectedRuntimeUi() {
  const runtime = selectedRuntime();
  state.turnActive = runtime?.turnActive === true;
  state.streamingNode = runtime?.streamingNode || null;
  state.activeItems = runtime?.activeItems || new Map();
  state.activityLog = runtime?.activityLog || [];
  state.lastCompletedTurnId = runtime?.lastCompletedTurnId || null;
  state.pendingThreadRefresh = runtime?.pendingRefresh === true;
  const transportUnavailable = !rpc?.connected || state.reconnecting;
  el("interruptBtn").hidden = !state.turnActive || transportUnavailable;
  const pending = runtime?.pendingSubmission;
  el("attachmentBtn").disabled = !state.selectedThreadId
    || transportUnavailable
    || state.sendInFlight
    || Boolean(pending);
  if (state.sendInFlight) {
    el("messageInput").disabled = true;
    el("sendBtn").disabled = true;
    el("messageInput").placeholder = "正在准备发送…";
  } else if (pending?.state === "queued") {
    el("messageInput").disabled = true;
    el("sendBtn").disabled = true;
    el("messageInput").placeholder = "正在把旧排队消息转换为当前任务引导…";
  } else if (
    pending?.state === "sending"
    || pending?.state === "steering"
    || pending?.state === "confirming"
  ) {
    el("messageInput").disabled = true;
    el("sendBtn").disabled = true;
    el("messageInput").placeholder = pending.state === "sending"
      ? "正在发送…"
      : pending.state === "steering"
        ? "正在引导当前任务…"
        : "正在确认上一条消息是否已发送…";
  } else {
    el("messageInput").disabled = !state.selectedThreadId;
    el("messageInput").placeholder = transportUnavailable
      ? "连接恢复后可继续发送"
      : "给 Codex 输入任务…";
    el("sendBtn").disabled = transportUnavailable
      || !hasComposerContent()
      || !state.selectedThreadId;
  }
  if (state.turnActive && !transportUnavailable) {
    showTyping();
  } else {
    hideTyping();
  }
  renderActivityDetails();
  updateSyncStatus();
  updateGlobalRuntimeState();
}

function setPhase(phase) {
  state.phase = phase;
  document.body.dataset.phase = phase;
  if (phase === "workspace") state.hasEnteredWorkspace = true;
  document.querySelectorAll("[data-screen]").forEach((node) => {
    node.classList.toggle("active", node.dataset.screen === phase);
  });
  el("menuBtn").hidden = phase !== "workspace";
}

function setStatus(text, tone = "") {
  el("statusText").textContent = text;
  el("statusText").dataset.tone = tone;
}

function setConnectionMode(mode) {
  state.connectionMode = mode === "desktopSync" ? "desktopSync" : "standalone";
  if (state.connectionMode === "desktopSync") {
    el("sidebarStatus").textContent = "正在实时同步电脑 Codex";
    updateSyncStatus();
    return;
  }
  el("sidebarStatus").textContent = "兼容模式，不同步电脑正在执行的任务";
  updateSyncStatus();
}

function overlayHistoryEntry() {
  const entry = history.state?.[OVERLAY_HISTORY_KEY];
  if (!entry || typeof entry !== "object") return null;
  if (entry.session !== OVERLAY_HISTORY_SESSION) return null;
  if (entry.kind === "drawer") return { kind: "drawer" };
  if (entry.kind === "file" && String(entry.path || "").trim()) {
    return {
      kind: "file",
      path: String(entry.path),
      name: String(entry.name || ""),
    };
  }
  return null;
}

function nextOverlayHistoryState(entry = null) {
  const current = history.state && typeof history.state === "object" ? history.state : {};
  const next = { ...current };
  delete next.mirrorDrawer;
  if (entry) next[OVERLAY_HISTORY_KEY] = entry;
  else delete next[OVERLAY_HISTORY_KEY];
  return Object.keys(next).length ? next : null;
}

function pushOverlayHistory(entry) {
  overlayHistoryClosing = "";
  const sessionEntry = { ...entry, session: OVERLAY_HISTORY_SESSION };
  const current = overlayHistoryEntry();
  if (current?.kind === "file" && entry.kind === "file") {
    history.replaceState(nextOverlayHistoryState(sessionEntry), "");
    return;
  }
  if (current?.kind === entry.kind) return;
  history.pushState(nextOverlayHistoryState(sessionEntry), "");
}

function clearOverlayHistoryState() {
  overlayHistoryClosing = "";
  if (history.state?.[OVERLAY_HISTORY_KEY] || history.state?.mirrorDrawer === true) {
    history.replaceState(nextOverlayHistoryState(), "");
  }
}

function elementCanReceiveFocus(node) {
  return Boolean(
    node?.isConnected
    && typeof node.focus === "function"
    && !node.closest?.("[hidden], [inert], [aria-hidden='true']")
    && node.getClientRects().length > 0
  );
}

function restoreOverlayFocus(node) {
  if (!elementCanReceiveFocus(node)) return false;
  node.focus({ preventScroll: true });
  return true;
}

function visibleFocusableElements(root) {
  return Array.from(root?.querySelectorAll?.(FOCUSABLE_SELECTOR) || []).filter((node) => (
    elementCanReceiveFocus(node)
    && node.getClientRects().length > 0
  ));
}

function focusDrawer() {
  const sidebar = el("sidebar");
  const candidates = [
    el("closeMenuBtn"),
    sidebar?.querySelector(".nav-tab.active"),
    visibleFocusableElements(sidebar)[0],
    sidebar,
  ];
  candidates.some(restoreOverlayFocus);
}

function focusFileViewer() {
  restoreOverlayFocus(el("closeFileBtn") || el("fileViewer"));
}

function syncOverlayAccessibility() {
  const sidebar = el("sidebar");
  const viewer = el("fileViewer");
  const conversation = document.querySelector(".conversation");
  const topbar = document.querySelector(".topbar");
  if (!sidebar || !viewer) return;
  const mobile = isMobileLayout();
  const fileOpen = !viewer.hidden;
  const drawerModal = mobile && state.drawerOpen && !fileOpen;
  const fileModal = mobile && fileOpen;

  sidebar.setAttribute("aria-hidden", String(mobile && (!state.drawerOpen || fileOpen)));
  if (drawerModal) {
    sidebar.setAttribute("role", "dialog");
    sidebar.setAttribute("aria-modal", "true");
  } else {
    sidebar.removeAttribute("role");
    sidebar.removeAttribute("aria-modal");
  }
  viewer.setAttribute("aria-hidden", String(!fileOpen));
  if (fileModal) viewer.setAttribute("aria-modal", "true");
  else viewer.removeAttribute("aria-modal");

  if (topbar) topbar.inert = drawerModal || fileModal;
  if (conversation) conversation.inert = drawerModal || fileOpen;
  sidebar.inert = mobile && (!state.drawerOpen || fileOpen);
}

function applyDrawerState(open, { focus = false, restoreFocus = true } = {}) {
  const workspace = document.querySelector(".workspace-screen");
  if (!workspace) return;
  state.drawerOpen = Boolean(open);
  workspace.classList.toggle("drawer-open", state.drawerOpen);
  el("menuBtn").setAttribute("aria-expanded", String(state.drawerOpen));
  el("menuBtn").setAttribute("aria-label", state.drawerOpen ? "关闭导航" : "打开导航");
  el("menuBtn").classList.toggle("is-open", state.drawerOpen);
  document.body.classList.toggle("drawer-active", state.drawerOpen);
  syncOverlayAccessibility();
  if (state.drawerOpen && focus && isMobileLayout()) focusDrawer();
  if (!state.drawerOpen && restoreFocus) restoreOverlayFocus(drawerFocusOrigin || el("menuBtn"));
}

function requestOverlayClose(kind) {
  if (overlayHistoryEntry()?.kind === kind) {
    if (overlayHistoryClosing === kind) return;
    overlayHistoryClosing = kind;
    history.back();
    return;
  }
  if (kind === "file") closeFileViewerVisual();
  else applyDrawerState(false);
}

function setDrawer(open, {
  pushHistory = true,
  rememberFocus = true,
  focus = true,
  restoreFocus = true,
} = {}) {
  const nextOpen = Boolean(open);
  if (nextOpen) {
    if (!state.drawerOpen && rememberFocus) drawerFocusOrigin = document.activeElement;
    if (pushHistory) pushOverlayHistory({ kind: "drawer" });
    applyDrawerState(true, { focus, restoreFocus: false });
    return;
  }
  if (pushHistory && state.drawerOpen) {
    requestOverlayClose("drawer");
    return;
  }
  applyDrawerState(false, { restoreFocus });
}

function activeModalRoot() {
  if (!isMobileLayout()) return null;
  if (!el("fileViewer")?.hidden) return el("fileViewer");
  if (state.drawerOpen) return el("sidebar");
  return null;
}

function trapOverlayFocus(event) {
  if (event.key !== "Tab") return;
  const root = activeModalRoot();
  if (!root) return;
  const focusable = visibleFocusableElements(root);
  if (!focusable.length) {
    event.preventDefault();
    root.focus({ preventScroll: true });
    return;
  }
  const first = focusable[0];
  const last = focusable.at(-1);
  if (event.shiftKey && (document.activeElement === first || !root.contains(document.activeElement))) {
    event.preventDefault();
    last.focus({ preventScroll: true });
  } else if (!event.shiftKey && (document.activeElement === last || !root.contains(document.activeElement))) {
    event.preventDefault();
    first.focus({ preventScroll: true });
  }
}

async function applyOverlayHistoryState() {
  overlayHistoryClosing = "";
  const entry = overlayHistoryEntry();
  const fileWasOpen = !el("fileViewer").hidden;
  const drawerWasOpen = state.drawerOpen;
  if (entry?.kind === "file") {
    applyDrawerState(false, { restoreFocus: false });
    if (!fileWasOpen || state.fileViewerPath !== entry.path) {
      await openFile(entry.path, entry.name, {
        pushHistory: false,
        rememberFocus: false,
        focus: true,
      });
    } else {
      syncOverlayAccessibility();
      focusFileViewer();
    }
    return;
  }
  if (entry?.kind === "drawer") {
    const returnTarget = fileWasOpen ? fileViewerFocusOrigin : null;
    if (fileWasOpen) closeFileViewerVisual({ restoreFocus: false });
    setDrawer(true, {
      pushHistory: false,
      rememberFocus: false,
      focus: false,
      restoreFocus: false,
    });
    if (!restoreOverlayFocus(returnTarget)) focusDrawer();
    return;
  }
  const returnTarget = fileWasOpen
    ? fileViewerFocusOrigin
    : drawerWasOpen ? drawerFocusOrigin : null;
  if (fileWasOpen) closeFileViewerVisual({ restoreFocus: false });
  applyDrawerState(false, { restoreFocus: false });
  restoreOverlayFocus(returnTarget);
}

function showToast(text) {
  const node = document.createElement("div");
  node.className = "toast";
  node.textContent = text;
  el("toastArea").appendChild(node);
  setTimeout(() => node.remove(), 3200);
}

function showToastOnce(key, text, windowMs = 5000) {
  showToastOnce.lastShown ||= new Map();
  const now = Date.now();
  if (now - (showToastOnce.lastShown.get(key) || 0) < windowMs) return;
  showToastOnce.lastShown.set(key, now);
  showToast(text);
}

function showSetupError(message = "") {
  const node = el("setupError");
  if (!node) return;
  node.textContent = message;
  node.hidden = !message;
}

function friendlyBootstrapError(error) {
  const message = String(error?.message || error || "");
  if (
    message.includes("安全加密能力")
    || message.includes("SubtleCrypto")
    || message.includes("crypto.subtle")
  ) {
    return "当前浏览器安全能力不足。请使用手机系统浏览器扫描电脑端二维码打开。";
  }
  if (message.includes("Network error") || message.includes("Failed to construct")) {
    return "当前网络无法连接手机中继，请切换 Wi-Fi/移动网络后重试。";
  }
  return message || "启动失败，请重新扫描电脑端二维码。";
}

function setConnecting(title, detail) {
  el("connectingTitle").textContent = title || "正在连接…";
  el("connectingDetail").textContent = detail || "建立端对端加密通道";
}

function showInitialHostOfflineState() {
  state.initialPairingIssue = "hostOffline";
  setConnecting(
    "未找到匹配的电脑",
    "请核对 Key，或先在电脑端开启手机远程；本页会继续重试",
  );
}

function esc(value) {
  return String(value ?? "").replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
}

function projectLabel(cwd) {
  const text = String(cwd || "").replace(/^\\\\\?\\/, "").trim();
  const parts = text.split(/[\\/]+/).filter(Boolean);
  const label = parts.at(-1) || "未关联目录";
  return /^codex\s*\+{2}$/i.test(label) || /^codexplusplus$/i.test(label)
    ? "Mirror X Codex"
    : label;
}

function threadTitle(thread) {
  return String(thread.name || thread.preview || "未命名会话").trim().slice(0, 120);
}

function threadTimestamp(thread) {
  const raw = thread.recencyAt ?? thread.updatedAt ?? thread.createdAt;
  if (typeof raw === "number") return raw < 100000000000 ? raw * 1000 : raw;
  const parsed = Date.parse(raw || "");
  return Number.isFinite(parsed) ? parsed : 0;
}

function formatTime(thread) {
  const time = threadTimestamp(thread);
  if (!time) return "";
  const diff = Date.now() - time;
  if (diff < 60000) return "刚刚";
  if (diff < 3600000) return `${Math.max(1, Math.floor(diff / 60000))} 分钟前`;
  if (diff < 86400000) return `${Math.floor(diff / 3600000)} 小时前`;
  const date = new Date(time);
  return date.toLocaleDateString("zh-CN", { month: "numeric", day: "numeric" });
}

function historyBucket(thread) {
  const time = threadTimestamp(thread);
  const today = new Date();
  const dayStart = new Date(today.getFullYear(), today.getMonth(), today.getDate()).getTime();
  if (time >= dayStart) return "今天";
  if (time >= dayStart - 86400000) return "昨天";
  if (time >= dayStart - 7 * 86400000) return "最近 7 天";
  return "更早";
}

function normalizeCwd(cwd) {
  return String(cwd || "").replace(/^\\\\\?\\/, "");
}

function mergeThreads(incoming, replace = false) {
  const merged = new Map(replace ? [] : state.threads.map((thread) => [thread.id, thread]));
  for (const thread of incoming || []) {
    if (!thread?.id) continue;
    merged.set(thread.id, { ...merged.get(thread.id), ...thread, cwd: normalizeCwd(thread.cwd) });
  }
  state.threads = Array.from(merged.values()).sort((a, b) => threadTimestamp(b) - threadTimestamp(a));
}

function normalizedStatusType(value) {
  const raw = typeof value === "string" ? value : value?.type;
  return String(raw || "").replaceAll("_", "").replaceAll("-", "").toLowerCase();
}

function threadIsActive(thread) {
  return ["active", "inprogress", "running"].includes(normalizedStatusType(thread?.status));
}

function turnIsActive(turn) {
  return ["active", "inprogress", "running"].includes(normalizedStatusType(turn?.status));
}

function threadRuntimeIsActive(thread) {
  return threadIsActive(thread) || runtimeFor(thread?.id, false)?.turnActive === true;
}

function activeThreadIds() {
  const ids = new Set(state.threads.filter(threadRuntimeIsActive).map((thread) => thread.id));
  for (const [threadId, runtime] of state.threadRuntime.entries()) {
    if (runtime.turnActive) ids.add(threadId);
  }
  return ids;
}

function updateRuntimeConnectivityPresentation(transportUnavailable) {
  document.body.dataset.runtimeConnectivity = transportUnavailable ? "stale" : "live";
  for (const node of document.querySelectorAll('[data-runtime-active="true"]')) {
    if (node.matches(".thread-item, .project-card")) {
      node.classList.toggle("running", !transportUnavailable);
    }
    if (node.matches("details.turn-process")) {
      node.classList.toggle("active", !transportUnavailable);
    }
  }
  for (const label of document.querySelectorAll('[data-runtime-label="thread"]')) {
    label.textContent = transportUnavailable ? "断线前执行中" : "执行中";
    label.closest(".thread-running-label")?.classList.toggle("stale", transportUnavailable);
  }
  for (const label of document.querySelectorAll('[data-runtime-label="project"]')) {
    const count = Number(label.dataset.count || 0);
    label.textContent = transportUnavailable
      ? `断线前 ${count} 个任务执行中`
      : `${count} 个任务正在运行`;
    label.closest(".project-running-label")?.classList.toggle("stale", transportUnavailable);
  }
  for (const label of document.querySelectorAll('[data-runtime-label="turn"]')) {
    label.textContent = transportUnavailable ? "断线前执行中" : "正在执行";
    label.classList.toggle("stale", transportUnavailable);
  }
  for (const item of document.querySelectorAll('.thread-item[data-runtime-active="true"]')) {
    const marker = item.querySelector(".thread-mark");
    if (!marker) continue;
    if (transportUnavailable && marker.querySelector(".thread-running-spinner")) {
      marker.textContent = "◯";
    } else if (!transportUnavailable && !marker.querySelector(".thread-running-spinner")) {
      marker.innerHTML = '<i class="thread-running-spinner" aria-hidden="true"></i>';
    }
  }
}

function updateGlobalRuntimeState() {
  const count = activeThreadIds().size;
  const transportUnavailable = !rpc?.connected || state.reconnecting;
  const visibleCount = transportUnavailable ? 0 : count;
  const label = `${count} 个任务执行中`;
  const topRuntime = el("topRuntime");
  const sidebarRuntime = el("sidebarRuntime");
  const historyBadge = el("historyRunBadge");
  const projectBadge = el("projectRunBadge");
  if (topRuntime) {
    topRuntime.hidden = visibleCount === 0;
    el("topRuntimeText").textContent = count > 1 ? `${count} 个执行中` : "执行中";
  }
  if (sidebarRuntime) {
    sidebarRuntime.hidden = visibleCount === 0;
    el("sidebarRuntimeTitle").textContent = count > 1 ? "多个任务正在工作" : "Codex 正在工作";
    el("sidebarRuntimeText").textContent = label;
  }
  for (const badge of [historyBadge, projectBadge]) {
    if (!badge) continue;
    badge.hidden = visibleCount === 0;
    badge.textContent = String(Math.min(count, 99));
  }
  el("menuBtn")?.classList.toggle("has-running-task", visibleCount > 0);
  updateRuntimeConnectivityPresentation(transportUnavailable);
}

function relativeSyncText(time) {
  if (!time) return "等待同步";
  const seconds = Math.max(0, Math.floor((Date.now() - time) / 1000));
  if (seconds < 8) return "刚刚同步";
  if (seconds < 60) return `${seconds} 秒前同步`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} 分钟前同步`;
  return "点击刷新";
}

function updateSyncStatus() {
  const button = el("syncStatusBtn");
  if (!button) return;
  const runtime = selectedRuntime();
  button.hidden = !state.selectedThreadId;
  button.className = "sync-status";
  if (!state.selectedThreadId) return;
  let text = relativeSyncText(runtime?.lastSyncAt);
  if (state.reconnecting || !rpc?.connected) {
    text = "连接恢复中";
    button.classList.add("warn", "syncing");
  } else if (runtime?.syncing) {
    text = "正在同步";
    button.classList.add("syncing");
  } else if (runtime?.turnActive) {
    text = Date.now() - (runtime.lastActivityAt || 0) < 20000 ? "实时接收中" : "等待电脑更新";
    button.classList.add("live");
  } else if (runtime?.syncIssue) {
    text = "同步可能延迟";
    button.classList.add("warn");
  } else if (runtime?.lastSyncAt) {
    button.classList.add("ok");
  }
  el("syncStatusText").textContent = text;
  button.title = runtime?.syncIssue
    ? `${runtime.syncIssue}。点击重新同步`
    : "点击刷新当前会话";
}

function renderHistorySkeleton() {
  el("threadList").innerHTML = `
    <div class="side-skeleton" aria-label="正在读取历史会话">
      ${Array.from({ length: 6 }, (_, index) => `
        <div class="side-skeleton-row" style="--delay:${index * 70}ms">
          <i></i><span><b></b><small></small></span>
        </div>`).join("")}
    </div>`;
}

function renderProjectSkeleton() {
  el("projectList").innerHTML = `
    <div class="project-skeleton" aria-label="正在整理项目">
      ${Array.from({ length: 3 }, (_, index) => `
        <div style="--delay:${index * 90}ms"><b></b><span></span><small></small></div>`).join("")}
    </div>`;
}

function renderConversationSkeleton() {
  el("messages").innerHTML = `
    <div class="conversation-skeleton" aria-label="正在同步会话内容">
      <div class="conversation-skeleton-status"><i></i><span>正在同步本机会话</span></div>
      <div class="skeleton-message agent"><b></b><span></span><span></span><span></span></div>
      <div class="skeleton-message user"><b></b><span></span><span></span></div>
      <div class="skeleton-message agent short"><b></b><span></span><span></span></div>
    </div>`;
}

function newestActiveThread() {
  return state.threads
    .filter(threadIsActive)
    .sort((a, b) => threadTimestamp(b) - threadTimestamp(a))[0] || null;
}

function markThreadRuntimeState(threadId, active) {
  if (!threadId) return;
  const thread = state.threads.find((item) => item.id === threadId);
  mergeThreads([{
    ...(thread || {}),
    id: threadId,
    status: { type: active ? "active" : "idle" },
    recencyAt: Date.now(),
  }]);
  if (active) state.desktopActiveThreadId = threadId;
  else if (state.desktopActiveThreadId === threadId) state.desktopActiveThreadId = null;
  renderHistory();
  renderProjects();
  updateGlobalRuntimeState();
}

function autoOpenDesktopThread(threadId) {
  if (!threadId || state.selectedThreadId || state.autoOpeningThreadId === threadId) return;
  const thread = state.threads.find((item) => item.id === threadId) || {
    id: threadId,
    name: "电脑当前任务",
    status: { type: "active" },
    recencyAt: Date.now(),
  };
  mergeThreads([thread]);
  state.autoOpeningThreadId = threadId;
  openThread(thread).catch((error) => {
    console.warn("desktop active thread auto-open failed", error);
  }).finally(() => {
    if (state.autoOpeningThreadId === threadId) state.autoOpeningThreadId = null;
  });
}

function renderHistory() {
  const host = el("threadList");
  const query = el("threadSearch").value.trim().toLowerCase();
  const visible = state.threads.filter((thread) => {
    const haystack = `${threadTitle(thread)} ${thread.cwd || ""}`.toLowerCase();
    return !query || haystack.includes(query);
  });
  host.innerHTML = "";
  if (!visible.length) {
    host.innerHTML = '<p class="side-empty">没有找到会话。</p>';
    updateGlobalRuntimeState();
    return;
  }
  let lastBucket = "";
  for (const thread of visible) {
    const active = threadRuntimeIsActive(thread);
    const liveActive = active && rpc?.connected && !state.reconnecting;
    const bucket = historyBucket(thread);
    if (bucket !== lastBucket) {
      const title = document.createElement("div");
      title.className = "history-group-title";
      title.textContent = bucket;
      host.appendChild(title);
      lastBucket = bucket;
    }
    const button = document.createElement("button");
    button.type = "button";
    button.className = `thread-item ${thread.id === state.selectedThreadId ? "active" : ""} ${liveActive ? "running" : ""}`;
    button.dataset.runtimeActive = active ? "true" : "false";
    button.innerHTML = `
      <span class="thread-mark">${liveActive ? '<i class="thread-running-spinner" aria-hidden="true"></i>' : "◯"}</span>
      <span class="thread-copy">
        <strong>${esc(threadTitle(thread))}</strong>
        <span>${active ? `<em class="thread-running-label ${liveActive ? "" : "stale"}"><i></i><span data-runtime-label="thread">${liveActive ? "执行中" : "断线前执行中"}</span></em> · ` : ""}${esc(projectLabel(thread.cwd))} · ${esc(formatTime(thread))}</span>
      </span>`;
    button.onclick = () => {
      openThread(thread);
      closeDrawer();
    };
    host.appendChild(button);
  }
  updateGlobalRuntimeState();
}

function groupByProject() {
  const groups = new Map();
  for (const thread of state.threads) {
    const cwd = normalizeCwd(thread.cwd);
    const key = cwd.toLowerCase() || "__unknown__";
    if (!groups.has(key)) groups.set(key, { cwd, threads: [] });
    groups.get(key).threads.push(thread);
  }
  return Array.from(groups.values()).sort((a, b) => threadTimestamp(b.threads[0]) - threadTimestamp(a.threads[0]));
}

function renderProjects() {
  const host = el("projectList");
  const groups = groupByProject();
  host.innerHTML = "";
  if (!groups.length) {
    host.innerHTML = '<p class="side-empty">暂未发现项目。</p>';
    updateGlobalRuntimeState();
    return;
  }
  for (const group of groups) {
    const activeCount = group.threads.filter(threadRuntimeIsActive).length;
    const liveActive = activeCount > 0 && rpc?.connected && !state.reconnecting;
    const card = document.createElement("button");
    card.type = "button";
    card.className = `project-card ${group.cwd === state.fileRoot ? "active" : ""} ${liveActive ? "running" : ""}`;
    card.dataset.runtimeActive = activeCount ? "true" : "false";
    card.innerHTML = `<strong>${esc(projectLabel(group.cwd))}</strong>
      <span>${esc(group.cwd || "未知路径")}</span>
      <small>${activeCount ? `<em class="project-running-label ${liveActive ? "" : "stale"}"><i></i><span data-runtime-label="project" data-count="${activeCount}">${liveActive ? `${activeCount} 个任务正在运行` : `断线前 ${activeCount} 个任务执行中`}</span></em><b> · </b>` : ""}${group.threads.length} 个会话</small>`;
    card.onclick = () => {
      selectProject(group.cwd);
      setActivePanel("files");
    };
    host.appendChild(card);
  }
  updateGlobalRuntimeState();
}

function setHistoryNotice(text, warning = false, syncing = false) {
  el("historyNotice").textContent = text;
  el("historyNotice").classList.toggle("warn", warning);
  el("historyNotice").classList.toggle("syncing", syncing);
  state.historySyncing = syncing;
  el("refreshBtn").disabled = syncing;
  el("refreshBtn").classList.toggle("spinning", syncing);
}

async function loadThreadPage(params, timeoutMs) {
  return rpc.call("thread/list", {
    limit: HISTORY_PAGE_SIZE,
    sortKey: "updated_at",
    sortDirection: "desc",
    ...params,
  }, timeoutMs);
}

async function refreshThreads() {
  const generation = ++state.historyRefreshGeneration;
  const hadUsableThreads = state.threads.length > 0;
  if (!hadUsableThreads) {
    renderHistorySkeleton();
    renderProjectSkeleton();
  }
  setHistoryNotice("正在快速读取本机会话…", false, true);
  let fastError = null;
  let fastSucceeded = false;
  try {
    const fast = await loadThreadPage({ useStateDbOnly: true }, 30000);
    if (generation !== state.historyRefreshGeneration) return;
    mergeThreads(fast?.data || [], true);
    state.nextCursor = fast?.nextCursor || null;
    state.historyMode = "fast";
    renderHistory();
    renderProjects();
    el("loadMoreBtn").hidden = !state.nextCursor;
    setHistoryNotice(`已显示 ${state.threads.length} 个会话，正在补全磁盘历史…`, false, true);
    fastSucceeded = true;
  } catch (error) {
    fastError = error;
    if (generation !== state.historyRefreshGeneration) return;
    setHistoryNotice("快速索引暂不可用，正在直接读取完整历史…", true, true);
    console.warn("fast thread history scan failed", error);
  }

  const finishCompleteHistory = async () => {
    try {
      const complete = await loadThreadPage({}, FULL_HISTORY_TIMEOUT_MS);
      if (generation !== state.historyRefreshGeneration) return;
      mergeThreads(complete?.data || []);
      state.nextCursor = complete?.nextCursor || state.nextCursor;
      state.historyMode = "complete";
      state.lastHistorySyncAt = Date.now();
      renderHistory();
      renderProjects();
      el("loadMoreBtn").hidden = !state.nextCursor;
      setHistoryNotice(`刚刚同步 · ${state.threads.length} 个会话`);
    } catch (error) {
      if (generation !== state.historyRefreshGeneration) return;
      state.historyMode = "fallback";
      if (state.threads.length || hadUsableThreads) {
        setHistoryNotice(`历史补全暂时失败，已保留 ${state.threads.length} 个可用会话`, true);
        console.warn("full thread history scan failed", error);
        return;
      }
      console.warn("full thread history scan failed", error);
      const detail = [fastError?.message, error?.message].filter(Boolean).join("；");
      throw new Error(detail || "无法读取本机会话");
    } finally {
      if (generation === state.historyRefreshGeneration && state.historySyncing) {
        setHistoryNotice(
          state.threads.length
            ? `已保留 ${state.threads.length} 个会话 · 点击刷新重试`
            : "会话同步未完成 · 点击刷新重试",
          true,
        );
      }
    }
  };

  if (fastSucceeded) {
    setTimeout(() => {
      finishCompleteHistory().catch((error) => {
        console.warn("background full thread history scan failed", error);
      });
    }, 250);
    return;
  }
  await finishCompleteHistory();
}

async function loadMoreThreads() {
  if (!state.nextCursor) return;
  const button = el("loadMoreBtn");
  button.disabled = true;
  button.textContent = "加载中…";
  try {
    const params = { cursor: state.nextCursor };
    if (state.historyMode !== "complete") params.useStateDbOnly = true;
    const result = await loadThreadPage(params, FULL_HISTORY_TIMEOUT_MS);
    mergeThreads(result?.data || []);
    state.nextCursor = result?.nextCursor || null;
    renderHistory();
    renderProjects();
    button.hidden = !state.nextCursor;
  } catch (error) {
    showToast(`加载更多失败：${error.message}`);
  } finally {
    button.disabled = false;
    button.textContent = "加载更多历史";
  }
}

function itemRole(item) {
  if (item.type === "userMessage") return "user";
  if (item.type === "agentMessage") return "agent";
  if (item.type === "plan" || item.type === "reasoning") return "progress";
  if (["commandExecution", "fileChange", "mcpToolCall", "dynamicToolCall", "collabAgentToolCall", "webSearch"].includes(item.type)) return "tool";
  return "other";
}

function userContentText(content) {
  return (content || []).map((part) => {
    if (part?.text || part?.value) return part.text || part.value;
    if (["image", "localImage", "input_image"].includes(part?.type)) {
      const path = part.path || part.filePath || part.url || "";
      const name = part.name || fileNameFromPath(path) || "图片附件";
      return path ? `![${name}](<${path}>)` : "[图片附件]";
    }
    if (part?.type === "mention") {
      const path = part.path || part.filePath || "";
      const name = part.name || fileNameFromPath(path) || "文件";
      return path ? `[${name}](<${path}>)` : `[附件：${name}]`;
    }
    return "";
  }).filter(Boolean).join("\n");
}

function decodeControlText(value) {
  return String(value || "")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, "\"")
    .replace(/&#39;|&apos;/g, "'")
    .replace(/&amp;/g, "&");
}

function controlField(body, field, { partial = false } = {}) {
  const opening = new RegExp(`<${field}(?:\\s[^>]*)?>`, "i").exec(body);
  if (!opening) return null;
  const rest = body.slice(opening.index + opening[0].length);
  const closing = new RegExp(`</${field}\\s*>`, "i").exec(rest);
  if (!closing && !partial) return null;
  const value = closing ? rest.slice(0, closing.index) : rest;
  return decodeControlText(value.replace(/<\/?[a-z_][^>]*$/i, "")).trim();
}

export function presentConversationText(value, role = "other", { partial = false } = {}) {
  const source = String(value || "").replace(/^\uFEFF/, "");
  const trimmed = source.trim();
  if (!trimmed) return "";
  const root = /^<(heartbeat|codex_delegation)(?:\s[^>]*)?>/i.exec(trimmed);
  if (!root) return source;
  const rootName = root[1].toLowerCase();
  const body = trimmed.slice(root[0].length).replace(
    new RegExp(`</${rootName}\\s*>\\s*$`, "i"),
    "",
  );
  if (rootName === "heartbeat") {
    const looksInternal = partial
      || /<(?:automation_id|current_time_iso|decision|instructions|message)(?:\s|>)/i.test(body);
    if (!looksInternal) return source;
    const preferred = role === "user" ? ["instructions", "message"] : ["message", "instructions"];
    for (const field of preferred) {
      const extracted = controlField(body, field, { partial });
      if (extracted !== null) return extracted;
    }
    return "";
  }
  if (rootName === "codex_delegation") {
    const looksInternal = partial || /<(?:source_thread_id|input)(?:\s|>)/i.test(body);
    if (!looksInternal) return source;
    const extracted = controlField(body, "input", { partial });
    return extracted === null ? "" : extracted;
  }
  return source;
}

export function normalizeMessageSyntax(value) {
  return String(value || "")
    .replace(/\[\s*[!！]\s*image\s*\]/gi, "[图片附件]")
    .replace(/\[\s*[!！]\s*图片\s*\]/g, "[图片附件]");
}

function compactText(value, limit = 80) {
  const text = String(value || "").replace(/\s+/g, " ").trim();
  return text.length > limit ? `${text.slice(0, limit)}…` : text;
}

export function collabAgentSummary(item = {}) {
  const name = compactText(
    item.agentName
      || item.taskName
      || item.target
      || item.receiver
      || item.agent?.name
      || item.agent?.id
      || item.prompt
      || item.input,
    64,
  );
  const rawStatus = String(item.status?.type || item.status || item.state || "").toLowerCase();
  const status = rawStatus.includes("fail") || rawStatus.includes("error")
    ? "失败"
    : rawStatus.includes("complete") || rawStatus.includes("done")
      ? "已完成"
      : rawStatus.includes("wait")
        ? "等待中"
        : "执行中";
  return name ? `子 Agent ${status}：${name}` : `子 Agent ${status}`;
}

function fileChangeSummary(item = {}) {
  const changes = item.changes || [];
  if (!changes.length) return "文件变更";
  const paths = changes
    .map((change) => change.path || change.filePath || change.file || change.name)
    .filter(Boolean)
    .slice(0, 3)
    .map((path) => fileNameFromPath(path) || path);
  return paths.length
    ? `文件变更：${paths.join("、")}${changes.length > paths.length ? ` 等 ${changes.length} 项` : ""}`
    : `文件变更：${changes.length} 项`;
}

function itemText(item) {
  let text = "";
  if (typeof item.text === "string" && item.text) text = item.text;
  else if (item.type === "userMessage") text = userContentText(item.content);
  else if (item.type === "plan") text = item.text || "";
  else if (item.type === "reasoning") {
    const summary = Array.isArray(item.summary) ? item.summary.filter(Boolean).join("\n\n") : "";
    const content = Array.isArray(item.content) ? item.content.filter(Boolean).join("\n\n") : "";
    text = summary || content;
  }
  else if (item.type === "commandExecution") text = `${item.command || "命令"}${item.aggregatedOutput ? `\n${item.aggregatedOutput}` : ""}`;
  if (item.type === "fileChange") return fileChangeSummary(item);
  if (item.type === "mcpToolCall") return `MCP：${item.server || ""}/${item.tool || ""}`;
  if (item.type === "dynamicToolCall") return `工具：${item.namespace ? `${item.namespace}/` : ""}${item.tool || ""}`;
  if (item.type === "collabAgentToolCall") return collabAgentSummary(item);
  if (item.type === "webSearch") return `网页搜索：${item.query || ""}`;
  if (item.type === "contextCompaction") return "上下文已压缩";
  return presentConversationText(text, itemRole(item));
}

export function itemPresentationKind(item = {}, turnStatus = "") {
  if (item.type === "userMessage") return "user";
  if (item.type === "agentMessage") {
    if (item.phase === "final_answer") return "final";
    if (item.phase === "commentary") return "process";
    return normalizedStatusType(turnStatus) === "inprogress" ? "process" : "agent";
  }
  if ([
    "reasoning",
    "plan",
    "commandExecution",
    "fileChange",
    "mcpToolCall",
    "dynamicToolCall",
    "collabAgentToolCall",
    "webSearch",
    "contextCompaction",
    "subAgentActivity",
    "imageView",
    "imageGeneration",
  ].includes(item.type)) return "process";
  return "other";
}

export function turnTimelineSegments(turn = {}) {
  const items = Array.isArray(turn.items) ? turn.items : [];
  const status = turn.status;
  const explicitFinal = items.some((item) => (
    item.type === "agentMessage" && item.phase === "final_answer"
  ));
  let legacyFinalIndex = -1;
  if (!explicitFinal && !turnIsActive(turn)) {
    for (let index = items.length - 1; index >= 0; index -= 1) {
      if (items[index]?.type === "agentMessage" && !items[index]?.phase) {
        legacyFinalIndex = index;
        break;
      }
    }
  }
  const segments = [];
  let processItems = [];
  const flushProcess = () => {
    if (!processItems.length) return;
    segments.push({ kind: "process", items: processItems });
    processItems = [];
  };
  items.forEach((item, index) => {
    let kind = itemPresentationKind(item, status);
    if (index === legacyFinalIndex) kind = "final";
    if (kind === "process" || kind === "other" || kind === "agent") {
      processItems.push(item);
      return;
    }
    flushProcess();
    segments.push({ kind, items: [item] });
  });
  flushProcess();
  return segments;
}

function activityDetail(item = {}) {
  if (item.type === "commandExecution") return item.command || "命令";
  if (item.type === "fileChange") return fileChangeSummary(item);
  if (item.type === "mcpToolCall") return [item.server, item.tool].filter(Boolean).join("/");
  if (item.type === "dynamicToolCall") return [item.namespace, item.tool].filter(Boolean).join("/");
  if (item.type === "collabAgentToolCall") return collabAgentSummary(item);
  if (item.type === "webSearch") return item.query || "";
  return "";
}

const ROLE_LABEL = {
  user: "你",
  agent: "Codex",
  progress: "执行过程",
  tool: "执行记录",
  other: "系统",
};

function safeLinkUrl(value, image = false) {
  const source = String(value || "").trim();
  if (!source) return "";
  if (/^https?:\/\//i.test(source)) return source;
  if (image && /^data:image\/(?:png|jpe?g|gif|webp|svg\+xml);base64,/i.test(source)) return source;
  return "";
}

function decodeFileReference(value) {
  let source = String(value || "").trim();
  if (source.startsWith("<") && source.endsWith(">")) source = source.slice(1, -1).trim();
  try {
    source = decodeURIComponent(source);
  } catch {
    // Keep the original path when it contains a literal percent sign.
  }
  return source;
}

export function resolveLocalFilePath(value, base = "") {
  let source = decodeFileReference(value);
  if (!source || source.startsWith("#")) return "";
  if (/^file:\/\//i.test(source)) {
    try {
      source = decodeURIComponent(new URL(source).pathname);
      if (/^\/[a-z]:\//i.test(source)) source = source.slice(1);
    } catch {
      return "";
    }
  }
  if (/^[a-z]:[\\/]/i.test(source) || /^\\\\/.test(source) || /^\//.test(source)) {
    return source;
  }
  if (/^[a-z][a-z0-9+.-]*:/i.test(source)) return "";
  return base ? joinPath(base, source) : "";
}

function fileNameFromPath(path) {
  return decodeFileReference(path).split(/[\\/]/).filter(Boolean).pop() || "";
}

function fileExtension(path) {
  const name = fileNameFromPath(path).toLowerCase();
  const index = name.lastIndexOf(".");
  return index >= 0 ? name.slice(index + 1) : "";
}

export function filePreviewKind(path) {
  const ext = fileExtension(path);
  if (["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "avif"].includes(ext)) return "image";
  if (["md", "markdown", "mdx"].includes(ext)) return "markdown";
  if (["mp4", "webm", "mov", "m4v", "ogg"].includes(ext)) return "video";
  if (ext === "pdf") return "pdf";
  if ([
    "txt", "log", "json", "jsonl", "toml", "yaml", "yml", "xml", "csv",
    "js", "mjs", "cjs", "ts", "tsx", "jsx", "css", "scss", "html", "htm",
    "rs", "py", "go", "java", "kt", "swift", "c", "cc", "cpp", "h", "hpp",
    "sh", "bash", "zsh", "ps1", "bat", "cmd", "sql", "ini", "conf",
  ].includes(ext)) return "text";
  return "binary";
}

function fileMimeType(path, kind = filePreviewKind(path)) {
  const ext = fileExtension(path);
  const exact = {
    png: "image/png",
    jpg: "image/jpeg",
    jpeg: "image/jpeg",
    gif: "image/gif",
    webp: "image/webp",
    svg: "image/svg+xml",
    bmp: "image/bmp",
    avif: "image/avif",
    mp4: "video/mp4",
    webm: "video/webm",
    mov: "video/quicktime",
    m4v: "video/x-m4v",
    ogg: "video/ogg",
    pdf: "application/pdf",
  };
  if (exact[ext]) return exact[ext];
  if (kind === "markdown" || kind === "text") return "text/plain;charset=utf-8";
  return "application/octet-stream";
}

function fileBasePath() {
  return state.selectedCwd || state.fileRoot || "";
}

async function readPreviewFile(path, maxBytes) {
  if (!rpc?.connected) throw new Error("电脑连接尚未恢复");
  const bytes = await rpc.downloadFile(path, maxBytes);
  if (!(bytes instanceof Uint8Array)) throw new Error("电脑返回的文件内容无效");
  return { bytes, estimatedBytes: bytes.byteLength };
}

function createLocalAttachmentButton(path, label) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "message-attachment message-attachment-button";
  button.textContent = label || fileNameFromPath(path) || "查看附件";
  button.title = "点击预览本机文件";
  button.onclick = () => openFile(path, fileNameFromPath(path) || label || "文件");
  return button;
}

function appendLocalImagePreview(host, path, alt) {
  const card = document.createElement("button");
  card.type = "button";
  card.className = "message-local-image loading";
  card.setAttribute("aria-label", `预览图片 ${alt || fileNameFromPath(path) || ""}`.trim());
  const status = document.createElement("span");
  status.className = "message-local-image-status";
  status.textContent = `正在读取 ${alt || fileNameFromPath(path) || "图片"}…`;
  card.append(status);
  card.onclick = () => openFile(path, fileNameFromPath(path) || alt || "图片");
  host.append(card);
  readPreviewFile(path, MAX_MEDIA_FILE_BYTES).then(({ bytes }) => {
    if (!card.isConnected) return;
    if (!bytes.byteLength) throw new Error("图片文件为空");
    const image = document.createElement("img");
    image.className = "message-image";
    const objectUrl = URL.createObjectURL(new Blob([bytes], { type: fileMimeType(path, "image") }));
    image.src = objectUrl;
    image.alt = alt || fileNameFromPath(path) || "图片";
    image.loading = "lazy";
    image.onload = () => URL.revokeObjectURL(objectUrl);
    image.onerror = () => URL.revokeObjectURL(objectUrl);
    card.classList.remove("loading");
    card.replaceChildren(image);
  }).catch((error) => {
    if (!card.isConnected) return;
    card.classList.remove("loading");
    card.classList.add("error");
    status.textContent = `${alt || "图片"} · 点击重试`;
    card.title = error.message;
  });
}

function appendInlineMarkdown(host, source) {
  const pattern = /\[图片附件\]|!\[(?<imageAlt>[^\]]*)\]\((?:<(?<imageAngle>[^>]+)>|(?<imagePlain>[^)]+))\)|\[(?<linkText>[^\]]+)\]\((?:<(?<linkAngle>[^>]+)>|(?<linkPlain>[^)]+))\)|`(?<code>[^`\n]+)`|\*\*(?<strong>[^*\n]+)\*\*|~~(?<strike>[^~\n]+)~~|(?<!\*)\*(?<emphasis>[^*\n]+)\*(?!\*)|_(?<underscoreEmphasis>[^_\n]+)_|\[附件[:：]\s*(?<attachment>[^\]]+)\]/g;
  let offset = 0;
  let match;
  while ((match = pattern.exec(source)) !== null) {
    if (match.index > offset) host.append(document.createTextNode(source.slice(offset, match.index)));
    const groups = match.groups || {};
    if (groups.attachment !== undefined) {
      const attachment = document.createElement("span");
      attachment.className = "message-attachment";
      attachment.textContent = groups.attachment;
      host.append(attachment);
    } else if (match[0] === "[图片附件]") {
      const attachment = document.createElement("span");
      attachment.className = "message-attachment";
      attachment.textContent = "图片附件";
      host.append(attachment);
    } else if (groups.imageAlt !== undefined) {
      const reference = groups.imageAngle ?? groups.imagePlain ?? "";
      const url = safeLinkUrl(reference, true);
      const localPath = resolveLocalFilePath(reference, fileBasePath());
      if (url) {
        const image = document.createElement("img");
        image.className = "message-image";
        image.src = url;
        image.alt = groups.imageAlt || "图片";
        image.loading = "lazy";
        image.referrerPolicy = "no-referrer";
        host.append(image);
      } else if (localPath) {
        appendLocalImagePreview(host, localPath, groups.imageAlt);
      } else {
        const attachment = document.createElement("span");
        attachment.className = "message-attachment";
        attachment.textContent = groups.imageAlt || "图片附件";
        host.append(attachment);
      }
    } else if (groups.linkText !== undefined) {
      const reference = groups.linkAngle ?? groups.linkPlain ?? "";
      const url = safeLinkUrl(reference);
      const localPath = resolveLocalFilePath(reference, fileBasePath());
      if (url) {
        const link = document.createElement("a");
        link.href = url;
        link.target = "_blank";
        link.rel = "noopener noreferrer";
        link.textContent = groups.linkText;
        host.append(link);
      } else if (localPath) {
        host.append(createLocalAttachmentButton(localPath, groups.linkText));
      } else {
        host.append(document.createTextNode(match[0]));
      }
    } else if (groups.code !== undefined) {
      const code = document.createElement("code");
      code.textContent = groups.code;
      host.append(code);
    } else if (groups.strike !== undefined) {
      const strike = document.createElement("del");
      strike.textContent = groups.strike;
      host.append(strike);
    } else if (groups.emphasis !== undefined || groups.underscoreEmphasis !== undefined) {
      const emphasis = document.createElement("em");
      emphasis.textContent = groups.emphasis ?? groups.underscoreEmphasis;
      host.append(emphasis);
    } else {
      const strong = document.createElement("strong");
      strong.textContent = groups.strong;
      host.append(strong);
    }
    offset = pattern.lastIndex;
  }
  if (offset < source.length) host.append(document.createTextNode(source.slice(offset)));
}

export function splitMarkdownTableRow(line) {
  const source = String(line || "").replace(/^\||\|$/g, "");
  const cells = [];
  let current = "";
  for (let index = 0; index < source.length; index += 1) {
    const character = source[index];
    if (character === "\\" && ["|", "\\"].includes(source[index + 1])) {
      current += source[index + 1];
      index += 1;
    } else if (character === "|") {
      cells.push(current.trim());
      current = "";
    } else {
      current += character;
    }
  }
  cells.push(current.trim());
  return cells;
}

function renderMessageContent(host, text) {
  host.innerHTML = "";
  const normalized = normalizeMessageSyntax(text);
  const lines = normalized.split(/\r?\n/);
  let codeBlock = null;
  let list = null;
  let table = null;
  const flushList = () => { list = null; };
  const flushTable = () => { table = null; };
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    const fence = line.match(/^\s*```([\w+-]*)/);
    if (fence) {
      if (codeBlock) codeBlock = null;
      else {
        codeBlock = document.createElement("pre");
        const code = document.createElement("code");
        const copy = document.createElement("button");
        copy.type = "button";
        copy.className = "code-copy";
        copy.textContent = "复制";
        copy.onclick = async () => {
          try {
            await navigator.clipboard.writeText(code.textContent || "");
            copy.textContent = "已复制";
            setTimeout(() => { copy.textContent = "复制"; }, 1200);
          } catch {
            showToast("复制失败，请长按代码复制");
          }
        };
        if (fence[1]) {
          code.dataset.language = fence[1];
          codeBlock.dataset.language = fence[1];
        }
        codeBlock.append(code, copy);
        host.append(codeBlock);
      }
      flushList();
      flushTable();
      continue;
    }
    if (codeBlock) {
      const code = codeBlock.querySelector("code");
      code.textContent += `${code.textContent ? "\n" : ""}${line}`;
      continue;
    }
    const next = lines[index + 1] || "";
    if (line.includes("|") && /^\s*\|?\s*:?-{3,}/.test(next)) {
      flushList();
      table = document.createElement("table");
      const head = document.createElement("thead");
      const row = document.createElement("tr");
      for (const cell of splitMarkdownTableRow(line)) {
        const th = document.createElement("th");
        appendInlineMarkdown(th, cell.trim());
        row.append(th);
      }
      head.append(row);
      table.append(head, document.createElement("tbody"));
      host.append(table);
      index += 1;
      continue;
    }
    if (table && line.includes("|")) {
      const row = document.createElement("tr");
      for (const cell of splitMarkdownTableRow(line)) {
        const td = document.createElement("td");
        appendInlineMarkdown(td, cell.trim());
        row.append(td);
      }
      table.querySelector("tbody").append(row);
      continue;
    }
    flushTable();
    const listMatch = line.match(/^\s*(?:([-*+])|(\d+)\.)\s+(.+)$/);
    if (listMatch) {
      const tag = listMatch[2] ? "ol" : "ul";
      if (!list || list.tagName.toLowerCase() !== tag) {
        list = document.createElement(tag);
        host.append(list);
      }
      const item = document.createElement("li");
      const task = listMatch[3].match(/^\[([ xX])\]\s+(.+)$/);
      if (task) {
        item.className = "task-item";
        const checkbox = document.createElement("input");
        checkbox.type = "checkbox";
        checkbox.checked = task[1].toLowerCase() === "x";
        checkbox.disabled = true;
        item.append(checkbox);
        appendInlineMarkdown(item, task[2]);
      } else {
        appendInlineMarkdown(item, listMatch[3]);
      }
      list.append(item);
      continue;
    }
    flushList();
    if (/^\s*(?:---+|\*\*\*+|___+)\s*$/.test(line)) {
      host.append(document.createElement("hr"));
      continue;
    }
    if (/^\s*>\s?/.test(line)) {
      const quote = document.createElement("blockquote");
      appendInlineMarkdown(quote, line.replace(/^\s*>\s?/, ""));
      host.append(quote);
      continue;
    }
    const heading = line.match(/^(#{1,6})\s+(.+)$/);
    const block = document.createElement(heading ? `h${heading[1].length}` : "div");
    block.className = "message-line";
    appendInlineMarkdown(block, heading ? heading[2] : line);
    host.append(block);
  }
}

function appendMessage(role, text, options = {}) {
  const wrap = document.createElement("article");
  wrap.className = `message ${role}`;
  if (options.className) wrap.classList.add(...String(options.className).split(/\s+/).filter(Boolean));
  const label = document.createElement("span");
  label.className = "message-role";
  label.textContent = options.label || ROLE_LABEL[role] || role;
  const bubble = document.createElement("div");
  bubble.className = "message-bubble";
  const content = options.presented
    ? String(text || "")
    : presentConversationText(text, role, { partial: options.partial === true });
  const toolLines = role === "tool" ? content.split(/\r?\n/).filter((line) => line.trim()) : [];
  const collapsibleTool = role === "tool" && toolLines.length > 1;
  const isLong = !collapsibleTool && content.length > MAX_MESSAGE_CHARS;
  const toolSummary = collapsibleTool
    ? `${toolLines[0].slice(0, 180)}${toolLines[0].length > 180 ? "…" : ""}`
    : "";
  renderMessageContent(
    bubble,
    collapsibleTool ? toolSummary : (isLong ? content.slice(0, MAX_MESSAGE_CHARS) : content),
  );
  wrap.append(label, bubble);
  if (collapsibleTool) {
    wrap.classList.add("collapsed-tool");
    const actions = document.createElement("div");
    actions.className = "message-actions";
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.textContent = `展开执行详情（${toolLines.length} 行）`;
    let expanded = false;
    toggle.onclick = () => {
      expanded = !expanded;
      wrap.classList.toggle("expanded-tool", expanded);
      renderMessageContent(bubble, expanded ? content : toolSummary);
      toggle.textContent = expanded ? "收起执行详情" : `展开执行详情（${toolLines.length} 行）`;
    };
    actions.append(toggle);
    wrap.append(actions);
  } else if (isLong) {
    const actions = document.createElement("div");
    actions.className = "message-actions";
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.textContent = `展开全部（${content.length.toLocaleString("zh-CN")} 字）`;
    let expanded = false;
    toggle.onclick = () => {
      expanded = !expanded;
      renderMessageContent(bubble, expanded ? content : content.slice(0, MAX_MESSAGE_CHARS));
      toggle.textContent = expanded ? "收起长内容" : `展开全部（${content.length.toLocaleString("zh-CN")} 字）`;
    };
    const copy = document.createElement("button");
    copy.type = "button";
    copy.textContent = "复制全文";
    copy.onclick = async () => {
      try {
        await navigator.clipboard.writeText(content);
        showToast("全文已复制");
      } catch {
        showToast("复制失败，请长按文本复制");
      }
    };
    actions.append(toggle, copy);
    wrap.append(actions);
  }
  (options.host || el("messages")).appendChild(wrap);
  if (options.scroll !== false) markNewContent();
  return { wrap, bubble };
}

function itemDisplayLabel(item = {}) {
  if (item.type === "reasoning") return "思考摘要";
  if (item.type === "plan") return "执行计划";
  if (item.type === "commandExecution") return "命令执行";
  if (item.type === "fileChange") return "文件变更";
  if (item.type === "mcpToolCall") return "MCP 工具";
  if (item.type === "dynamicToolCall") return "工具调用";
  if (item.type === "collabAgentToolCall") return "子 Agent";
  if (item.type === "webSearch") return "网页搜索";
  if (item.type === "contextCompaction") return "上下文整理";
  if (item.type === "agentMessage" && item.phase === "commentary") return "Codex 进度";
  return ROLE_LABEL[itemRole(item)] || "执行过程";
}

function appendProcessGroup(items, turn, host) {
  const visibleItems = (items || []).filter((item) => itemText(item));
  if (!visibleItems.length) return;
  const details = document.createElement("details");
  details.className = "turn-process";
  const active = turnIsActive(turn);
  const liveActive = active && rpc?.connected && !state.reconnecting;
  details.dataset.runtimeActive = active ? "true" : "false";
  details.classList.toggle("active", liveActive);
  details.open = liveActive;
  const summary = document.createElement("summary");
  const status = active ? (liveActive ? "正在执行" : "断线前执行中") : "已完成";
  summary.innerHTML = `<span><i aria-hidden="true"></i>思考与执行过程</span><small><span ${active ? 'data-runtime-label="turn"' : ""}>${esc(status)}</span> · ${visibleItems.length} 项</small>`;
  const body = document.createElement("div");
  body.className = "turn-process-body";
  for (const item of visibleItems) {
    const role = itemRole(item) === "tool" ? "tool" : "progress";
    appendMessage(role, itemText(item), {
      host: body,
      label: itemDisplayLabel(item),
      scroll: false,
      className: "process-item",
    });
  }
  details.append(summary, body);
  host.append(details);
}

function renderTurns(turns, { scrollMode = "bottom" } = {}) {
  const messages = el("messages");
  const previousHeight = messages.scrollHeight;
  const previousTop = messages.scrollTop;
  resetRuntimeStreamNodes(selectedRuntime());
  messages.innerHTML = "";
  if (state.threadTurnsCursor) {
    const older = document.createElement("button");
    older.type = "button";
    older.className = "load-older-turns";
    older.textContent = state.loadingOlderTurns ? "正在加载…" : "加载更早记录";
    older.disabled = state.loadingOlderTurns;
    older.onclick = () => loadOlderTurns();
    messages.appendChild(older);
  }
  for (const turn of turns || []) {
    for (const segment of turnTimelineSegments(turn)) {
      if (segment.kind === "process") {
        appendProcessGroup(segment.items, turn, messages);
        continue;
      }
      for (const item of segment.items) {
        const text = itemText(item);
        if (!text) continue;
        const isFinal = segment.kind === "final";
        appendMessage(itemRole(item), text, {
          scroll: false,
          label: isFinal ? "最终结论" : undefined,
          className: isFinal ? "final-answer" : "",
        });
      }
    }
  }
  if (!messages.children.length) {
    messages.innerHTML = '<p class="side-empty">这个会话还没有消息，可以直接发送第一条指令。</p>';
  }
  if (scrollMode === "prepend") {
    messages.scrollTop = Math.max(0, previousTop + messages.scrollHeight - previousHeight);
  } else if (scrollMode === "retain") {
    messages.scrollTop = previousTop;
  } else {
    state.followOutput = true;
    state.newContentPending = false;
    messages.scrollTop = messages.scrollHeight;
  }
  updateJumpLatestButton();
}

function resetRuntimeStreamNodes(runtime) {
  for (const stream of runtime?.liveStreams?.values?.() || []) {
    stream.node = null;
    stream.wrap = null;
  }
  if (runtime) {
    runtime.liveProcessDetails = null;
    runtime.liveProcessBody = null;
    runtime.liveProcessCount = 0;
  }
}

function streamRole(stream) {
  return stream?.phase === "final_answer" ? "agent" : "progress";
}

function updateLiveProcessSummary(runtime) {
  const summary = runtime?.liveProcessDetails?.querySelector("summary small");
  if (summary) summary.textContent = `正在执行 · ${runtime.liveProcessCount} 项`;
}

function ensureLiveProcessContainer(runtime) {
  if (runtime?.liveProcessDetails?.isConnected && runtime.liveProcessBody?.isConnected) {
    return runtime.liveProcessBody;
  }
  const details = document.createElement("details");
  details.className = "turn-process active live-turn-process";
  details.open = true;
  const summary = document.createElement("summary");
  summary.innerHTML = '<span><i aria-hidden="true"></i>当前执行过程</span><small>正在执行 · 0 项</small>';
  const body = document.createElement("div");
  body.className = "turn-process-body";
  details.append(summary, body);
  el("messages").append(details);
  runtime.liveProcessDetails = details;
  runtime.liveProcessBody = body;
  runtime.liveProcessCount = 0;
  return body;
}

function appendLiveProcessItem(runtime, item) {
  const text = itemText(item);
  if (!text) return;
  const body = ensureLiveProcessContainer(runtime);
  appendMessage(itemRole(item) === "tool" ? "tool" : "progress", text, {
    host: body,
    label: itemDisplayLabel(item),
    className: "process-item",
  });
  runtime.liveProcessCount += 1;
  updateLiveProcessSummary(runtime);
}

function ensureLiveStreamNode(runtime, stream) {
  if (!runtime?.turnActive || !stream?.text || state.selectedThreadId !== stream.threadId) return null;
  if (stream.node?.isConnected) return stream.node;
  const isFinal = stream.phase === "final_answer";
  const host = isFinal ? el("messages") : ensureLiveProcessContainer(runtime);
  const appended = appendMessage(streamRole(stream), stream.text, {
    host,
    presented: true,
    label: isFinal ? "最终结论" : itemDisplayLabel(stream),
    className: isFinal ? "final-answer live-stream" : "live-stream",
  });
  stream.node = appended.bubble;
  stream.wrap = appended.wrap;
  stream.node.dataset.rawText = stream.text;
  if (!isFinal) {
    runtime.liveProcessCount += 1;
    updateLiveProcessSummary(runtime);
  }
  return stream.node;
}

function appendRuntimeStreams(runtime) {
  if (!runtime?.turnActive) return;
  resetRuntimeStreamNodes(runtime);
  for (const stream of runtime.liveStreams.values()) ensureLiveStreamNode(runtime, stream);
}

function scheduleLiveStreamRender(runtime, stream) {
  if (!stream || stream.renderTimer) return;
  stream.renderTimer = setTimeout(() => {
    stream.renderTimer = null;
    if (state.selectedThreadId !== stream.threadId || !runtime.turnActive) return;
    const node = ensureLiveStreamNode(runtime, stream);
    if (!node) return;
    node.dataset.rawText = stream.text;
    renderMessageContent(node, stream.text);
    markNewContent();
    updateSyncStatus();
  }, STREAM_RENDER_INTERVAL_MS);
}

async function loadTurnPage(threadId, cursor = null) {
  return rpc.call("thread/turns/list", {
    threadId,
    limit: TURN_PAGE_SIZE,
    sortDirection: "desc",
    ...(cursor ? { cursor } : {}),
  }, 45000);
}

function setThreadTurnsSnapshot(threadId, turns, cursor = null) {
  const normalizedTurns = Array.isArray(turns) ? turns : [];
  const runtime = runtimeFor(threadId);
  runtime.turns = normalizedTurns;
  runtime.turnsCursor = cursor || null;
  runtime.hasTurnSnapshot = true;
  if (threadId === state.selectedThreadId) {
    state.threadTurns = normalizedTurns;
    state.threadTurnsCursor = runtime.turnsCursor;
  }
}

async function loadOlderTurns() {
  if (!state.selectedThreadId || !state.threadTurnsCursor || state.loadingOlderTurns) return;
  const threadId = state.selectedThreadId;
  state.loadingOlderTurns = true;
  renderTurns(state.threadTurns, { scrollMode: "retain" });
  try {
    const page = await loadTurnPage(threadId, state.threadTurnsCursor);
    if (threadId !== state.selectedThreadId) return;
    const older = [...(page?.data || [])].reverse();
    setThreadTurnsSnapshot(
      threadId,
      [...older, ...state.threadTurns],
      page?.nextCursor || null,
    );
    renderTurns(state.threadTurns, { scrollMode: "prepend" });
  } catch (error) {
    showToast(`更早记录加载失败：${error.message}`);
  } finally {
    state.loadingOlderTurns = false;
    if (threadId === state.selectedThreadId) renderTurns(state.threadTurns, { scrollMode: "retain" });
  }
}

function showConversation(thread) {
  const previousThreadId = state.selectedThreadId;
  const switchingThread = Boolean(previousThreadId && previousThreadId !== thread.id);
  if (switchingThread) saveComposerDraft(previousThreadId);
  state.selectedThreadId = thread.id;
  persistSelectedThreadId(thread.id);
  state.selectedCwd = normalizeCwd(thread.cwd);
  el("threadTitle").textContent = threadTitle(thread);
  el("threadMeta").textContent = state.selectedCwd || "未关联项目目录";
  const workspaceName = projectLabel(state.selectedCwd);
  el("workspaceLabel").textContent = workspaceName === "Mirror X Codex"
    ? "手机工作台"
    : workspaceName;
  el("welcome").hidden = true;
  el("messages").hidden = false;
  const runtime = runtimeFor(thread.id);
  runtime.pendingSubmission ||= readPendingSubmission(thread.id);
  if (previousThreadId !== thread.id) {
    restoreComposerDraft(thread.id);
    state.followOutput = true;
    state.newContentPending = false;
    updateJumpLatestButton();
  }
  renderHistory();
  if (state.selectedCwd) selectProject(state.selectedCwd, false);
  syncSelectedRuntimeUi();
  closeDrawer();
}

async function openThread(thread) {
  if (!thread?.id) return;
  const generation = ++state.threadOpenGeneration;
  const openingRuntime = runtimeFor(thread.id);
  openingRuntime.threadReady = false;
  openingRuntime.syncing = true;
  openingRuntime.syncIssue = "";
  showConversation(thread);
  if (openingRuntime.hasTurnSnapshot) {
    state.threadTurns = openingRuntime.turns;
    state.threadTurnsCursor = openingRuntime.turnsCursor;
    renderTurns(state.threadTurns, { scrollMode: "bottom" });
    appendRuntimeStreams(openingRuntime);
  } else {
    state.threadTurns = [];
    state.threadTurnsCursor = null;
    renderConversationSkeleton();
  }
  updateSyncStatus();
  const readPromise = rpc.call(
    "thread/read",
    { threadId: thread.id, includeTurns: false },
    30000,
  ).then((read) => ({ ok: true, read })).catch((error) => ({ ok: false, error }));
  try {
    const page = await loadTurnPage(thread.id);
    if (generation !== state.threadOpenGeneration || state.selectedThreadId !== thread.id) return;
    setThreadTurnsSnapshot(
      thread.id,
      [...(page?.data || [])].reverse(),
      page?.nextCursor || null,
    );
    const runtime = runtimeFor(thread.id);
    const activeTurn = [...state.threadTurns].reverse().find(turnIsActive);
    if (activeTurn) {
      runtime.turnActive = true;
      runtime.turnId = activeTurn.id || activeTurn.turnId || runtime.turnId;
      runtime.pendingRefresh = true;
      runtime.turnStartedAt ||= Date.now();
    } else if (!threadIsActive(thread)) {
      runtime.turnActive = false;
      runtime.turnId = null;
    }
    renderTurns(state.threadTurns, {
      scrollMode: conversationRefreshScrollMode(state.followOutput),
    });
    appendRuntimeStreams(runtime);
    runtime.pendingRefresh = runtime.turnActive;
    runtime.syncing = false;
    runtime.lastSyncAt = Date.now();
    runtime.syncIssue = "";
    syncSelectedRuntimeUi();
    readPromise.then((outcome) => {
      if (!outcome.ok) {
        console.warn("thread metadata read failed after timeline loaded", outcome.error);
        return;
      }
      if (generation !== state.threadOpenGeneration || state.selectedThreadId !== thread.id) return;
      const read = outcome.read;
      const active = { ...thread, ...(read?.thread || {}) };
      showConversation(active);
      openingRuntime.threadReady = active?.status?.type !== "notLoaded";
      if (!threadIsActive(active) && !runtime.turnActive) {
        runtime.turnId = null;
      }
      syncSelectedRuntimeUi();
    });
  } catch (error) {
    if (generation !== state.threadOpenGeneration || state.selectedThreadId !== thread.id) return;
    const message = String(error?.message || error || "");
    if (message.includes("not materialized") || message.includes("before first user message")) {
      state.threadTurns = [];
      state.threadTurnsCursor = null;
      openingRuntime.threadReady = true;
      openingRuntime.syncing = false;
      openingRuntime.syncIssue = "";
      renderTurns([]);
      syncSelectedRuntimeUi();
      return;
    }
    if (openingRuntime.hasTurnSnapshot) {
      openingRuntime.syncing = false;
      openingRuntime.syncIssue = message || "会话更新失败";
      renderTurns(openingRuntime.turns, {
        scrollMode: conversationRefreshScrollMode(state.followOutput),
      });
      appendRuntimeStreams(openingRuntime);
      updateSyncStatus();
      return;
    }
    el("messages").innerHTML = "";
    appendMessage("error", `此会话文件无法读取：${message}`);
    openingRuntime.syncing = false;
    openingRuntime.syncIssue = message || "会话读取失败";
    updateSyncStatus();
  }
}

async function ensureThreadReady(threadId = state.selectedThreadId) {
  const runtime = runtimeFor(threadId);
  if (runtime.threadReady) return threadId;
  if (runtime.resumePromise) return runtime.resumePromise;
  const thread = state.threads.find((item) => item.id === threadId);
  if (!thread) throw new Error("请先选择会话");
  runtime.resumePromise = rpc.call("thread/resume", {
    threadId: thread.id,
    excludeTurns: true,
    persistExtendedHistory: true,
  }, 45000).then((result) => {
    runtime.threadReady = true;
    if (result?.thread && state.selectedThreadId === thread.id) showConversation(result.thread);
    return thread.id;
  }).catch(async (error) => {
    const message = String(error?.message || error);
    if (!message.includes("already has an active writer")) throw error;
    if (state.connectionMode === "desktopSync") {
      runtime.threadReady = true;
      return thread.id;
    }

    const result = await rpc.call("thread/fork", {
      threadId: thread.id,
      excludeTurns: true,
      ephemeral: false,
      deferGoalContinuation: true,
    }, 120000);
    const forked = result?.thread || result;
    if (!forked?.id) throw new Error("Codex 未返回手机续接会话");
    mergeThreads([forked]);
    const forkedRuntime = runtimeFor(forked.id);
    forkedRuntime.threadReady = true;
    if (state.selectedThreadId === thread.id) {
      state.selectedThreadId = forked.id;
      state.mobileForkSourceId = thread.id;
      showConversation(forked);
      el("workspaceLabel").textContent = "手机续接会话";
      el("threadMeta").textContent = `${state.selectedCwd || "未关联项目"} · 与电脑当前会话分开`;
      showToast("电脑正在使用原会话，已创建“手机续接会话”。请在电脑历史会话中打开新会话查看。");
    }
    return forked.id;
  }).finally(() => {
    runtime.resumePromise = null;
  });
  return runtime.resumePromise;
}

async function startThread(cwd = state.fileRoot || state.selectedCwd) {
  try {
    const result = await rpc.call("thread/start", {
      ...(cwd ? { cwd } : {}),
      approvalPolicy: MOBILE_FULL_ACCESS.approvalPolicy,
      sandbox: MOBILE_FULL_ACCESS.sandbox,
    });
    const thread = result?.thread || result;
    if (!thread?.id) throw new Error("Codex 未返回会话 ID");
    mergeThreads([thread]);
    renderHistory();
    renderProjects();
    await openThread(thread);
  } catch (error) {
    showToast(`新建会话失败：${error.message}`);
  }
}

function joinPath(parent, name) {
  const separator = parent.includes("\\") ? "\\" : "/";
  return `${parent.replace(/[\\/]+$/, "")}${separator}${name}`;
}

function fileIcon(name, directory) {
  if (directory) return "▱";
  const ext = name.split(".").pop().toLowerCase();
  if (["png", "jpg", "jpeg", "gif", "webp", "svg"].includes(ext)) return "▧";
  if (["md", "txt", "json", "toml", "yaml", "yml"].includes(ext)) return "≡";
  return "·";
}

async function renderDirectory(path, host, {
  generation = state.fileTreeGeneration,
  root = state.fileRoot,
} = {}) {
  const requestIsCurrent = () => (
    generation === state.fileTreeGeneration
    && root === state.fileRoot
    && host.isConnected
  );
  if (!requestIsCurrent()) return false;
  host.innerHTML = '<p class="file-loading">正在读取…</p>';
  try {
    const result = await rpc.call("fs/readDirectory", { path });
    if (!requestIsCurrent()) return false;
    const entries = (result?.entries || []).sort((a, b) => {
      if (a.isDirectory !== b.isDirectory) return a.isDirectory ? -1 : 1;
      return a.fileName.localeCompare(b.fileName, "zh-CN");
    });
    host.innerHTML = "";
    if (!entries.length) host.innerHTML = '<p class="file-loading">空目录</p>';
    for (const entry of entries) {
      if ([".git", "node_modules", "target", "dist", ".next"].includes(entry.fileName)) continue;
      const fullPath = joinPath(path, entry.fileName);
      const wrapper = document.createElement("div");
      const row = document.createElement("button");
      row.type = "button";
      row.className = "file-row";
      row.innerHTML = `<span class="chev">${entry.isDirectory ? "›" : ""}</span>
        <span class="file-icon">${fileIcon(entry.fileName, entry.isDirectory)}</span>
        <span class="file-name">${esc(entry.fileName)}</span>`;
      if (entry.isDirectory) {
        let open = false;
        let loaded = false;
        const children = document.createElement("div");
        children.className = "file-children";
        children.hidden = true;
        row.onclick = async () => {
          if (!requestIsCurrent()) return;
          open = !open;
          row.querySelector(".chev").textContent = open ? "⌄" : "›";
          children.hidden = !open;
          if (open && !loaded) {
            loaded = await renderDirectory(fullPath, children, { generation, root });
          }
        };
        wrapper.append(row, children);
      } else {
        row.onclick = () => openFile(fullPath, entry.fileName);
        wrapper.appendChild(row);
      }
      host.appendChild(wrapper);
    }
    return true;
  } catch (error) {
    if (!requestIsCurrent()) return false;
    host.innerHTML = `<p class="side-empty">目录读取失败<br>${esc(error.message)}</p>`;
    return false;
  }
}

async function selectProject(cwd, load = true, { force = false } = {}) {
  const normalized = normalizeCwd(cwd);
  if (!normalized) return;
  const rootChanged = normalized !== state.fileRoot;
  if (rootChanged) {
    state.fileTreeGeneration += 1;
    state.fileTreeLoadingRoot = "";
    state.fileTreeLoadedRoot = "";
    state.fileTreeLoadPromise = null;
    el("fileTree").innerHTML = '<p class="file-loading">选择“项目文件”后读取目录。</p>';
  }
  state.fileRoot = normalized;
  el("fileProjectName").textContent = projectLabel(normalized);
  el("fileProjectPath").textContent = normalized;
  renderProjects();
  if (!load) return;
  if (!force && state.fileTreeLoadedRoot === normalized) return;
  if (
    !force
    && state.fileTreeLoadingRoot === normalized
    && state.fileTreeLoadPromise
  ) return state.fileTreeLoadPromise;

  const generation = ++state.fileTreeGeneration;
  state.fileTreeLoadingRoot = normalized;
  let loadPromise;
  loadPromise = renderDirectory(normalized, el("fileTree"), {
    generation,
    root: normalized,
  }).then((rendered) => {
    if (
      rendered
      && generation === state.fileTreeGeneration
      && state.fileRoot === normalized
    ) state.fileTreeLoadedRoot = normalized;
    return rendered;
  }).finally(() => {
    if (state.fileTreeLoadPromise !== loadPromise) return;
    state.fileTreeLoadPromise = null;
    if (state.fileTreeLoadingRoot === normalized) state.fileTreeLoadingRoot = "";
  });
  state.fileTreeLoadPromise = loadPromise;
  return loadPromise;
}

async function openFile(path, name, {
  pushHistory = true,
  rememberFocus = true,
  focus = true,
} = {}) {
  const viewer = el("fileViewer");
  const displayName = name || fileNameFromPath(path) || "文件";
  if (rememberFocus && viewer.hidden) fileViewerFocusOrigin = document.activeElement;
  if (pushHistory) pushOverlayHistory({ kind: "file", path, name: displayName });
  const generation = ++state.fileViewerGeneration;
  const kind = filePreviewKind(path);
  const host = el("fileContent");
  state.fileViewerMode = "preview";
  state.fileViewerRawText = "";
  state.fileViewerPath = path;
  state.fileViewerName = displayName;
  if (state.fileViewerObjectUrl) URL.revokeObjectURL(state.fileViewerObjectUrl);
  state.fileViewerObjectUrl = "";
  viewer.hidden = false;
  el("fileViewerName").textContent = displayName;
  el("fileViewerPath").textContent = path;
  el("fileSourceBtn").hidden = true;
  el("fileSourceBtn").textContent = "查看源码";
  host.className = "file-content loading";
  host.textContent = "正在从电脑读取文件…";
  applyDrawerState(false, { restoreFocus: false });
  syncOverlayAccessibility();
  if (focus) focusFileViewer();
  try {
    if (kind === "binary") throw new Error("该文件格式暂不支持手机预览");
    const limit = ["image", "video", "pdf"].includes(kind)
      ? MAX_MEDIA_FILE_BYTES
      : MAX_TEXT_FILE_BYTES;
    const { bytes } = await readPreviewFile(path, limit);
    if (generation !== state.fileViewerGeneration) return;
    host.className = `file-content ${kind}`;
    host.innerHTML = "";
    if (kind === "image") {
      if (!bytes.byteLength) throw new Error("图片文件为空");
      const image = document.createElement("img");
      image.className = "file-preview-image";
      state.fileViewerObjectUrl = URL.createObjectURL(new Blob([bytes], { type: fileMimeType(path, kind) }));
      image.src = state.fileViewerObjectUrl;
      image.alt = name || fileNameFromPath(path) || "图片";
      host.append(image);
      return;
    }
    if (kind === "video") {
      if (!bytes.byteLength) throw new Error("视频文件为空");
      const video = document.createElement("video");
      video.className = "file-preview-video";
      state.fileViewerObjectUrl = URL.createObjectURL(new Blob([bytes], { type: fileMimeType(path, kind) }));
      video.src = state.fileViewerObjectUrl;
      video.controls = true;
      video.playsInline = true;
      video.preload = "metadata";
      host.append(video);
      return;
    }
    if (kind === "pdf") {
      if (!bytes.byteLength) throw new Error("PDF 文件为空");
      const frame = document.createElement("iframe");
      frame.className = "file-preview-pdf";
      state.fileViewerObjectUrl = URL.createObjectURL(new Blob([bytes], { type: "application/pdf" }));
      frame.src = state.fileViewerObjectUrl;
      frame.title = name || "PDF 预览";
      host.append(frame);
      return;
    }
    const text = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
    state.fileViewerRawText = text;
    if (kind === "markdown") {
      const article = document.createElement("article");
      article.className = "file-markdown message-bubble";
      renderMessageContent(article, text);
      host.append(article);
      el("fileSourceBtn").hidden = false;
    } else {
      const pre = document.createElement("pre");
      pre.className = "file-source";
      pre.textContent = text;
      host.append(pre);
    }
  } catch (error) {
    if (generation !== state.fileViewerGeneration) return;
    if (state.fileViewerObjectUrl) URL.revokeObjectURL(state.fileViewerObjectUrl);
    state.fileViewerObjectUrl = "";
    host.className = "file-content error";
    host.innerHTML = "";
    const title = document.createElement("strong");
    title.textContent = "无法预览";
    const detail = document.createElement("span");
    detail.textContent = error.message;
    host.append(title, detail);
  }
}

function toggleFileSource() {
  if (!state.fileViewerRawText) return;
  const host = el("fileContent");
  const sourceMode = state.fileViewerMode !== "source";
  state.fileViewerMode = sourceMode ? "source" : "preview";
  host.innerHTML = "";
  if (sourceMode) {
    const pre = document.createElement("pre");
    pre.className = "file-source";
    pre.textContent = state.fileViewerRawText;
    host.append(pre);
  } else {
    const article = document.createElement("article");
    article.className = "file-markdown message-bubble";
    renderMessageContent(article, state.fileViewerRawText);
    host.append(article);
  }
  el("fileSourceBtn").textContent = sourceMode ? "查看预览" : "查看源码";
}

function closeFileViewerVisual({ restoreFocus = true } = {}) {
  if (el("fileViewer").hidden) return;
  state.fileViewerGeneration += 1;
  el("fileViewer").hidden = true;
  if (state.fileViewerObjectUrl) URL.revokeObjectURL(state.fileViewerObjectUrl);
  state.fileViewerObjectUrl = "";
  state.fileViewerRawText = "";
  state.fileViewerMode = "preview";
  state.fileViewerPath = "";
  state.fileViewerName = "";
  syncOverlayAccessibility();
  if (restoreFocus) restoreOverlayFocus(fileViewerFocusOrigin);
}

function closeFileViewer({ consumeHistory = true } = {}) {
  if (el("fileViewer").hidden) return;
  if (consumeHistory) {
    requestOverlayClose("file");
    return;
  }
  closeFileViewerVisual();
}

function setActivePanel(panel) {
  state.activePanel = panel;
  document.querySelectorAll(".nav-tab").forEach((node) => node.classList.toggle("active", node.dataset.panel === panel));
  document.querySelectorAll(".side-panel").forEach((node) => node.classList.toggle("active", node.id === `${panel}Panel`));
  if (panel === "files" && state.fileRoot) selectProject(state.fileRoot);
}

function openDrawer() { setDrawer(true); }
function closeDrawer(options = {}) { setDrawer(false, options); }
function toggleDrawer() { setDrawer(!state.drawerOpen); }

function formatAttachmentBytes(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function attachmentKind(attachment) {
  const type = String(attachment.mimeType || attachment.file?.type || "");
  if (type.startsWith("image/")) return "图片";
  if (type.startsWith("video/")) return "视频";
  if (type === "application/pdf") return "PDF";
  return "文件";
}

function renderAttachmentList() {
  const host = el("attachmentList");
  host.innerHTML = "";
  host.hidden = state.selectedAttachments.length === 0;
  const count = el("attachmentCount");
  const attachmentButton = el("attachmentBtn");
  if (count) {
    count.hidden = state.selectedAttachments.length === 0;
    count.textContent = String(state.selectedAttachments.length);
  }
  attachmentButton?.classList.toggle("has-items", state.selectedAttachments.length > 0);
  attachmentButton?.setAttribute(
    "aria-label",
    state.selectedAttachments.length
      ? `已选择 ${state.selectedAttachments.length} 个附件，继续添加`
      : "添加图片、视频或文件",
  );
  for (const attachment of state.selectedAttachments) {
    const chip = document.createElement("div");
    chip.className = "attachment-chip";
    chip.dataset.state = attachment.status || "ready";
    const kind = document.createElement("span");
    kind.className = "attachment-kind";
    kind.textContent = attachmentKind(attachment);
    const copy = document.createElement("span");
    copy.className = "attachment-copy";
    const name = document.createElement("strong");
    name.textContent = attachment.name;
    const detail = document.createElement("small");
    if (attachment.status === "uploading") {
      detail.textContent = `正在上传 ${Math.round((attachment.progress || 0) * 100)}%`;
    } else if (attachment.status === "error") {
      detail.textContent = attachment.error || "上传失败";
    } else if (attachment.remotePath) {
      detail.textContent = attachmentKind(attachment) === "视频"
        ? `${formatAttachmentBytes(attachment.size)} · 已传到电脑，按文件处理`
        : `${formatAttachmentBytes(attachment.size)} · 已传到电脑`;
    } else {
      detail.textContent = attachmentKind(attachment) === "视频"
        ? `${formatAttachmentBytes(attachment.size)} · 将按文件上传`
        : formatAttachmentBytes(attachment.size);
    }
    copy.append(name, detail);
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "attachment-remove";
    remove.setAttribute("aria-label", `移除 ${attachment.name}`);
    remove.textContent = "×";
    remove.disabled = attachment.status === "uploading" || state.sendInFlight;
    remove.onclick = () => {
      state.selectedAttachments = state.selectedAttachments.filter((item) => item.id !== attachment.id);
      saveComposerDraft();
      renderAttachmentList();
      syncSelectedRuntimeUi();
    };
    chip.append(kind, copy, remove);
    if (attachment.status === "uploading") {
      const progress = document.createElement("span");
      progress.className = "attachment-progress";
      progress.style.setProperty("--progress", `${Math.round((attachment.progress || 0) * 100)}%`);
      progress.append(document.createElement("span"));
      chip.append(progress);
    }
    host.append(chip);
  }
}

function addSelectedAttachments(files) {
  const incoming = [...(files || [])];
  if (!incoming.length) return;
  const available = MAX_ATTACHMENT_FILES - state.selectedAttachments.length;
  if (available <= 0) {
    showToast(`一次最多添加 ${MAX_ATTACHMENT_FILES} 个附件`);
    return;
  }
  let total = state.selectedAttachments.reduce((sum, item) => sum + item.size, 0);
  for (const file of incoming.slice(0, available)) {
    if (file.size === 0) {
      showToast(`${file.name} 是空文件，未添加`);
      continue;
    }
    if (file.size > MAX_ATTACHMENT_BYTES) {
      showToast(`${file.name} 超过 25 MB，未添加`);
      continue;
    }
    if (total + file.size > MAX_ATTACHMENT_TOTAL_BYTES) {
      showToast("附件总量不能超过 50 MB");
      break;
    }
    total += file.size;
    state.selectedAttachments.push({
      id: `local-${globalThis.crypto?.randomUUID?.() || `${Date.now()}-${Math.random()}`}`,
      file,
      name: file.name,
      mimeType: file.type || "application/octet-stream",
      size: file.size,
      status: "ready",
      progress: 0,
      remotePath: null,
    });
  }
  if (incoming.length > available) showToast(`一次最多添加 ${MAX_ATTACHMENT_FILES} 个附件`);
  if (incoming.some((file) => String(file.type || "").startsWith("video/"))) {
    showToast("视频可以上传，但当前作为文件交给 Codex，不等同于直接观看视频内容");
  }
  saveComposerDraft();
  renderAttachmentList();
  syncSelectedRuntimeUi();
}

async function prepareSubmissionAttachments(attachments, ownerThreadId) {
  const prepared = [];
  for (const attachment of attachments) {
    if (attachment.remotePath) {
      prepared.push({
        id: attachment.id,
        name: attachment.name,
        mimeType: attachment.mimeType,
        size: attachment.size,
        path: attachment.remotePath,
      });
      continue;
    }
    if (!attachment.file) throw new Error(`${attachment.name} 需要重新选择`);
    attachment.status = "uploading";
    attachment.error = "";
    attachment.progress = 0;
    if (state.selectedThreadId === ownerThreadId) renderAttachmentList();
    try {
      const uploaded = await rpc.uploadFile(attachment.file, (progress) => {
        attachment.progress = progress;
        if (state.selectedThreadId === ownerThreadId) renderAttachmentList();
      });
      attachment.remotePath = uploaded.path;
      attachment.status = "uploaded";
      attachment.progress = 1;
      prepared.push(uploaded);
      if (state.selectedThreadId === ownerThreadId) renderAttachmentList();
    } catch (error) {
      attachment.status = "error";
      attachment.error = error.message;
      if (state.selectedThreadId === ownerThreadId) renderAttachmentList();
      throw new Error(`${attachment.name} 上传失败：${error.message}`);
    }
  }
  return prepared;
}

function submissionInput(submission) {
  const input = [];
  if (String(submission.text || "").trim()) {
    input.push({ type: "text", text: submission.text });
  }
  for (const attachment of submission.attachments || []) {
    if (String(attachment.mimeType || "").startsWith("image/")) {
      input.push({ type: "localImage", path: attachment.path });
    } else {
      input.push({ type: "mention", name: attachment.name, path: attachment.path });
    }
  }
  return input;
}

function submissionPreviewText(text, attachments = []) {
  const names = attachments.map((attachment) => attachment.name);
  const base = String(text || "").trim() || "请查看并分析附件。";
  return names.length ? `${base}\n\n附件：${names.join("、")}` : base;
}

function restoreSubmissionToComposer(submission) {
  el("messageInput").value = submission?.text || "";
  state.selectedAttachments = (submission?.attachments || []).map((attachment) => ({
    id: attachment.id || `restored-${Date.now()}-${Math.random()}`,
    file: null,
    name: attachment.name,
    mimeType: attachment.mimeType,
    size: attachment.size,
    status: "uploaded",
    progress: 1,
    remotePath: attachment.path,
  }));
  saveComposerDraft(submission?.threadId || state.selectedThreadId);
  renderAttachmentList();
}

function activityLabel(method, params = {}) {
  const item = params.item || {};
  const type = item.type || params.type || "";
  if (method === "turn/started") return "正在分析任务";
  if (method === "turn/completed") return "任务已完成";
  if (method === "turn/plan/updated" || method.includes("plan")) return "正在规划步骤";
  if (method.includes("agentMessage")) return "正在生成回复";
  if (type === "collabAgentToolCall") {
    return collabAgentSummary({
      ...item,
      status: method === "item/completed" ? "completed" : item.status,
    });
  }
  if (type === "commandExecution") return "正在执行命令";
  if (type === "fileChange") return "正在修改文件";
  if (type === "mcpToolCall") return `正在调用 MCP${item.server ? `：${item.server}` : ""}`;
  if (type === "dynamicToolCall") return `正在调用工具${item.tool ? `：${item.tool}` : ""}`;
  if (type === "webSearch") return "正在检索资料";
  if (method.includes("requestApproval")) return "检测到意外审批请求，正在等待电脑处理";
  if (method === "item/started") return "正在处理";
  if (method === "item/completed") return "步骤已完成";
  return "";
}

function formatElapsed(milliseconds) {
  const seconds = Math.max(0, Math.floor(Number(milliseconds || 0) / 1000));
  if (seconds < 60) return `${seconds} 秒`;
  const minutes = Math.floor(seconds / 60);
  const remainder = seconds % 60;
  if (minutes < 60) return remainder ? `${minutes} 分 ${remainder} 秒` : `${minutes} 分钟`;
  const hours = Math.floor(minutes / 60);
  return `${hours} 小时 ${minutes % 60} 分`;
}

function updateActivity(label, {
  done = false,
  error = false,
  detail = "",
  runtime = selectedRuntime(),
} = {}) {
  if (label) {
    const log = runtime?.activityLog || state.activityLog;
    log.push({ label, detail, time: Date.now(), done, error });
    if (log.length > 30) log.splice(0, log.length - 30);
    if (runtime) {
      runtime.lastActivityAt = Date.now();
      runtime.currentActivityLabel = label;
      runtime.currentActivityDetail = detail;
    }
    if (runtime === selectedRuntime()) state.activityLog = log;
  }
  if (runtime && runtime !== selectedRuntime()) {
    updateGlobalRuntimeState();
    return;
  }
  const bar = el("activityBar");
  if (!state.turnActive && !error && !done) {
    bar.hidden = true;
  } else {
    bar.hidden = false;
    el("activityText").textContent = label || "正在工作";
    bar.classList.toggle("error", error);
    bar.classList.toggle("waiting", label.includes("等待"));
  }
  if (done) setTimeout(() => {
    if (!state.turnActive) bar.hidden = true;
  }, 1800);
  renderActivityDetails();
  updateSyncStatus();
  updateGlobalRuntimeState();
}

function renderActivityDetails() {
  const details = el("activityDetails");
  details.innerHTML = "";
  const runtime = selectedRuntime();
  if (runtime?.turnActive) {
    const summary = document.createElement("div");
    summary.className = "activity-summary";
    const elapsed = runtime.turnStartedAt ? formatElapsed(Date.now() - runtime.turnStartedAt) : "计算中";
    const sinceActivity = runtime.lastActivityAt ? formatElapsed(Date.now() - runtime.lastActivityAt) : "暂无事件";
    summary.innerHTML = `<span>${esc(runtime.currentActivityLabel || "Codex 正在工作")}</span><small>已运行 ${esc(elapsed)} · 最近活动 ${esc(sinceActivity)}前</small>`;
    details.append(summary);
  }
  for (const entry of state.activityLog.slice(-10)) {
    const row = document.createElement("div");
    row.className = `activity-row ${entry.error ? "error" : ""}`;
    const time = new Date(entry.time).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
    row.innerHTML = `<span>${esc(entry.label)}</span><time>${esc(time)}</time>${entry.detail ? `<small>${esc(entry.detail)}</small>` : ""}`;
    details.append(row);
  }
}

function showTyping() {
  if (typingEl) return;
  typingEl = document.createElement("div");
  typingEl.className = "typing-indicator";
  typingEl.innerHTML = '<i class="typing-dot"></i><i class="typing-dot"></i><i class="typing-dot"></i>';
  el("messages").appendChild(typingEl);
  markNewContent();
}

function hideTyping() {
  typingEl?.remove();
  typingEl = null;
}

function liveStreamDescriptor(method, params, runtime) {
  const itemId = params.itemId || params.item?.id;
  if (!itemId) return null;
  const sourceItem = params.item || runtime.activeItems.get(itemId) || {};
  let type = sourceItem.type || "";
  if (method.includes("reasoning/")) type = "reasoning";
  else if (method.includes("plan/")) type = "plan";
  else if (method.includes("agentMessage") || method === "agentMessage/delta") type = "agentMessage";
  if (!["agentMessage", "plan", "reasoning"].includes(type)) return null;
  let stream = runtime.liveStreams.get(itemId);
  if (!stream) {
    stream = {
      id: itemId,
      threadId: notificationThreadId(params),
      turnId: params.turnId || sourceItem.turnId || runtime.turnId,
      type,
      phase: sourceItem.phase || null,
      rawText: "",
      text: "",
      node: null,
      wrap: null,
      renderTimer: null,
    };
    runtime.liveStreams.set(itemId, stream);
  } else {
    stream.type = type || stream.type;
    stream.phase = sourceItem.phase || stream.phase;
  }
  return stream;
}

function updateLiveStreamFromNotification(method, params, runtime) {
  const stream = liveStreamDescriptor(method, params, runtime);
  if (!stream) return false;
  if (method === "item/reasoning/summaryPartAdded" && stream.text.trim()) {
    stream.rawText += "\n\n";
  }
  const deltaMethods = new Set([
    "item/agentMessage/delta",
    "item/plan/delta",
    "item/reasoning/summaryTextDelta",
    "item/reasoning/textDelta",
    "turn/delta",
    "agentMessage/delta",
  ]);
  if (deltaMethods.has(method)) {
    const delta = params.delta ?? params.text ?? params.chunk ?? "";
    if (typeof delta === "string" && delta) stream.rawText += delta;
  } else if (params.item) {
    const completedText = itemText(params.item);
    if (completedText && completedText.length >= stream.rawText.length) stream.rawText = completedText;
  }
  stream.text = presentConversationText(stream.rawText, "agent", { partial: true });
  if (!stream.text) return true;
  hideTyping();
  scheduleLiveStreamRender(runtime, stream);
  return true;
}

function handleNotification(msg) {
  const params = msg.params || {};
  const threadId = notificationThreadId(params);
  if (!threadId) {
    if (msg.method === "error") {
      updateActivity(params.error?.message || params.message || "Codex 执行失败", { error: true });
    }
    return;
  }
  const runtime = runtimeFor(threadId);
  runtime.lastActivityAt = Date.now();
  runtime.syncIssue = "";
  const visible = threadId === state.selectedThreadId;
  if (msg.method === "turn/started") {
    runtime.turnActive = true;
    runtime.turnId = params.turn?.id || params.turnId || null;
    runtime.pendingRefresh = false;
    runtime.liveStreams.clear();
    runtime.liveProcessDetails = null;
    runtime.liveProcessBody = null;
    runtime.liveProcessCount = 0;
    runtime.turnStartedAt ||= Date.now();
    markThreadRuntimeState(threadId, true);
    if (state.connectionMode === "desktopSync") autoOpenDesktopThread(threadId);
    if (visible) {
      syncSelectedRuntimeUi();
    }
    updateActivity("正在分析任务", { runtime });
    updateGlobalRuntimeState();
    updateSyncStatus();
    return;
  }
  if (msg.method === "turn/completed") {
    if (runtime.queuedRetryTimer) {
      clearTimeout(runtime.queuedRetryTimer);
      runtime.queuedRetryTimer = null;
    }
    runtime.turnActive = false;
    runtime.lastCompletedTurnId = params.turn?.id || params.turnId || runtime.turnId || null;
    runtime.turnId = null;
    for (const stream of runtime.liveStreams.values()) {
      if (stream.renderTimer) {
        clearTimeout(stream.renderTimer);
        stream.renderTimer = null;
      }
    }
    runtime.streamingNode = null;
    runtime.streamingText = "";
    runtime.pendingRefresh = false;
    runtime.activeItems.clear();
    markThreadRuntimeState(threadId, false);
    if (visible) {
      syncSelectedRuntimeUi();
      if (runtime.pendingSubmission?.state !== "queued") {
        refreshSelectedThread().catch(() => {});
      }
    }
    updateActivity("任务已完成", { done: true, runtime });
    runtime.turnStartedAt = 0;
    if (document.visibilityState === "hidden") {
      state.completedWhileHidden = true;
      document.title = "任务已完成 · Mirror X Codex";
    }
    runtime.lastSyncAt = Date.now();
    updateGlobalRuntimeState();
    updateSyncStatus();
    refreshThreads().catch(() => {});
    if (runtime.pendingSubmission?.state === "queued") {
      submitPendingSubmission(threadId, runtime).catch((error) => {
        handleSubmissionFailure(threadId, runtime, error);
      });
    } else if (runtime.pendingSubmission) {
      setTimeout(() => {
        reconcilePendingSubmission(threadId, runtime).catch((error) => {
          handleSubmissionFailure(threadId, runtime, error);
        });
      }, 250);
    }
    return;
  }
  if (msg.method === "error") {
    runtime.turnActive = false;
    runtime.turnId = null;
    runtime.activeItems.clear();
    runtime.turnStartedAt = 0;
    markThreadRuntimeState(threadId, false);
    if (visible) {
      syncSelectedRuntimeUi();
    }
    updateActivity(params.error?.message || params.message || "执行失败", { error: true, runtime });
    if (runtime.pendingSubmission?.state === "queued") {
      scheduleQueuedSubmissionCheck(threadId, 400);
    }
    return;
  }
  if (msg.method === "item/started" && params.item?.id) {
    runtime.activeItems.set(params.item.id, params.item);
    const role = itemRole(params.item);
    if (visible && role === "tool") appendLiveProcessItem(runtime, params.item);
    updateLiveStreamFromNotification(msg.method, params, runtime);
  }
  if (msg.method === "item/completed" && params.item?.id) {
    updateLiveStreamFromNotification(msg.method, params, runtime);
    runtime.activeItems.delete(params.item.id);
  }
  const activity = activityLabel(msg.method, params);
  if (activity) updateActivity(activity, { detail: activityDetail(params.item), runtime });
  updateLiveStreamFromNotification(msg.method, params, runtime);
}

async function refreshSelectedThread() {
  const threadId = state.selectedThreadId;
  if (!threadId || !rpc?.connected) return;
  const runtime = runtimeFor(threadId);
  runtime.syncing = true;
  runtime.syncIssue = "";
  updateSyncStatus();
  try {
    const page = await loadTurnPage(threadId);
    if (threadId !== state.selectedThreadId) return;
    if (runtime.turnActive) {
      runtime.pendingRefresh = true;
      runtime.syncing = false;
      syncSelectedRuntimeUi();
      return;
    }
    setThreadTurnsSnapshot(
      threadId,
      [...(page?.data || [])].reverse(),
      page?.nextCursor || null,
    );
    runtime.pendingRefresh = false;
    runtime.syncing = false;
    runtime.lastSyncAt = Date.now();
    renderTurns(state.threadTurns, {
      scrollMode: conversationRefreshScrollMode(state.followOutput),
    });
    updateSyncStatus();
  } catch (error) {
    runtime.syncing = false;
    runtime.syncIssue = error.message || "同步失败";
    updateSyncStatus();
    console.warn("selected thread refresh failed", error);
  }
}

async function reconcileResumedThread() {
  const threadId = state.selectedThreadId;
  if (!threadId || !rpc?.connected) return;
  try {
    const read = await rpc.call("thread/read", { threadId, includeTurns: false }, 30000);
    if (threadId !== state.selectedThreadId) return;
    const thread = read?.thread || {};
    const status = String(thread?.status?.type || "").toLowerCase();
    if (thread.id) {
      mergeThreads([thread]);
      showConversation(thread);
    }
    const runtime = runtimeFor(threadId);
    const active = threadIsActive(thread);
    if (active) {
      runtime.turnActive = true;
      runtime.turnStartedAt ||= Date.now();
      if (!runtime.turnId) {
        const page = await loadTurnPage(threadId);
        if (threadId !== state.selectedThreadId) return;
        const turns = [...(page?.data || [])].reverse();
        const activeTurn = [...turns].reverse().find(turnIsActive);
        runtime.turnId = activeTurn?.id || activeTurn?.turnId || null;
        setThreadTurnsSnapshot(threadId, turns, page?.nextCursor || null);
        renderTurns(turns, {
          scrollMode: conversationRefreshScrollMode(state.followOutput),
        });
      }
      runtime.pendingRefresh = true;
      syncSelectedRuntimeUi();
    } else if (runtime.turnActive || status === "idle" || status === "notloaded") {
      runtime.turnActive = false;
      runtime.turnId = null;
      runtime.streamingNode = null;
      runtime.streamingText = "";
      runtime.liveStreams.clear();
      runtime.pendingRefresh = false;
      syncSelectedRuntimeUi();
      updateActivity("任务状态已恢复", { done: true });
    }
    if (!runtime.turnActive) await refreshSelectedThread();
    else {
      runtime.pendingRefresh = true;
      syncSelectedRuntimeUi();
    }
    await reconcilePendingSubmission(threadId, runtime);
  } catch (error) {
    console.warn("resumed thread reconciliation failed", error);
    const runtime = runtimeFor(threadId);
    runtime.pendingRefresh = true;
    syncSelectedRuntimeUi();
  }
}

function matchingUserTextCount(turns, expected) {
  const normalized = String(expected || "").trim();
  return (turns || []).reduce((count, turn) => count + (turn.items || []).filter((item) => (
    itemRole(item) === "user" && itemText(item).trim() === normalized
  )).length, 0);
}

function turnContainsSubmissionId(turns, submissionId) {
  if (!submissionId) return false;
  return (turns || []).some((turn) => (turn.items || []).some((item) => (
    item.clientUserMessageId === submissionId
    || item.clientMessageId === submissionId
    || item.id === submissionId
  )));
}

function pendingSubmissionWasDelivered(turns, pending) {
  if (turnContainsSubmissionId(turns, pending.id)) return true;
  if (!String(pending.text || "").trim()) return false;
  const matches = matchingUserTextCount(turns, pending.text);
  const baseline = Number(pending.baselineUserTextCount);
  if (Number.isFinite(baseline) && baseline >= 0) return matches > baseline;
  return matches > 0;
}

async function resolveActiveTurnId(threadId, runtime, { refresh = false } = {}) {
  if (!refresh && runtime.turnId) return runtime.turnId;
  const page = await loadTurnPage(threadId);
  const activeTurn = [...(page?.data || [])].reverse().find(turnIsActive);
  const turnId = activeTurn?.id || activeTurn?.turnId || null;
  if (turnId) runtime.turnId = turnId;
  return turnId;
}

function scheduleQueuedSubmissionCheck(threadId, delayMs = 1500) {
  const runtime = runtimeFor(threadId);
  if (!runtime?.pendingSubmission || runtime.pendingSubmission.state !== "queued") return;
  if (runtime.queuedRetryTimer) return;
  runtime.queuedRetryTimer = setTimeout(async () => {
    runtime.queuedRetryTimer = null;
    if (!runtime.pendingSubmission || runtime.pendingSubmission.state !== "queued") return;
    if (!rpc?.connected || state.reconnecting) {
      scheduleQueuedSubmissionCheck(threadId, 2500);
      return;
    }
    try {
      const read = await rpc.call("thread/read", { threadId, includeTurns: false }, 30000);
      const active = threadIsActive(read?.thread);
      runtime.turnActive = active;
      if (active) {
        if (threadId === state.selectedThreadId) syncSelectedRuntimeUi();
        await submitPendingSubmission(threadId, runtime);
        return;
      }
      runtime.turnId = null;
      if (threadId === state.selectedThreadId) syncSelectedRuntimeUi();
      await submitPendingSubmission(threadId, runtime);
    } catch (error) {
      handleSubmissionFailure(threadId, runtime, error);
      if (runtime.pendingSubmission?.state === "queued") {
        scheduleQueuedSubmissionCheck(threadId, 3000);
      }
    }
  }, delayMs);
}

async function reconcilePendingSubmission(threadId, runtime = runtimeFor(threadId)) {
  const pending = runtime.pendingSubmission || readPendingSubmission(threadId);
  if (!pending) return;
  runtime.pendingSubmission = pending;
  if (threadId === state.selectedThreadId) syncSelectedRuntimeUi();
  if (pending.state === "queued") {
    await submitPendingSubmission(threadId, runtime);
    return;
  }
  const page = await loadTurnPage(threadId);
  const turns = [...(page?.data || [])].reverse();
  if (pendingSubmissionWasDelivered(turns, pending)) {
    clearPendingSubmission(threadId);
    runtime.pendingSubmission = null;
    if (threadId === state.selectedThreadId) {
      setThreadTurnsSnapshot(threadId, turns, page?.nextCursor || null);
      if (!runtime.turnActive) {
        renderTurns(turns, {
          scrollMode: conversationRefreshScrollMode(state.followOutput),
        });
      }
      syncSelectedRuntimeUi();
      showToast("上一条消息已确认送达");
    }
    return;
  }
  if (runtime.turnActive) {
    pending.state = "confirming";
    writePendingSubmission(pending);
    if (threadId === state.selectedThreadId) syncSelectedRuntimeUi();
    return;
  }
  clearPendingSubmission(threadId);
  runtime.pendingSubmission = null;
  if (threadId === state.selectedThreadId) {
    restoreSubmissionToComposer(pending);
    syncSelectedRuntimeUi();
    showToast("未发现已发送记录，内容和附件已恢复");
  }
}

async function startPendingSubmission(
  threadId,
  runtime = runtimeFor(threadId),
  { completedBeforeSteer = false } = {},
) {
  const submission = runtime.pendingSubmission;
  if (!submission || submission.threadId !== threadId) return;
  submission.intent = "start";
  submission.state = "sending";
  writePendingSubmission(submission);
  if (threadId === state.selectedThreadId) syncSelectedRuntimeUi();
  const result = await rpc.call("turn/start", {
    threadId,
    clientUserMessageId: submission.id,
    input: submissionInput(submission),
    approvalPolicy: MOBILE_FULL_ACCESS.approvalPolicy,
    sandboxPolicy: MOBILE_FULL_ACCESS.sandboxPolicy,
  });
  runtime.turnActive = true;
  runtime.turnId = result?.turn?.id || result?.turnId || runtime.turnId;
  runtime.pendingRefresh = true;
  runtime.turnStartedAt ||= Date.now();
  markThreadRuntimeState(threadId, true);
  clearPendingSubmission(threadId);
  runtime.pendingSubmission = null;
  if (runtime.queuedRetryTimer) {
    clearTimeout(runtime.queuedRetryTimer);
    runtime.queuedRetryTimer = null;
  }
  if (threadId === state.selectedThreadId) {
    syncSelectedRuntimeUi();
    if (completedBeforeSteer) {
      updateActivity("上一任务刚结束，消息已作为新任务发送");
      showToast("上一任务刚结束，消息已作为新任务发送");
    } else {
      updateActivity("任务已提交，等待电脑响应");
    }
  }
}

async function steerPendingSubmission(
  threadId,
  runtime = runtimeFor(threadId),
  { retryOnTurnChange = true } = {},
) {
  const submission = runtime.pendingSubmission;
  if (!submission || submission.threadId !== threadId) return;
  let turnId = await resolveActiveTurnId(threadId, runtime);
  if (!turnId) {
    const read = await rpc.call("thread/read", { threadId, includeTurns: false }, 30000);
    if (!threadIsActive(read?.thread)) {
      runtime.turnActive = false;
      runtime.turnId = null;
      return startPendingSubmission(threadId, runtime, { completedBeforeSteer: true });
    }
    throw new Error("正在同步当前任务 ID，暂时无法安全引导，请稍后重试");
  }

  submission.intent = "steer";
  submission.state = "steering";
  submission.expectedTurnId = turnId;
  writePendingSubmission(submission);
  if (threadId === state.selectedThreadId) {
    syncSelectedRuntimeUi();
    updateActivity("正在引导当前任务", { detail: "新要求将追加到正在执行的任务" });
  }

  try {
    const result = await rpc.call("turn/steer", buildTurnSteerParams({
      threadId,
      turnId,
      clientUserMessageId: submission.id,
      input: submissionInput(submission),
    }));
    const acknowledgedTurnId = result?.turnId || turnId;
    const completedBeforeAck = !runtime.turnActive
      && runtime.lastCompletedTurnId === acknowledgedTurnId;
    runtime.turnActive = !completedBeforeAck;
    runtime.turnId = completedBeforeAck ? null : acknowledgedTurnId;
    runtime.pendingRefresh = !completedBeforeAck;
    clearPendingSubmission(threadId);
    runtime.pendingSubmission = null;
    if (runtime.queuedRetryTimer) {
      clearTimeout(runtime.queuedRetryTimer);
      runtime.queuedRetryTimer = null;
    }
    if (threadId === state.selectedThreadId) {
      syncSelectedRuntimeUi();
      if (completedBeforeAck) {
        updateActivity("引导已送达，任务已完成", { done: true });
        showToast("引导已送达，任务随后完成");
        refreshSelectedThread().catch(() => {});
      } else {
        updateActivity("引导已送达", { detail: "Codex 将在当前任务中按新要求继续" });
        showToast("已引导当前任务");
      }
    }
  } catch (error) {
    if (!rpc?.connected || state.reconnecting) throw error;
    let active = true;
    try {
      const read = await rpc.call("thread/read", { threadId, includeTurns: false }, 30000);
      active = threadIsActive(read?.thread);
    } catch {
      throw error;
    }
    if (!active) {
      runtime.turnActive = false;
      runtime.turnId = null;
      return startPendingSubmission(threadId, runtime, { completedBeforeSteer: true });
    }
    if (retryOnTurnChange) {
      const refreshedTurnId = await resolveActiveTurnId(threadId, runtime, { refresh: true });
      if (refreshedTurnId && refreshedTurnId !== turnId) {
        return steerPendingSubmission(threadId, runtime, { retryOnTurnChange: false });
      }
    }
    throw error;
  }
}

async function submitPendingSubmission(threadId, runtime = runtimeFor(threadId)) {
  if (runtime.turnActive) return steerPendingSubmission(threadId, runtime);
  return startPendingSubmission(threadId, runtime);
}

function handleSubmissionFailure(threadId, runtime, error) {
  hideTyping();
  const message = String(error?.message || error || "");
  if (
    runtime?.pendingSubmission
    && runtime.pendingSubmission.intent !== "steer"
    && (message.includes("already has an active writer") || message.includes("turn already running"))
  ) {
    runtime.pendingSubmission.intent = "steer";
    runtime.pendingSubmission.state = "steering";
    writePendingSubmission(runtime.pendingSubmission);
    if (threadId === state.selectedThreadId) {
      runtime.turnActive = true;
      runtime.turnStartedAt ||= Date.now();
      syncSelectedRuntimeUi();
      updateActivity("检测到电脑任务仍在运行", { detail: "正在把本次发送改为引导当前任务" });
    }
    steerPendingSubmission(threadId, runtime).catch((steerError) => {
      handleSubmissionFailure(threadId, runtime, steerError);
    });
    return;
  }
  if (runtime?.pendingSubmission && (!rpc?.connected || state.reconnecting)) {
    runtime.pendingSubmission.state = "confirming";
    writePendingSubmission(runtime.pendingSubmission);
    if (threadId === state.selectedThreadId) {
      syncSelectedRuntimeUi();
      updateActivity("发送状态确认中", { detail: "网络恢复后自动核对，避免重复执行" });
    }
    return;
  }
  const failedSubmission = runtime?.pendingSubmission || null;
  if (failedSubmission) {
    clearPendingSubmission(threadId);
    runtime.pendingSubmission = null;
  }
  if (threadId === state.selectedThreadId) {
    const failureMessage = failedSubmission?.intent === "steer"
      ? turnSteerFailureMessage(error)
      : `发送失败：${error.message}`;
    appendMessage("error", failureMessage);
    if (failedSubmission) restoreSubmissionToComposer(failedSubmission);
    syncSelectedRuntimeUi();
  }
}

async function sendMessage() {
  const input = el("messageInput");
  const text = input.value.trim();
  if ((!text && state.selectedAttachments.length === 0) || !state.selectedThreadId) return;
  if (state.sendInFlight) {
    showToast("上一条消息仍在处理中，请稍候");
    return;
  }
  if (!rpc?.connected || state.reconnecting) {
    showToast("连接正在恢复，内容仍在输入框中");
    syncSelectedRuntimeUi();
    return;
  }
  const sourceThreadId = state.selectedThreadId;
  const sourceRuntime = runtimeFor(sourceThreadId);
  const sourceAttachments = [...state.selectedAttachments];
  if (sourceRuntime.pendingSubmission) {
    showToast("此会话上一条消息仍在发送或确认中");
    syncSelectedRuntimeUi();
    return;
  }
  let targetThreadId = sourceThreadId;
  let targetRuntime = sourceRuntime;
  state.sendInFlight = true;
  syncSelectedRuntimeUi();
  try {
    el("sendBtn").disabled = true;
    targetThreadId = await ensureThreadReady(sourceThreadId);
    targetRuntime = runtimeFor(targetThreadId);
    if (targetRuntime.pendingSubmission) {
      throw new Error("此会话上一条消息仍在发送或确认中");
    }
    const attachments = await prepareSubmissionAttachments(sourceAttachments, sourceThreadId);
    const submission = {
      id: newClientMessageId(),
      threadId: targetThreadId,
      text,
      attachments,
      createdAt: Date.now(),
      state: "sending",
      intent: targetRuntime.turnActive ? "steer" : "start",
      baselineUserTextCount: text && targetThreadId === state.selectedThreadId
        ? matchingUserTextCount(state.threadTurns, text)
        : 0,
    };
    targetRuntime.pendingSubmission = submission;
    writePendingSubmission(submission);
    if (state.selectedThreadId === sourceThreadId || state.selectedThreadId === targetThreadId) {
      input.value = "";
      input.style.height = "auto";
      state.selectedAttachments = [];
      renderAttachmentList();
    }
    clearComposerDraft(sourceThreadId);
    if (targetThreadId !== sourceThreadId) clearComposerDraft(targetThreadId);
    if (state.selectedThreadId === targetThreadId) {
      state.followOutput = true;
      state.newContentPending = false;
      appendMessage("user", submissionPreviewText(text, attachments));
    }
    else showToast("消息将发送到你刚才选择的会话");
    if (!targetRuntime.turnActive) {
      targetRuntime.streamingNode = null;
      targetRuntime.streamingText = "";
      targetRuntime.liveStreams.clear();
    }
    await submitPendingSubmission(targetThreadId, targetRuntime);
  } catch (error) {
    handleSubmissionFailure(targetThreadId, targetRuntime, error);
  } finally {
    state.sendInFlight = false;
    if (state.selectedThreadId) syncSelectedRuntimeUi();
  }
}

async function interruptTurn() {
  if (!state.selectedThreadId || state.interruptInFlight) return;
  const threadId = state.selectedThreadId;
  const runtime = runtimeFor(threadId);
  state.interruptInFlight = true;
  el("interruptBtn").disabled = true;
  el("interruptBtn").textContent = "停止中…";
  try {
    if (!runtime.turnId) {
      const page = await loadTurnPage(threadId);
      const activeTurn = [...(page?.data || [])].reverse().find(turnIsActive);
      runtime.turnId = activeTurn?.id || activeTurn?.turnId || null;
    }
    if (!runtime.turnId) {
      showToast("正在同步当前任务，暂未取得可安全停止的任务 ID");
      await reconcileResumedThread();
      return;
    }
    await rpc.call("turn/interrupt", {
      threadId,
      turnId: runtime.turnId,
    });
    showToast("停止请求已发送");
  } catch (error) {
    appendMessage("error", `停止失败：${error.message}`);
  } finally {
    state.interruptInFlight = false;
    el("interruptBtn").disabled = false;
    el("interruptBtn").textContent = "停止";
  }
}

function persistPairing(bundle) { safeStorage.setItem(STORAGE_KEY, JSON.stringify(bundle)); }
function clearPersistedPairing() {
  safeStorage.removeItem(STORAGE_KEY);
  safeStorage.removeItem(LEGACY_STORAGE_KEY);
}

async function recoverAppServer(reason = "") {
  if (appServerRecovery) return appServerRecovery;
  const now = Date.now();
  const recoveryTimes = (recoverAppServer.attempts || []).filter((time) => now - time < 60000);
  if (recoveryTimes.length >= 3) {
    setStatus("电脑端 Codex 异常", "warn");
    el("sidebarStatus").textContent = "Codex 服务反复退出";
    showToast("电脑端 Codex 服务反复退出，请重启 Mirror X Codex 后重新连接");
    return null;
  }
  recoveryTimes.push(now);
  recoverAppServer.attempts = recoveryTimes;
  appServerRecovery = (async () => {
    state.reconnecting = true;
    el("sidebarStatus").textContent = "Codex 服务正在自动恢复";
    if (reason) console.warn("app-server closed", reason);
    await new Promise((resolve) => setTimeout(resolve, 800));
    if (!rpc || !connection) throw new Error("手机中继已断开");
    const { resumed, mode } = await rpc.openSession();
    setConnectionMode(mode);
    if (!resumed || !rpc.initialized) await rpc.initialize();
    await refreshThreads();
    await reconcileResumedThread();
    state.reconnecting = false;
    setStatus("已连接", "ok");
    el("sidebarStatus").textContent = "Codex 服务已恢复";
    showToast("Codex 服务已自动恢复");
  })().catch((error) => {
    state.reconnecting = false;
    syncSelectedRuntimeUi();
    setStatus("Codex 恢复失败", "warn");
    el("sidebarStatus").textContent = "Codex 服务恢复失败";
    showToast(`自动恢复失败：${error.message}`);
    throw error;
  }).finally(() => {
    appServerRecovery = null;
  });
  return appServerRecovery;
}

function clearBootstrapRetry() {
  if (state.bootstrapRetryTimer) {
    clearTimeout(state.bootstrapRetryTimer);
    state.bootstrapRetryTimer = null;
  }
}

function retryConnection({ automatic = false } = {}) {
  if (!connection) return;
  clearBootstrapRetry();
  el("retryConnectBtn").hidden = true;
  setConnecting(
    automatic ? "正在自动重试…" : "正在重新连接…",
    "重新建立加密通道并读取本机会话",
  );
  if (!state.hasEnteredWorkspace) setPhase("connecting");
  connection.disconnect({ forgetSession: false });
  setTimeout(() => connection?.connect(), 120);
}

async function bootstrapWithKeys(keys, bundle) {
  if (connection || rpc) disconnect({ forget: false });
  persistPairing(bundle);
  connection = new RelayConnection(keys);
  rpc = new AppServerRpc(connection);
  rpc.onNotification(handleNotification);

  connection.on("status", ({ state: current, message }) => {
    if (current === "connecting") {
      setStatus("连接中", "warn");
      if (!state.hasEnteredWorkspace) {
        if (state.initialPairingIssue === "hostOffline") showInitialHostOfflineState();
        else setConnecting("正在连接…", "建立端对端加密通道");
        setPhase("connecting");
      } else {
        state.reconnecting = true;
        el("sidebarStatus").textContent = "网络恢复中";
      }
      syncSelectedRuntimeUi();
    } else if (current === "offline") {
      setStatus("重连中", "warn");
      state.reconnecting = true;
      if (!state.hasEnteredWorkspace) {
        if (state.initialPairingIssue === "hostOffline") showInitialHostOfflineState();
        else setConnecting("连接中断", "正在自动重新连接…");
        setPhase("connecting");
      } else {
        el("sidebarStatus").textContent = "连接中断，正在恢复";
        showToastOnce("network-offline", "网络中断，当前内容已保留，正在自动恢复");
      }
      syncSelectedRuntimeUi();
    } else if (current === "online") {
      setStatus("已连接", "ok");
      state.reconnecting = false;
      setConnectionMode(rpc?.mode);
      syncSelectedRuntimeUi();
    } else if (current === "appServerClosed") {
      setStatus("Codex 服务恢复中", "warn");
      recoverAppServer(message).catch(() => {});
    } else if (current === "error") setStatus(message || "网络错误", "warn");
    updateSyncStatus();
  });

  connection.on("registered", async () => {
    state.initialPairingIssue = "";
    const previousThreadId = state.selectedThreadId || readSelectedThreadId();
    const wasInWorkspace = state.hasEnteredWorkspace;
    if (!wasInWorkspace) setConnecting("正在打开 Codex…", "读取本机会话和项目");
    try {
      const { resumed, mode } = await rpc.openSession();
      setConnectionMode(mode);
      if (!resumed || !rpc.initialized) await rpc.initialize();
      await refreshThreads();
      setPhase("workspace");
      setStatus("已连接", "ok");
      state.reconnecting = false;
      state.bootstrapRetryCount = 0;
      clearBootstrapRetry();
      el("retryConnectBtn").hidden = true;
      if (!resumed || !wasInWorkspace) {
        const previousThread = state.threads.find((thread) => thread.id === previousThreadId);
        const activeThread = state.connectionMode === "desktopSync" ? newestActiveThread() : null;
        const mostRecentThread = state.threads[0] || null;
        if (previousThread) await openThread(previousThread);
        else if (activeThread) await openThread(activeThread);
        else if (mostRecentThread) await openThread(mostRecentThread);
      } else if (previousThreadId) {
        await reconcileResumedThread();
      }
      updateGlobalRuntimeState();
      updateSyncStatus();
    } catch (error) {
      setStatus("初始化失败", "warn");
      state.reconnecting = true;
      syncSelectedRuntimeUi();
      const detail = friendlyBootstrapError(error);
      if (wasInWorkspace) {
        setPhase("workspace");
        el("sidebarStatus").textContent = "初始化失败，可手动重试";
        showToastOnce("workspace-bootstrap-failed", `初始化失败：${detail}`, 8000);
      } else {
        setConnecting("初始化失败", detail);
        setPhase("connecting");
      }
      el("retryConnectBtn").hidden = false;
      state.bootstrapRetryCount += 1;
      if (state.bootstrapRetryCount <= 2) {
        clearBootstrapRetry();
        state.bootstrapRetryTimer = setTimeout(() => {
          state.bootstrapRetryTimer = null;
          retryConnection({ automatic: true });
        }, state.bootstrapRetryCount * 1800);
      }
    }
  });

  connection.on("relayError", ({ code, message }) => {
    if (code === "CLIENT_REPLACED") {
      setStatus("已在另一设备打开", "warn");
      setConnecting("连接已被接管", message || "此连接已被另一台手机或浏览器标签页接管。");
      el("messageInput").disabled = true;
      el("sendBtn").disabled = true;
      el("takeoverBtn").hidden = false;
      showToast("另一台设备已接管，本页面停止自动重连");
      return;
    }
    if (code === "HOST_OFFLINE") {
      setStatus("等待电脑恢复", "warn");
      state.reconnecting = true;
      if (state.hasEnteredWorkspace) {
        setConnecting("等待电脑上线", "配对信息已保留，电脑恢复后会自动连接");
      } else {
        showInitialHostOfflineState();
      }
      if (!state.hasEnteredWorkspace) setPhase("connecting");
      else el("sidebarStatus").textContent = "电脑连接恢复中";
      syncSelectedRuntimeUi();
      showToastOnce(
        "host-offline",
        state.hasEnteredWorkspace
          ? "电脑尚未上线，配对信息已保留并将继续重试"
          : "未找到匹配的电脑，请核对 Key 或电脑端远程状态",
      );
      return;
    }
    if (code === "TOKEN_MISMATCH" || code === "DECRYPT_FAILED") {
      connection.disconnect();
      clearPersistedPairing();
      const detail = "手机与电脑的配对信息不一致，请重新扫码或输入与电脑端一致的 Key。";
      showSetupError(detail);
      setTimeout(() => {
        setPhase("setup");
        el("connectBtn").disabled = el("apiKeyInput").value.trim().length < 5;
      }, 1200);
      return;
    }
    if (state.hasEnteredWorkspace) {
      setStatus(code === "RATE_LIMITED" ? "稍后重连" : "重连中", "warn");
      state.reconnecting = true;
      el("sidebarStatus").textContent = code === "RATE_LIMITED"
        ? "连接频率受限，稍后自动重试"
        : "连接恢复中";
      showToastOnce(
        `relay-${code || "error"}`,
        code === "RATE_LIMITED"
          ? "连接频率受限，当前内容已保留，稍后自动重试"
          : "连接暂时不可用，当前内容已保留",
      );
      return;
    }
    setStatus("连接被拒绝", "warn");
    setConnecting("无法连接", message || code || "连接被拒绝");
    setPhase("connecting");
  });
  connection.connect();
}

async function bootstrapFromApiKey(apiKey) {
  const keys = await deriveKeys(apiKey);
  await bootstrapWithKeys(keys, {
    version: 1,
    roomId: keys.roomId,
    relayToken: keys.relayToken,
    encKey: b64urlEncode(keys.encBytes),
  });
}

async function bootstrapFromBundle(bundle) {
  await bootstrapWithKeys(await restoreKeys(bundle), bundle);
}

function disconnect({ forget = true } = {}) {
  saveComposerDraft();
  clearBootstrapRetry();
  for (const runtime of state.threadRuntime.values()) {
    if (runtime.queuedRetryTimer) clearTimeout(runtime.queuedRetryTimer);
  }
  if (forget) clearSelectedThreadId();
  connection?.disconnect({ forgetSession: forget });
  connection = null;
  rpc = null;
  if (forget) clearPersistedPairing();
  state.fileTreeGeneration += 1;
  closeFileViewerVisual({ restoreFocus: false });
  clearOverlayHistoryState();
  Object.assign(state, {
    threads: [], nextCursor: null, selectedThreadId: null, selectedCwd: "",
    fileRoot: "", streamingNode: null, turnActive: false, drawerOpen: false,
    reconnecting: false, activityLog: [], activeItems: new Map(),
    threadRuntime: new Map(),
    connectionMode: "standalone", desktopActiveThreadId: null, autoOpeningThreadId: null,
    hasEnteredWorkspace: false, sendInFlight: false, interruptInFlight: false,
    bootstrapRetryCount: 0, bootstrapRetryTimer: null,
    initialPairingIssue: "",
    selectedAttachments: [],
    historySyncing: false, lastHistorySyncAt: 0,
    fileTreeLoadingRoot: "", fileTreeLoadedRoot: "", fileTreeLoadPromise: null,
  });
  renderAttachmentList();
  updateGlobalRuntimeState();
  updateSyncStatus();
  setDrawer(false, { pushHistory: false, restoreFocus: false });
  drawerFocusOrigin = null;
  fileViewerFocusOrigin = null;
  setStatus("准备就绪");
  setPhase("setup");
}

function pauseConnection() {
  saveComposerDraft();
  clearBootstrapRetry();
  for (const runtime of state.threadRuntime.values()) {
    if (runtime.queuedRetryTimer) clearTimeout(runtime.queuedRetryTimer);
  }
  connection?.disconnect({ forgetSession: false });
  if (rpc) {
    rpc.connected = false;
    rpc.rejectAll(new Error("手机端已暂时断开"));
  }
  state.reconnecting = true;
  syncSelectedRuntimeUi();
  setStatus("已暂时断开", "warn");
  setConnecting("已暂时断开", "配对和当前草稿已保留，可随时重新连接");
  setPhase("connecting");
  el("retryConnectBtn").hidden = false;
}

function readBootstrapFromLocation() {
  const fragment = location.hash.replace(/^#/, "").trim();
  if (!fragment) return null;
  try {
    const bundle = decodePairingFragment(fragment);
    if (bundle) {
      history.replaceState(null, "", location.pathname + location.search);
      return { kind: "bundle", value: bundle };
    }
  } catch {}
  if (fragment.length > 10) {
    history.replaceState(null, "", location.pathname + location.search);
    try { return { kind: "apiKey", value: decodeURIComponent(fragment) }; } catch {}
  }
  return null;
}

function readStoredBootstrap() {
  const stored = safeStorage.getItem(STORAGE_KEY);
  if (stored) {
    try { return { kind: "bundle", value: JSON.parse(stored) }; } catch { clearPersistedPairing(); }
  }
  const legacy = safeStorage.getItem(LEGACY_STORAGE_KEY);
  if (legacy) {
    try {
      const parsed = JSON.parse(legacy);
      if (parsed?.apiKey) return { kind: "apiKey", value: parsed.apiKey };
    } catch {}
  }
  return null;
}

async function bootstrapFromSeed(seed) {
  if (seed?.kind === "bundle") return bootstrapFromBundle(seed.value);
  if (seed?.kind === "apiKey") return bootstrapFromApiKey(seed.value);
}

function init() {
  clearOverlayHistoryState();
  updateDeviceLayoutMode();
  const scheduleLayoutUpdate = () => {
    updateDeviceLayoutMode();
    setTimeout(updateDeviceLayoutMode, 80);
  };
  window.addEventListener("resize", scheduleLayoutUpdate, { passive: true });
  window.addEventListener("orientationchange", scheduleLayoutUpdate, { passive: true });
  const keyInput = el("apiKeyInput");
  const connectBtn = el("connectBtn");
  el("connectForm").onsubmit = (event) => {
    event.preventDefault();
    if (!connectBtn.disabled) connectBtn.click();
  };
  keyInput.oninput = () => {
    showSetupError("");
    connectBtn.disabled = keyInput.value.trim().length < 5;
  };
  el("eyeBtn").onclick = () => {
    const visible = keyInput.type === "password";
    keyInput.type = visible ? "text" : "password";
    el("eyeBtn").textContent = visible ? "隐藏" : "显示";
  };
  connectBtn.onclick = async () => {
    connectBtn.disabled = true;
    keyInput.type = "password";
    el("eyeBtn").textContent = "显示";
    showSetupError("");
    try {
      await bootstrapFromApiKey(keyInput.value.trim());
    } catch (error) {
      const message = friendlyBootstrapError(error);
      setStatus("启动失败", "warn");
      showSetupError(message);
      showToast(message);
      setPhase("setup");
      connectBtn.disabled = false;
    }
  };
  const bindPointerAction = (node, handler) => {
    let handledAt = 0;
    node.addEventListener("pointerup", (event) => {
      handledAt = Date.now();
      event.preventDefault();
      event.stopPropagation();
      handler();
    });
    node.addEventListener("click", (event) => {
      if (Date.now() - handledAt < 500) return;
      event.preventDefault();
      event.stopPropagation();
      handler();
    });
  };
  bindPointerAction(el("menuBtn"), toggleDrawer);
  bindPointerAction(el("closeMenuBtn"), closeDrawer);
  bindPointerAction(el("drawerBackdrop"), closeDrawer);
  el("retryConnectBtn").onclick = () => retryConnection();
  el("changeKeyBtn").onclick = () => {
    if (!window.confirm("退出并清除本机保存的配对？之后需要重新扫码或输入 Key 才能连接。")) return;
    disconnect({ forget: true });
    keyInput.value = "";
    connectBtn.disabled = true;
    requestAnimationFrame(() => keyInput.focus());
  };
  document.querySelectorAll(".nav-tab").forEach((button) => {
    button.onclick = () => setActivePanel(button.dataset.panel);
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && !el("fileViewer").hidden) {
      event.preventDefault();
      closeFileViewer();
      return;
    }
    if (event.key === "Escape" && state.drawerOpen) {
      event.preventDefault();
      closeDrawer();
      return;
    }
    trapOverlayFocus(event);
  });
  window.addEventListener("popstate", () => {
    applyOverlayHistoryState().catch((error) => {
      console.warn("overlay history restore failed", error);
      closeFileViewerVisual({ restoreFocus: false });
      applyDrawerState(false);
    });
  });
  let drawerTouchStartX = null;
  el("sidebar").addEventListener("touchstart", (event) => {
    drawerTouchStartX = event.touches?.[0]?.clientX ?? null;
  }, { passive: true });
  el("sidebar").addEventListener("touchend", (event) => {
    const endX = event.changedTouches?.[0]?.clientX;
    if (drawerTouchStartX !== null && endX !== undefined && endX - drawerTouchStartX < -55) {
      closeDrawer();
    }
    drawerTouchStartX = null;
  }, { passive: true });
  el("threadSearch").oninput = renderHistory;
  el("refreshBtn").onclick = () => refreshThreads().catch((error) => showToast(error.message));
  el("syncStatusBtn").onclick = () => {
    refreshSelectedThread()
      .then(() => refreshThreads())
      .catch((error) => showToast(`同步失败：${error.message}`));
  };
  el("loadMoreBtn").onclick = loadMoreThreads;
  el("reloadFilesBtn").onclick = () => (
    state.fileRoot && selectProject(state.fileRoot, true, { force: true })
  );
  el("newThreadBtn").onclick = () => startThread();
  el("disconnectBtn").onclick = pauseConnection;
  el("closeFileBtn").onclick = () => closeFileViewer();
  el("fileSourceBtn").onclick = toggleFileSource;
  el("interruptBtn").onclick = interruptTurn;
  el("takeoverBtn").onclick = () => {
    el("takeoverBtn").hidden = true;
    connection?.connect();
  };

  const input = el("messageInput");
  const attachmentInput = el("attachmentInput");
  el("attachmentBtn").onclick = () => attachmentInput.click();
  attachmentInput.onchange = () => {
    addSelectedAttachments(attachmentInput.files);
    attachmentInput.value = "";
  };
  input.oninput = () => {
    input.style.height = "auto";
    input.style.height = `${Math.min(input.scrollHeight, 150)}px`;
    saveComposerDraft();
    const runtime = selectedRuntime();
    el("sendBtn").disabled = !hasComposerContent()
      || !state.selectedThreadId
      || !rpc?.connected
      || state.reconnecting
      || state.sendInFlight
      || Boolean(runtime?.pendingSubmission);
  };
  const settleFocusedComposer = () => {
    updateVisualViewportMetrics();
    requestAnimationFrame(updateVisualViewportMetrics);
    setTimeout(updateVisualViewportMetrics, 80);
    setTimeout(updateVisualViewportMetrics, 240);
  };
  input.addEventListener("focus", () => {
    closeDrawer();
    settleFocusedComposer();
  });
  input.addEventListener("blur", () => {
    setTimeout(() => {
      document.documentElement.classList.remove("keyboard-open");
      updateVisualViewportMetrics();
    }, 120);
  });
  input.onkeydown = (event) => {
    if (shouldSubmitComposerKey(event)) {
      event.preventDefault();
      if (!el("sendBtn").disabled) sendMessage();
    }
  };
  el("sendBtn").onclick = sendMessage;
  el("jumpLatestBtn").onclick = jumpToLatest;
  el("messages").addEventListener("scroll", () => {
    const nearBottom = messagesNearBottom();
    state.followOutput = nearBottom;
    if (nearBottom) state.newContentPending = false;
    updateJumpLatestButton();
  }, { passive: true });
  el("activityDetailsBtn").onclick = () => {
    const details = el("activityDetails");
    details.hidden = !details.hidden;
    el("activityDetailsBtn").setAttribute("aria-expanded", String(!details.hidden));
    el("activityDetailsBtn").textContent = details.hidden ? "查看详情" : "收起详情";
  };

  if (window.visualViewport) {
    window.visualViewport.addEventListener("resize", updateVisualViewportMetrics);
    window.visualViewport.addEventListener("scroll", updateVisualViewportMetrics);
  }
  updateVisualViewportMetrics();
  setInterval(() => {
    updateSyncStatus();
    if (!el("activityDetails").hidden) renderActivityDetails();
    if (state.lastHistorySyncAt && !state.historySyncing) {
      const minutes = Math.floor((Date.now() - state.lastHistorySyncAt) / 60000);
      if (minutes > 0 && state.historyMode === "complete") {
        setHistoryNotice(`${minutes} 分钟前同步 · ${state.threads.length} 个会话`);
      }
    }
  }, 15000);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible" && rpc?.connected) {
      if (state.completedWhileHidden) {
        state.completedWhileHidden = false;
        document.title = "Mirror X Codex";
        showToast("任务已完成，已同步最新结果");
      }
      if (!state.turnActive) refreshSelectedThread();
      else state.pendingThreadRefresh = true;
      refreshThreads().catch(() => {});
    }
  });

  const seed = readBootstrapFromLocation() || readStoredBootstrap();
  if (seed?.kind === "apiKey") {
    keyInput.value = seed.value;
    connectBtn.disabled = false;
  }
  if (seed) bootstrapFromSeed(seed).catch((error) => {
    const message = friendlyBootstrapError(error);
    setStatus("启动失败", "warn");
    showSetupError(message);
    showToast(message);
    setPhase("setup");
  });
  window.addEventListener("hashchange", () => {
    const updated = readBootstrapFromLocation();
    if (updated) bootstrapFromSeed(updated).catch((error) => showToast(error.message));
  });
}

if (typeof document !== "undefined") init();
