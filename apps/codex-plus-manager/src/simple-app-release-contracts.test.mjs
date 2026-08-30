import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./SimpleApp.tsx", import.meta.url), "utf8");
const tauriLibSource = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");

const validationStart = source.indexOf("const validate = async");
const resetBeforeValidation = source.indexOf("clearGroupValidation(groupId);", validationStart);
const keyValidationInvoke = source.indexOf('"validate_mirror_key"', validationStart);
assert.ok(validationStart >= 0 && resetBeforeValidation > validationStart);
assert.ok(resetBeforeValidation < keyValidationInvoke, "old discovery must be cleared before validating again");
assert.ok(
  source.match(/clearGroupValidation\(groupId\);/g)?.length >= 3,
  "start, command failure, and thrown failure must all invalidate the group",
);

assert.match(source, /ref=\{noticeRef\}/);
assert.match(source, /scrollIntoView\(\{ behavior: "smooth", block: "center" \}\)/);
assert.match(source, /getApplyBlockReason\(\{/);
assert.match(source, /disabled=\{applyBlockReason !== null\}/);
assert.doesNotMatch(source, /\?view=advanced|高级管理|高级诊断/);

assert.match(source, /"get_mirror_mixed_auth_status"/);
assert.match(source, /mixedAuthReady: mixedAuth\?\.ready \?\? null/);
assert.match(source, /replaceExistingGroups: true/);

assert.doesNotMatch(
  source,
  /"get_windows_sandbox_diagnostic"|"enable_windows_sandbox_access"/,
  "normal access must not trigger the official Windows Sandbox setup workflow",
);
assert.match(tauriLibSource, /codex_setup::update_codex_desktop/);

assert.match(source, /"recover_mirror_from_baseline"/);
assert.match(source, /setBaselineRecoveryConfirmOpen\(true\)/);
assert.match(source, /从首次接入还原点灾难恢复/);

const launchStart = source.indexOf("const launch = async");
const launchPreflight = source.indexOf('"get_mirror_preflight"', launchStart);
const launchCommand = source.indexOf('"launch_codex_plus"', launchStart);
assert.ok(launchPreflight > launchStart && launchPreflight < launchCommand);
assert.match(source.slice(launchStart, launchCommand), /isCodexLaunchReady\(latestPreflight\)/);
assert.match(source.slice(launchStart, launchCommand), /latestPreflight\?\.codexRunning === true/);
assert.match(source, /result\.status === "degraded" \? "info"/);

console.log("SimpleApp public release contracts passed");
