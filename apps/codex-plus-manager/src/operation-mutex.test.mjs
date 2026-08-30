import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./SimpleApp.tsx", import.meta.url), "utf8");

assert.match(source, /const operationToken = useRef<symbol \| null>\(null\);/);
assert.match(source, /if \(operationToken\.current\) return null;/);
assert.match(source, /if \(operationToken\.current !== token\) return;/);
assert.match(source, /operationToken\.current = null;\s+setBusy\(null\);/);
assert.doesNotMatch(
  source,
  /finally \{\s+setBusy\(null\);/,
  "async operations must only unlock through their matching token",
);
assert.match(source, /<fieldset[\s\S]*disabled=\{busy !== null\}/);
assert.match(source, /await refresh\(token\);/);

for (const operation of [
  "validate-image",
  "enable",
  "repair",
  "restore",
  "recover-baseline",
  "launch",
  "install-codex",
  "choose-codex",
]) {
  assert.match(source, new RegExp(`beginOperation\\(.*[\"']${operation}`));
}
assert.match(source, /beginOperation\(`validate-\$\{groupId\}`\)/);
assert.doesNotMatch(source, /account-read|account-create-missing|load_mirror_account_keys/);

console.log("SimpleApp operation mutex contracts passed");
