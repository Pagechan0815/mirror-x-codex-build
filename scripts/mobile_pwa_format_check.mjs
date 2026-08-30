import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  buildTurnSteerParams,
  collabAgentSummary,
  conversationRefreshScrollMode,
  createSafeStorage,
  filePreviewKind,
  MOBILE_FULL_ACCESS,
  normalizeMessageSyntax,
  presentConversationText,
  resolveLocalFilePath,
  shouldSubmitComposerKey,
  splitMarkdownTableRow,
  turnTimelineSegments,
  turnSteerFailureMessage,
} from "../apps/codex-plus-mobile-relay/pwa/app.js";

const appSource = readFileSync(
  new URL("../apps/codex-plus-mobile-relay/pwa/app.js", import.meta.url),
  "utf8",
);
const indexSource = readFileSync(
  new URL("../apps/codex-plus-mobile-relay/pwa/index.html", import.meta.url),
  "utf8",
);
assert.match(
  appSource,
  /sidebar\.inert = mobile && \(!state\.drawerOpen \|\| fileOpen\);/,
  "closed mobile navigation must not remain keyboard-focusable",
);
assert.doesNotMatch(
  appSource,
  /(?<!globalThis\.)localStorage\.(?:getItem|setItem|removeItem)/,
  "PWA storage access must go through the resilient wrapper",
);
assert.match(appSource, /el\("disconnectBtn"\)\.onclick = pauseConnection;/);
assert.match(appSource, /window\.confirm\("退出并清除本机保存的配对/);
assert.match(appSource, /connection\?\.disconnect\(\{ forgetSession: false \}\);/);
assert.match(appSource, /rpc\.rejectAll\(new Error\("手机端已暂时断开"\)\);/);
assert.match(indexSource, /id="disconnectBtn"[^>]*>暂时断开<\/button>/);
assert.match(indexSource, /id="changeKeyBtn"[^>]*>退出并清除配对<\/button>/);

const fallbackEvents = [];
const throwingStorage = {
  getItem() { throw new Error("blocked"); },
  setItem() { throw new Error("quota"); },
  removeItem() { throw new Error("blocked"); },
};
const resilientStorage = createSafeStorage(
  throwingStorage,
  (error) => fallbackEvents.push(error.message),
);
assert.doesNotThrow(() => resilientStorage.setItem("pairing", "saved-in-memory"));
assert.equal(resilientStorage.getItem("pairing"), "saved-in-memory");
assert.doesNotThrow(() => resilientStorage.removeItem("pairing"));
assert.equal(resilientStorage.getItem("pairing"), null);
assert.deepEqual(fallbackEvents, ["quota"], "storage fallback notice must be non-blocking and shown once");
assert.equal(shouldSubmitComposerKey({ key: "Enter", shiftKey: false, isComposing: false, keyCode: 13 }), true);
assert.equal(shouldSubmitComposerKey({ key: "Enter", shiftKey: false, isComposing: true, keyCode: 13 }), false);
assert.equal(shouldSubmitComposerKey({ key: "Enter", shiftKey: false, isComposing: false, keyCode: 229 }), false);
assert.equal(shouldSubmitComposerKey({ key: "Enter", shiftKey: true, isComposing: false, keyCode: 13 }), false);
assert.match(
  appSource,
  /el\("interruptBtn"\)\.hidden = !state\.turnActive \|\| transportUnavailable;/,
  "offline snapshots must not expose a live stop action",
);
assert.match(
  appSource,
  /const visibleCount = transportUnavailable \? 0 : count;/,
  "offline snapshots must not be presented as currently running tasks",
);
assert.match(
  appSource,
  /transportUnavailable \? "断线前执行中" : "执行中"/,
  "offline active-thread labels must be identified as stale snapshots",
);

const assistantHeartbeat = `
<heartbeat>
<automation_id>30</automation_id>
<decision>DONT_NOTIFY</decision>
<message>任务已完成，终审结果待确认。</message>
</heartbeat>`;

const userHeartbeat = `
<heartbeat>
<automation_id>30</automation_id>
<current_time_iso>2026-08-15T06:55:21.558Z</current_time_iso>
<instructions>每30分钟执行一次 FIRST-AGENT 经营总控。</instructions>
</heartbeat>`;

assert.equal(
  presentConversationText(assistantHeartbeat, "agent"),
  "任务已完成，终审结果待确认。",
);
assert.equal(
  presentConversationText(userHeartbeat, "user"),
  "每30分钟执行一次 FIRST-AGENT 经营总控。",
);
assert.equal(
  presentConversationText(
    "<codex_delegation><source_thread_id>thread</source_thread_id><input>继续修复手机端</input></codex_delegation>",
    "user",
  ),
  "继续修复手机端",
);
assert.equal(
  presentConversationText(
    "<heartbeat><automation_id>30</automation_id><decision>DONT_NOTIFY</decision></heartbeat>",
    "agent",
  ),
  "",
);
assert.equal(
  presentConversationText(
    "<heartbeat><decision>DONT_NOTIFY</decision><message>正在生成",
    "agent",
    { partial: true },
  ),
  "正在生成",
);
assert.equal(
  presentConversationText("请解释普通文本里的 <heartbeat> 标签", "agent"),
  "请解释普通文本里的 <heartbeat> 标签",
);
assert.equal(
  presentConversationText("<heartbeat><custom>普通 XML</custom></heartbeat>", "agent"),
  "<heartbeat><custom>普通 XML</custom></heartbeat>",
);
assert.equal(
  normalizeMessageSyntax("[!image]\n[！图片]"),
  "[图片附件]\n[图片附件]",
);
assert.equal(filePreviewKind("D:\\project\\README.md"), "markdown");
assert.equal(filePreviewKind("/tmp/preview.PNG"), "image");
assert.equal(filePreviewKind("demo.mp4"), "video");
assert.equal(filePreviewKind("archive.zip"), "binary");
assert.equal(
  resolveLocalFilePath("images/cover.png", "D:\\project"),
  "D:\\project\\images/cover.png",
);
assert.equal(
  resolveLocalFilePath("<D:\\project files\\cover.png>"),
  "D:\\project files\\cover.png",
);
assert.equal(resolveLocalFilePath("javascript:alert(1)", "D:\\project"), "");
assert.deepEqual(splitMarkdownTableRow("| 名称 | a\\|b | 状态 |"), ["名称", "a|b", "状态"]);
assert.deepEqual(splitMarkdownTableRow("| 路径 | D:\\project\\file.md |"), ["路径", "D:\\project\\file.md"]);
assert.equal(
  collabAgentSummary({ type: "collabAgentToolCall", taskName: "核对公网版本", status: "completed" }),
  "子 Agent 已完成：核对公网版本",
);
assert.equal(MOBILE_FULL_ACCESS.approvalPolicy, "never");
assert.equal(MOBILE_FULL_ACCESS.sandbox, "dangerFullAccess");
assert.deepEqual(MOBILE_FULL_ACCESS.sandboxPolicy, { type: "dangerFullAccess" });
assert.equal(conversationRefreshScrollMode(true), "bottom");
assert.equal(conversationRefreshScrollMode(false), "retain");
assert.deepEqual(
  turnTimelineSegments({
    status: "completed",
    items: [
      { id: "u1", type: "userMessage", content: [{ type: "text", text: "修复同步" }] },
      { id: "a1", type: "agentMessage", phase: "commentary", text: "先检查链路" },
      { id: "r1", type: "reasoning", summary: ["定位到重复重绘"], content: [] },
      { id: "c1", type: "commandExecution", command: "cargo test", aggregatedOutput: "ok" },
      { id: "a2", type: "agentMessage", phase: "final_answer", text: "同步已修复" },
    ],
  }).map((segment) => ({
    kind: segment.kind,
    ids: segment.items.map((item) => item.id),
  })),
  [
    { kind: "user", ids: ["u1"] },
    { kind: "process", ids: ["a1", "r1", "c1"] },
    { kind: "final", ids: ["a2"] },
  ],
);
assert.deepEqual(
  turnTimelineSegments({
    status: { type: "interrupted" },
    items: [
      { id: "stopped-commentary", type: "agentMessage", phase: "commentary", text: "执行到一半" },
      { id: "stopped-command", type: "commandExecution", command: "check", aggregatedOutput: "" },
    ],
  }).map((segment) => ({
    kind: segment.kind,
    ids: segment.items.map((item) => item.id),
  })),
  [
    { kind: "process", ids: ["stopped-commentary", "stopped-command"] },
  ],
);
assert.deepEqual(
  turnTimelineSegments({
    status: { type: "inProgress" },
    items: [
      { id: "active-commentary", type: "agentMessage", phase: "commentary", text: "继续检查" },
      { id: "active-reasoning", type: "reasoning", summary: ["仍在执行"], content: [] },
    ],
  }).map((segment) => ({
    kind: segment.kind,
    ids: segment.items.map((item) => item.id),
  })),
  [
    { kind: "process", ids: ["active-commentary", "active-reasoning"] },
  ],
);
assert.deepEqual(
  turnTimelineSegments({
    status: "completed",
    items: [
      { id: "legacy-progress", type: "agentMessage", text: "正在检查" },
      { id: "legacy-tool", type: "fileChange", changes: [] },
      { id: "legacy-final", type: "agentMessage", text: "处理完成" },
    ],
  }).map((segment) => ({
    kind: segment.kind,
    ids: segment.items.map((item) => item.id),
  })),
  [
    { kind: "process", ids: ["legacy-progress", "legacy-tool"] },
    { kind: "final", ids: ["legacy-final"] },
  ],
);
assert.deepEqual(
  buildTurnSteerParams({
    threadId: "thread-active",
    turnId: "turn-active",
    clientUserMessageId: "mobile-guidance-1",
    input: [{ type: "text", text: "先修复登录问题" }],
  }),
  {
    threadId: "thread-active",
    expectedTurnId: "turn-active",
    clientUserMessageId: "mobile-guidance-1",
    input: [{ type: "text", text: "先修复登录问题" }],
  },
);
assert.throws(
  () => buildTurnSteerParams({ threadId: "thread-active", turnId: "", input: [] }),
  /当前任务 ID/,
);
assert.match(
  turnSteerFailureMessage({
    message: "active turn cannot accept same-turn steering",
    data: { codexErrorInfo: { activeTurnNotSteerable: { turnKind: "compact" } } },
  }),
  /压缩或审查阶段/,
);
assert.match(
  turnSteerFailureMessage({ code: -32601, message: "Method not found" }),
  /Codex 版本不支持/,
);

console.log("mobile PWA message format checks passed");
