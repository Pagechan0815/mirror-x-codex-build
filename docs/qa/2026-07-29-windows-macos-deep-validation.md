# Mirror X Codex Windows/macOS Deep Validation

Date: 2026-07-29

Commit under test: the local commit containing this report (not yet pushed because GitHub write access is unavailable)

## 1. Product Goal Understanding

Mirror X Codex is a boundary tool for novice customers who already have, or can install, the official Codex desktop application. A customer supplies one or more Mirror relay keys, selects the models available to each key, and enables either mixed API or pure API access. The tool must preserve MCP and plugin marketplace configuration, repair the default session environment, and restore the exact pre-use state with one action.

Commercial success requires:

- One-key setup for a single relay group and clear multi-key setup when model groups differ.
- GPT/Grok and Claude keys remain isolated and route only their selected models.
- Selected relay models appear in the Codex model selector.
- Existing MCP, plugins, unknown TOML fields, sessions, and ChatGPT auth survive mixed mode.
- Pure API mode uses API-key auth without corrupting the previous state.
- Restore is deterministic, tamper-aware, and resilient to partial writes.
- Windows x64, macOS Intel, and macOS Apple Silicon packages install and start.

## 2. User Journey Map

1. Preflight checks whether Codex is installed and whether its configuration is parseable.
2. The user enters a CodexPro key and optionally a separate Claude key.
3. Each key independently requests `/v1/models` from the configured relay.
4. The user selects at least one model per supplied key and chooses a default from the selected set.
5. Mirror X Codex creates an immutable baseline and writes grouped relay profiles atomically.
6. Codex starts with the repaired `CODEX_HOME`; model injection exposes selected models.
7. MCP and plugin marketplace remain available according to the chosen mode.
8. Restore validates the baseline and returns files/auth/session state to the pre-use state.

## 3. Validation Environment

- Windows 10/11 x64 host.
- Official Codex Store build `26.721.4979.0`.
- Real renderer assets: `index-DqK89hOt.js`, `app-initial-BbEVL4-_.js`, `app-main-CAoq-qgz.js`, `rpc-CDAeVAJt.js`.
- Relay base URL: `https://api.jingziai.club`.
- No production customer key was used. Anonymous `/v1/models` and `/v1/responses` correctly returned HTTP 401.
- Historical GitHub Actions run `30082044033`, commit `e526468`, covered Windows, macOS x64, and macOS arm64.

## 4. Core Findings

### Fixed during this validation

Codex `26.721` no longer exposes the old `app-server-manager-signals-*` request client used by the upstream injection. Module-export scanning found no usable `sendRequest` object, so the upstream fallback could not prove model-list interception.

The implementation now:

- Supports the public `app-initial` dispatcher and `vscode://codex/*` fetch envelopes.
- Discovers the real `AppServerRequestClient` inside `window.__codexRoot` React scope bindings.
- Normalizes `model/list` to `list-models-for-host`.
- Tracks fetch responses by request ID and leaves unrelated responses untouched.
- Keeps the previous module-export client scan as a compatibility fallback.
- Uses a current-build React-scope fast path before the generic graph scan.

Runtime proof against the real Codex renderer found six request clients. A temporary, automatically restored client patch inserted a proof model into a real `list-models-for-host` response: model count changed from 8 to 9 and the proof model was returned. No test model or method patch remained afterward.

The generic graph scan measured about 60.7 ms on the current renderer. The new React-scope fast path measured about 17 ms and found the three core clients, including the real `v9t` AppServer request client.

## 5. Test Matrix

| Area | Normal | Abnormal / Boundary | Result |
| --- | --- | --- | --- |
| Relay key validation | Each supplied key fetches its own model list | Empty, unauthorized, malformed model list rejected | Pass |
| Group routing | CodexPro and Claude profiles retain separate keys/models | Duplicate model across keys rejected | Pass |
| Model selection | Selected models and default model persisted | Empty group and default outside selection rejected | Pass |
| Current Codex injection | Real `26.721` client found and patched | Unrelated fetch request IDs ignored | Pass |
| Mixed API | ChatGPT auth preserved | Missing/invalid existing config rejected before write | Pass |
| Pure API | API-key auth selected | Partial write restores operation-start state | Pass |
| MCP/TOML | MCP and unknown TOML fields preserved | Existing malformed TOML not overwritten | Pass |
| Baseline/restore | Original files restored | Baseline overwrite and tampering refused | Pass |
| Session repair | Configured `CODEX_HOME` synchronized | Re-injection retains launcher bridge context | Pass |
| Windows package | Release build and isolated manager startup | Separate guard port avoids existing instance | Pass |
| macOS scripts | Package/verify scripts parse with `bash -n` | DMG runtime not executed for current commit | Partial |
| Mobile | Not a mobile product | N/A | N/A |

## 6. Automated Evidence

- `cargo test --workspace`: pass.
- Core unit tests: 136 pass.
- CDP/injection integration tests: 68 pass.
- Model-window Node tests: 9 pass.
- `npm run check`: pass.
- `npm run vite:build`: pass.
- `node --check assets/inject/renderer-inject.js`: pass.
- `cargo build --release --workspace`: pass.
- Windows manager isolated smoke: alive after 8 seconds, guard port listening.
- macOS `package-dmg.sh` and `verify-dmg.sh`: `bash -n` pass.

Windows release SHA256:

- Launcher: `5BBBA6380EBFAEDC2746B48513A9556AB02061B128928A21F57907D0F982F938`
- Manager: `97895CE78D37D97CA9DB5484B6997CD84664A4795C0BA3570AF52D8DB693A85B`

## 7. Destructive and Novice Scenarios

- Baseline is created once and cannot be silently replaced.
- Baseline integrity is checked before restore.
- Invalid existing config is reported instead of overwritten.
- Atomic write failure restores the state captured at operation start.
- Mixed mode does not delete ChatGPT auth.
- A novice cannot select a default model that was not selected for the key.
- A model cannot be ambiguously assigned to two keys.
- Missing Codex must lead to an official installation path rather than attempting injection.

## 8. Bug and Risk Grading

### P1 - Release blocker: current macOS artifacts are not verified

The historical x64/arm64 jobs proved the packaging pipeline, architecture check, ad-hoc signature, DMG mount, and manager startup for commit `e526468`. They do not prove the current model-injection commit. The current GitHub PAT has read-only access; branch push and workflow dispatch both returned HTTP 403.

Required acceptance evidence: run `pr-build.yml` for the current commit on `macos-15-intel` and `macos-14`, with both DMG verification jobs green.

### P1 - Commercial distribution trust

The Windows binaries are unsigned. macOS uses ad-hoc signing, not Apple notarization. Users can run them with operating-system bypass steps, but SmartScreen/Gatekeeper warnings and reputation risk remain. This is acceptable for controlled beta distribution, not zero-friction public commercial distribution.

### P1 - Credential exposure

Repository/GitHub, server, relay-admin, and other credentials shared during development must be rotated before commercial release. No credential should be embedded in source, installer, logs, screenshots, or documentation.

### P2 - Live relay coverage

Anonymous relay authentication behavior is correct, but this validation deliberately did not use a real customer key. A release candidate still needs disposable keys for CodexPro and Claude groups and real `/v1/models` plus one minimal response request per selected provider.

## 9. Regression Scope

Every release should repeat:

- Workspace Rust tests and CDP injection contract tests.
- Frontend typecheck, model-window tests, and production build.
- Real Codex current-version model-list interception test.
- Mixed/pure mode enable, restart, and restore on a disposable `CODEX_HOME`.
- MCP server and plugin marketplace visibility before/after enable and restore.
- Windows installer/startup and macOS x64/arm64 DMG mount, architecture, signature, and startup.

## 10. Acceptance Decision

Decision: **conditional pass, not yet approved for commercial release**.

Windows logic and the current Codex `26.721` model-injection path are verified. Core grouped-key, preservation, rollback, and session behaviors are covered by passing automated tests. Commercial release remains blocked until the exact current commit passes both macOS GitHub runners, disposable production-like relay keys complete real grouped requests, and the credential/signing distribution risks are explicitly resolved.
