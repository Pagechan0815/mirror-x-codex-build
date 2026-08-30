import assert from "node:assert/strict";
import test from "node:test";

import {
  getApplyBlockReason,
  getSandboxRecoveryAction,
  isCodexLaunchReady,
  type ApplyGuardInput,
  type GuardPreflight,
} from "./simple-app-guards.ts";

const readyPreflight: GuardPreflight = {
  ready: true,
  codexInstalled: true,
  codexAppPath: "C:/Program Files/WindowsApps/OpenAI.Codex",
  checks: [{ id: "codex", ready: true, label: "Codex Desktop", detail: "可启动" }],
};

function validInput(overrides: Partial<ApplyGuardInput> = {}): ApplyGuardInput {
  return {
    busy: false,
    stateUnreadable: false,
    baselineExists: false,
    preflight: readyPreflight,
    sandboxSupported: true,
    sandboxReady: true,
    sandboxStatus: "ready",
    mode: "mixedApi",
    mixedAuthReady: true,
    mixedAuthMessage: "已检测到 ChatGPT 登录",
    groups: [
      {
        label: "CodexPro Key",
        apiKey: "sk-valid",
        verified: true,
        selectedModelIds: ["gpt-5.4"],
      },
    ],
    defaultModel: "gpt-5.4",
    imagegenEnabled: false,
    imagegenConfigured: false,
    imageApiKey: "",
    imageKeyValidated: false,
    ...overrides,
  };
}

test("a fully verified configuration can be applied", () => {
  assert.equal(getApplyBlockReason(validInput()), null);
});

test("unreadable state takes priority and keeps writes blocked", () => {
  const reason = getApplyBlockReason(validInput({ stateUnreadable: true, preflight: null }));
  assert.match(reason ?? "", /接管状态无法读取/);

  const recoverable = getApplyBlockReason(validInput({
    stateUnreadable: true,
    baselineExists: true,
  }));
  assert.match(recoverable ?? "", /还原点灾难恢复/);
});

test("preflight failure exposes the concrete failing check", () => {
  const reason = getApplyBlockReason(validInput({
    preflight: {
      ...readyPreflight,
      ready: false,
      checks: [{ id: "storage", ready: false, label: "磁盘空间", detail: "剩余空间不足" }],
    },
  }));
  assert.equal(reason, "本机体检未通过：磁盘空间 - 剩余空间不足");
});

test("a filled but no longer verified key is blocked", () => {
  const reason = getApplyBlockReason(validInput({
    groups: [{
      label: "企业GPT专线",
      apiKey: "sk-replaced",
      verified: false,
      selectedModelIds: [],
    }],
  }));
  assert.match(reason ?? "", /请先验证 企业GPT专线/);
});

test("Windows sandbox diagnostics never block normal access", () => {
  for (const diagnostic of [
    { sandboxSupported: null },
    { sandboxReady: false },
    { sandboxReady: false, sandboxStatus: "policy_blocked" },
    { sandboxReady: false, sandboxStatus: "update_required" },
    { sandboxSupported: false, sandboxReady: false, sandboxStatus: "unsupported_platform" },
  ]) {
    assert.equal(getApplyBlockReason(validInput(diagnostic)), null);
  }
});

test("sandbox recovery distinguishes Codex updates from environment setup", () => {
  assert.equal(getSandboxRecoveryAction("update_required", "codex_app"), "update_codex");
  assert.equal(getSandboxRecoveryAction("update_required", "sandbox_environment"), "enable");
  assert.equal(getSandboxRecoveryAction("not_configured", null), "enable");
  assert.equal(getSandboxRecoveryAction("full_access_not_configured", null), "enable");
  assert.equal(getSandboxRecoveryAction("sandbox_state_invalid", null), "enable");
  assert.equal(getSandboxRecoveryAction("policy_blocked", null), null);
  assert.equal(getSandboxRecoveryAction("check_failed", null), null);
});

test("mixed API fails closed without confirmed ChatGPT login", () => {
  const checking = getApplyBlockReason(validInput({ mixedAuthReady: null }));
  assert.match(checking ?? "", /正在确认.*ChatGPT/);

  const signedOut = getApplyBlockReason(validInput({
    mixedAuthReady: false,
    mixedAuthMessage: "请先登录 ChatGPT",
  }));
  assert.equal(signedOut, "请先登录 ChatGPT");

  assert.equal(getApplyBlockReason(validInput({
    mode: "pureApi",
    mixedAuthReady: false,
    mixedAuthMessage: "请先登录 ChatGPT",
  })), null);
});

test("duplicate models and an invalid default model are explained", () => {
  const duplicate = getApplyBlockReason(validInput({
    groups: [
      { label: "CodexPro", apiKey: "sk-a", verified: true, selectedModelIds: ["gpt-5.4"] },
      { label: "企业GPT专线", apiKey: "sk-b", verified: true, selectedModelIds: ["gpt-5.4"] },
    ],
  }));
  assert.match(duplicate ?? "", /不能同时分配/);

  const missingDefault = getApplyBlockReason(validInput({ defaultModel: "gpt-unknown" }));
  assert.match(missingDefault ?? "", /默认模型/);
});

test("image access requires either the saved key or a newly validated key", () => {
  const missing = getApplyBlockReason(validInput({ imagegenEnabled: true }));
  assert.match(missing ?? "", /Image Key/);

  const unverified = getApplyBlockReason(validInput({
    imagegenEnabled: true,
    imageApiKey: "sk-image",
  }));
  assert.match(unverified ?? "", /请先确认/);

  assert.equal(getApplyBlockReason(validInput({
    imagegenEnabled: true,
    imageApiKey: "sk-image",
    imageKeyValidated: true,
  })), null);
});

test("Codex launch requires a launchable path confirmed by preflight", () => {
  assert.equal(isCodexLaunchReady(readyPreflight), true);
  assert.equal(isCodexLaunchReady({ ...readyPreflight, codexInstalled: false }), false);
  assert.equal(isCodexLaunchReady({ ...readyPreflight, codexAppPath: null }), false);
  assert.equal(isCodexLaunchReady({ ...readyPreflight, checks: [] }), false);
});
