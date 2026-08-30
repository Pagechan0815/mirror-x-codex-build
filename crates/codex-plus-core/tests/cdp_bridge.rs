use base64::Engine;
use codex_plus_core::assets;
use codex_plus_core::bridge::{self, BRIDGE_BINDING_NAME};
use codex_plus_core::cdp::{
    CdpTarget, is_avatar_overlay_page_target, is_primary_codex_page_target,
    is_quick_chat_page_target, list_targets, pick_injectable_codex_page_target, pick_page_target,
};

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::future::Future;
use std::io::Write;
use std::net::SocketAddr;
use std::pin::Pin;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{Notify, oneshot};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

fn target(id: &str, kind: &str, title: &str, url: &str, websocket_url: Option<&str>) -> CdpTarget {
    CdpTarget {
        id: id.to_string(),
        target_type: kind.to_string(),
        title: title.to_string(),
        url: url.to_string(),
        web_socket_debugger_url: websocket_url.map(str::to_string),
    }
}

#[test]
fn bridge_script_defines_expected_globals_and_binding() {
    let script = bridge::build_bridge_script(BRIDGE_BINDING_NAME);

    assert!(script.contains("window.__codexSessionDeleteBridge"));
    assert!(script.contains("window.__codexSessionDeleteResolve"));
    assert!(script.contains("window.__codexSessionDeleteReject"));
    assert!(script.contains("codexSessionDeleteV2"));
}

#[test]
fn injection_script_prefixes_helper_url_and_sponsor_images() {
    let script = assets::injection_script(57321);

    assert!(script.contains("!window.electronBridge"));
    assert!(script.contains(r#"!/^app:\/\/\-\//i.test(window.location.href)"#));
    assert!(script.contains("window.__CODEX_SESSION_DELETE_HELPER__"));
    assert!(script.contains("http://127.0.0.1:57321"));
    assert!(script.contains("window.__CODEX_PLUS_SPONSOR_IMAGES__"));
    assert!(script.contains("window.__CODEX_PLUS_VERSION__"));
    assert!(script.contains(codex_plus_core::version::VERSION));
    assert!(script.contains("https://discord.gg/y96kX7A76v"));
    assert!(script.contains("data-codex-plus-discord"));
}

#[test]
fn injection_script_omits_stepwise_runtime_when_disabled() {
    let script = assets::injection_script_with_settings(
        57321,
        &codex_plus_core::settings::BackendSettings::default(),
    );

    assert!(!script.contains("const API_KEY = \"__codexStepwisePanel\";"));
    assert!(script.contains("data-codex-plus-setting=\"stepwise\""));
}

#[test]
fn injection_script_includes_stepwise_runtime_when_enabled() {
    let settings = codex_plus_core::settings::BackendSettings {
        codex_app_stepwise_enabled: true,
        ..Default::default()
    };
    let script = assets::injection_script_with_settings(57321, &settings);

    assert!(script.contains("const API_KEY = \"__codexStepwisePanel\";"));
}

#[test]
fn injection_script_exposes_image_overlay_config() {
    let temp = tempfile::tempdir().unwrap();
    let image_path = temp.path().join("overlay.png");
    std::fs::write(
        &image_path,
        base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/p9sAAAAASUVORK5CYII=")
            .unwrap(),
    )
    .unwrap();
    let settings = codex_plus_core::settings::BackendSettings {
        codex_app_image_overlay_enabled: true,
        codex_app_image_overlay_path: image_path.to_string_lossy().to_string(),
        codex_app_image_overlay_opacity: 42,
        codex_app_image_overlay_fit_mode: "fill".to_string(),
        ..Default::default()
    };
    let script = assets::injection_script_with_settings(57321, &settings);

    assert!(script.contains("window.__CODEX_PLUS_IMAGE_OVERLAY__"));
    assert!(script.contains("\"enabled\":true"));
    assert!(script.contains("\"opacity\":0.42"));
    assert!(script.contains("\"fitMode\":\"fill\""));
    assert!(script.contains("\"dataUrl\":\"data:image/png;base64,"));
    assert!(script.contains("http://127.0.0.1:57321/overlay/image"));
}

#[test]
fn injection_script_installs_image_overlay_from_data_uri() {
    let script = assets::injection_script(57321);

    assert!(script.contains("const source = config.dataUrl || \"\""));
    assert!(script.contains("backgroundImage: `url(\"${source.replace(/\"/g, \"%22\")}\")`"));
    assert!(script.contains(
        "fit: { size: \"contain\", position: \"center center\", repeat: \"no-repeat\" }"
    ));
    assert!(script.contains("image_overlay_installed"));
}

#[test]
fn injection_script_marks_diagnostic_build_and_reports_script_loaded() {
    let script = assets::injection_script(57321);

    assert!(script.contains("window.__CODEX_PLUS_BUILD__"));
    assert!(script.contains(codex_plus_core::assets::DIAGNOSTIC_BUILD_ID));
    assert!(script.contains("script_loaded"));
    assert!(script.contains("data-codex-plus-build"));
}

#[test]
fn injection_script_fetches_ads_without_bridge() {
    let script = assets::injection_script(57321);

    assert!(script.contains("directFetchCodexPlusAds"));
    assert!(script.contains("cacheBustCodexPlusAdUrl"));
    assert!(script.contains("return { version: 1, ads: [] };"));
    assert!(!script.contains("BigPizzaV3/Ad-List"));
    assert!(
        !script.contains("codexPlusAds = normalizeCodexPlusAds(await postJson(\"/ads\", {}));")
    );
}

#[test]
fn injection_script_only_bounds_status_and_provider_settings_bridge_calls() {
    let script = assets::injection_script(57321);

    assert!(script.contains("bridgeWithBackendTimeout"));
    assert!(script.contains("backend_bridge_timeout"));
    assert!(script.contains("settings bridge timeout"));
    assert!(script.contains("refreshBackendSettingsForProviderRequest"));
    assert!(script.contains("applyBackendSettingsState(postJson(\"/settings/get\", {}))"));
    assert!(script.contains("await refreshBackendSettingsForProviderRequest()"));
    assert!(!script.contains("await loadBackendSettingsState({ timeoutMs"));
    assert!(!script.contains("bridgeWithRouteTimeout"));
    assert!(script.contains("return await window.__codexSessionDeleteBridge(path, payload)"));
    assert!(!script.contains("/backend/repair"));
    assert!(script.contains("backend_status_bridge_failed_http_fallback_ok"));
    assert!(script.contains("backend_status_bridge_and_http_failed"));
}

#[test]
fn injection_script_explains_plugin_patch_is_unneeded_in_relay_mode() {
    let script = assets::injection_script(57321);

    assert!(script.contains("兼容增强模式下无需开启"));
}

#[test]
fn injection_script_menu_exposes_marketplace_plugin_switch_only() {
    let script = assets::injection_script(57321);

    assert!(script.contains("插件市场解锁"));
    assert!(script.contains("data-codex-plus-setting=\"pluginMarketplaceUnlock\""));
    assert!(!script.contains("特殊插件强制安装"));
    assert!(!script.contains("data-codex-plus-setting=\"forcePluginInstall\""));
    assert!(!script.contains("forcePluginInstall"));
    assert!(!script.contains("强制解锁入口"));
    assert!(!script.contains("data-codex-plus-setting=\"pluginEntryUnlock\""));
}

#[test]
fn injection_script_menu_exposes_stepwise_switch_and_syncs_panel() {
    let settings = codex_plus_core::settings::BackendSettings {
        codex_app_stepwise_enabled: true,
        ..Default::default()
    };
    let script = assets::injection_script_with_settings(57321, &settings);

    assert!(script.contains("stepwise: false"));
    assert!(script.contains("stepwise: \"codexAppStepwiseEnabled\""));
    assert!(script.contains("Stepwise"));
    assert!(script.contains("data-codex-plus-setting=\"stepwise\""));
    assert!(script.contains("function syncStepwisePanel"));
    assert!(script.contains("window.__codexStepwisePanel?.syncSettings"));
    assert!(script.contains("if (key === \"stepwise\") syncStepwisePanel(value)"));
    assert!(script.contains("if (patch?.enabled === true)"));
    assert!(script.contains("activateRuntime();"));
}

#[test]
fn stepwise_runtime_stops_work_when_disabled() {
    let script = assets::stepwise_script().replace("\r\n", "\n");

    assert!(script.contains("function stepwiseEnabled()"));
    assert!(script.contains("if (!stepwiseEnabled()) {"));
    assert!(script.contains("stopRuntime();"));
    assert!(script.contains(
        "function requestBridgeStepwise(key, userText, assistantText) {\n    if (!stepwiseEnabled()) return;"
    ));
}

#[test]
fn stepwise_direct_send_targets_main_chat_composer() {
    let script = assets::stepwise_script();

    assert!(script.contains("function elementCenter("));
    assert!(script.contains("function horizontalOverlapRatio("));
    assert!(script.contains("function ignoredComposerContainer("));
    assert!(script.contains("function mainComposerCandidate("));
    assert!(script.contains("mainComposerCandidate(candidates)"));
    assert!(!script.contains("const target = candidates[candidates.length - 1];"));
}

#[test]
fn stepwise_scan_does_not_require_composer_for_suggestions() {
    let script = assets::stepwise_script();

    assert!(!script.contains("if (!composerCandidates().length) return false;"));
}

#[test]
fn stepwise_assistant_detection_accepts_two_action_buttons() {
    let script = assets::stepwise_script();

    assert!(script.contains("if (count >= 2) return current;"));
    assert!(script.contains("if (count < 2) continue;"));
    assert!(!script.contains("if (count >= 3) return current;"));
    assert!(!script.contains("if (count < 3) continue;"));
}

#[test]
fn stepwise_refreshes_suggestions_for_virtualized_assistant_bubbles() {
    let script = assets::stepwise_script();

    assert!(script.contains("function assistantBubbleCandidates("));
    assert!(script.contains("\".group.flex.min-w-0.flex-col\""));
    assert!(script.contains("candidates.push(...assistantBubbleCandidates())"));
    assert!(script.contains("function latestMessageByDocumentOrder("));
    assert!(script.contains("function clearPromptsForNewAssistant("));
    assert!(script.contains(
        "if (state.prompts.length || state.currentHash) clearPromptsForNewAssistant(hash);"
    ));
    assert!(script.contains("function setScanStatus("));
    assert!(script.contains("setScanStatus(\"not-ready\""));
    assert!(script.contains("setScanStatus(\"no-assistant-message\""));
    assert!(!script.contains("setScanStatus(\"surface-not-ready\""));
    assert!(!script.contains("return fallback[fallback.length - 1] || null;"));
}

#[test]
fn stepwise_exposes_manual_refresh_without_refreshing_busy_chats() {
    let script = assets::stepwise_script();

    assert!(script.contains("data-action=\"refresh\""));
    assert!(script.contains("function forceRefreshStepwise("));
    assert!(script.contains("state.bridgeStatus === \"pending\" || chatBusy()"));
    assert!(script.contains("setScanStatus(\"manual-refresh-busy\""));
    assert!(script.contains("state.bridgeCache.delete(bridgeKey)"));
    assert!(script.contains("requestBridgeStepwise(bridgeKey, userText, assistantText)"));
}

#[test]
fn injection_script_defers_backend_mapped_toggles_until_settings_load() {
    let script = assets::injection_script(57321);

    assert!(script.contains("const codexPlusBackendMappedSettings = new Set"));
    assert!(
        script
            .contains("codexPlusBackendMappedSettings.has(key) && !codexPlusBackendSettingsLoaded")
    );
    assert!(script.contains("button.dataset.pending = String(waitsForBackend)"));
    assert!(script.contains(
        "button.disabled = waitsForBackend || button.dataset.relayUnneeded === \"true\""
    ));
    assert!(script.contains("toggle.disabled || toggle.dataset.pending === \"true\""));
}

#[test]
fn injection_script_ignores_stale_backend_settings_responses() {
    let script = assets::injection_script(57321);

    assert!(script.contains("let codexPlusBackendSettingsSeq = 0"));
    assert!(script.contains("const seq = codexPlusBackendSettingsSeq"));
    assert!(script.contains("if (seq !== codexPlusBackendSettingsSeq)"));
    assert!(script.contains("const seq = ++codexPlusBackendSettingsSeq"));
    assert!(script.contains("if (seq === codexPlusBackendSettingsSeq)"));
}

#[test]
fn injection_script_skips_plugin_patch_work_in_relay_mode() {
    let script = assets::injection_script(57321);

    assert!(script.contains("function pluginPatchDisabledInRelayMode()"));
    assert!(script.contains("!codexPlusBackendSettingsLoaded"));
    assert!(script.contains("if (pluginPatchDisabledInRelayMode()) return"));
    assert!(script.contains("clearPluginPatchArtifacts()"));
}

#[test]
fn injection_script_disables_plugin_auto_expand_in_relay_mode() {
    let script = assets::injection_script(57321);

    assert!(script.contains("settings.pluginAutoExpand = false"));
    assert!(script.contains("if (pluginPatchDisabledInRelayMode()) return"));
    assert!(script.contains("if (!codexPlusSettings().pluginAutoExpand) return"));
}

#[test]
fn injection_script_defines_version_gated_plugin_unlock_strategy() {
    let script = assets::injection_script(57321);

    assert!(script.contains("codexPluginLegacyEntryUnlockBeforeVersion = \"26.601.2237\""));
    assert!(script.contains("function parseCodexVersionParts(version)"));
    assert!(script.contains("function compareCodexVersions(left, right)"));
    assert!(script.contains("function codexPluginUnlockStrategy()"));
    assert!(script.contains("const comparison = compareCodexVersions(version, codexPluginLegacyEntryUnlockBeforeVersion)"));
    assert!(script.contains("return comparison < 0 ? \"legacy\" : \"modern\""));
}

#[test]
fn injection_script_gates_legacy_and_modern_plugin_unlock_by_codex_version() {
    let script = assets::injection_script(57321);

    assert!(script.contains("const pluginUnlockStrategy = codexPluginUnlockStrategy()"));
    assert!(script.contains("if ((pluginUnlockStrategy === \"modern\" || pluginUnlockStrategy === \"unknown\") && settings.pluginMarketplaceUnlock)"));
    assert!(script.contains("plugin_unlock_strategy_selected"));
    assert!(script.contains("window.__codexPluginUnlockStrategyLogged"));
}

#[test]
fn injection_script_removes_legacy_plugin_sidebar_entry_unlock() {
    let script = assets::injection_script(57321);

    assert!(!script.contains("pluginEntryUnlock"));
    assert!(!script.contains("codexAppPluginEntryUnlock"));
    assert!(!script.contains("function spoofChatGPTAuthMethod(element)"));
    assert!(!script.contains("auth.setAuthMethod(\"chatgpt\")"));
    assert!(!script.contains("function pluginEntryButton()"));
    assert!(!script.contains("function enablePluginEntry()"));
    assert!(!script.contains("插件 - 已解锁"));
    assert!(!script.contains("Plugins - Unlocked"));
}

#[test]
fn injection_script_keeps_plugin_marketplace_unlock_separate_from_entry_unlock() {
    let script = assets::injection_script(57321);

    assert!(script.contains("pluginMarketplaceUnlock: true"));
    assert!(script.contains("pluginMarketplaceUnlock: \"codexAppPluginMarketplaceUnlock\""));
    assert!(script.contains("if (!codexPlusSettings().pluginMarketplaceUnlock) return"));
    assert!(script.contains("installPluginBuildFlavorFilterPatch"));
    assert!(script.contains("installPluginMarketplaceRequestPatch"));
}

#[test]
fn injection_script_localizes_codex_menu_commands() {
    let script = assets::injection_script(57321);

    assert!(script.contains("const codexMenuLocalizationMap = new Map"));
    assert!(script.contains("[\"Toggle Sidebar\", \"切换侧边栏\"]"));
    assert!(script.contains("[\"Toggle Bottom Panel\", \"切换底部面板\"]"));
    assert!(script.contains("[\"Toggle Pinned Summary\", \"切换置顶摘要\"]"));
    assert!(script.contains("[\"Open Terminal\", \"打开终端\"]"));
    assert!(script.contains("[\"Open Browser Tab\", \"打开浏览器标签页\"]"));
    assert!(script.contains("[\"Focus Browser Address Bar\", \"聚焦浏览器地址栏\"]"));
    assert!(script.contains("[\"Reload Browser Page\", \"重新加载浏览器页面\"]"));
    assert!(script.contains("[\"Toggle Side Panel\", \"切换侧边面板\"]"));
    assert!(script.contains("[\"Actual Size\", \"实际大小\"]"));
    assert!(script.contains("function localizeCodexMenus"));
    assert!(script.contains("localizeCodexMenus();"));
}

#[test]
fn injection_script_does_not_unlock_disabled_plugin_install_buttons() {
    let script = assets::injection_script(57321);

    assert!(script.contains("button[aria-disabled=\"true\"]"));
    assert!(script.contains("[role=\"button\"][data-disabled]"));
    assert!(!script.contains("installButtonUnlockNodes"));
    assert!(!script.contains("patchReactDisabledProps"));
    assert!(!script.contains("props[\"data-disabled\"] = undefined"));
    assert!(!script.contains("button.querySelectorAll?.(\"button, [role='button'], [disabled], [aria-disabled], [data-disabled]"));
    assert!(!script.contains("button.dataset.codexForceInstallUnlocked"));
}

#[test]
fn injection_script_keeps_bundled_marketplace_name_for_default_filter() {
    let script = assets::injection_script(57321);

    assert!(script.contains("codexPluginMarketplaceUnlockVersion = \"15\""));
    assert!(!script.contains("function pluginMarketplaceAliasForName"));
    assert!(
        !script.contains("if (name === \"openai-bundled\") return \"codex-plus-openai-bundled\"")
    );
    assert!(script.contains("if (name === \"openai-bundled\") return \"OpenAI插件1(Mirror X)\""));
}

#[test]
fn injection_script_does_not_bypass_plugin_marketplace_search_filters() {
    let script = assets::injection_script(57321);

    assert!(script.contains("codexPluginMarketplaceUnlockVersion = \"15\""));
    assert!(script.contains("isCodexPluginBuildFlavorFilter"));
    assert!(script.contains("source.includes(\"!u(e.marketplaceName)||e.marketplaceName===r\")"));
    assert!(script.contains("source.includes(\"!Eu(e.marketplaceName)||e.marketplaceName===n\")"));
    assert!(script.contains("source.includes(\"!t.includes(e.name)\")"));
    assert!(!script.contains("if (!source.includes(\"marketplaceName\")) return false"));
    assert!(!script.contains("if (!source.includes(\"name\")) return false"));
}

#[test]
fn injection_script_expands_api_key_plugin_marketplace_requests() {
    let script = assets::injection_script(57321);

    assert!(script.contains("codexPluginMarketplaceUnlockVersion = \"15\""));
    assert!(script.contains("installPluginMarketplaceRequestPatch"));
    assert!(script.contains("installPluginMarketplaceBridgePatch"));
    assert!(script.contains("installPluginBuildFlavorFilterPatch"));
    assert!(script.contains("Array.prototype.filter"));
    assert!(script.contains("codexPluginBuildFlavorFilterPatch"));
    assert!(script.contains("isCodexPluginBuildFlavorFilter"));
    assert!(script.contains(
        "codexPluginOfficialMarketplaceName(plugin?.marketplaceName) && !callback(plugin)"
    ));
    assert!(script.contains("isCodexPluginMarketplaceHiddenFilter"));
    assert!(script.contains(
        "codexPluginOfficialMarketplaceName(marketplace?.name) && !callback(marketplace)"
    ));
    assert!(script.contains("plugin_marketplace_hidden_filter_bypassed"));
    assert!(script.contains("method === \"list-plugins\""));
    assert!(script.contains("message.url.startsWith(urlPrefix)"));
    assert!(script.contains("message.url.slice(urlPrefix.length)"));
    assert!(script.contains("message.type === \"fetch\""));
    assert!(script.contains("data?.type === \"fetch-response\""));
    assert!(script.contains("__codexPluginMarketplaceFetchRequestIds"));
    assert!(script.contains("__codexPluginMarketplaceFetchRequestProfiles"));
    assert!(script.contains("__codexPluginMarketplaceRequestProfiles"));
    assert!(script.contains("pluginMarketplaceRequestProfile"));
    assert!(script.contains("remoteOnlyPluginMarketplaceFallbackResult"));
    assert!(script.contains("let nextKinds = Array.isArray(next.marketplaceKinds)"));
    assert!(script.contains("if (!nextKinds.includes(\"local\")) nextKinds.push(\"local\")"));
    assert!(script.contains("if (!nextKinds.includes(\"vertical\")) nextKinds.push(\"vertical\")"));
    assert!(script.contains("next.marketplaceKinds = Array.from(new Set(nextKinds))"));
    assert!(script.contains("codexPluginBroadCatalogKindsFromVersion = \"26.803.0\""));
    assert!(script.contains("broadCatalogPreserved: true"));
    assert!(script.contains("patchPluginMarketplaceResult"));
    assert!(script.contains("__CODEX_PLUS_PLUGIN_MARKETPLACES__"));
    assert!(script.contains("mergeLocalPluginMarketplaces(result)"));
    assert!(script.contains("plugin_marketplace_local_merged"));
    assert!(script.contains("plugin_marketplace_remote_auth_fallback"));
    assert!(script.contains("cloned.marketplaceName = marketplaceName"));
    assert!(script.contains("cloned.marketplacePath = marketplaceName"));
    assert!(script.contains("restorePluginMarketplaceName"));
    assert!(script.contains(
        "next.remoteMarketplaceName = restorePluginMarketplaceName(next.remoteMarketplaceName)"
    ));
    assert!(!script.contains("marketplace.name = alias"));
    assert!(script.contains("if (name === \"openai-curated\") return \"OpenAI插件2(Mirror X)\""));
    assert!(
        script
            .contains("if (name === \"openai-primary-runtime\") return \"OpenAI插件3(Mirror X)\"")
    );
    assert!(script.contains("restored === \"openai-api-curated\""));
    assert!(script.contains("restored === \"openai-curated-remote\""));
    assert!(script.contains(
        "if (name === \"mirror-x-curated\" || name === \"openai-curated-remote\") return \"OpenAI插件5(Mirror X)\""
    ));
    assert!(script.contains(
        "if (name === \"codex-plus-openai-curated-remote\") return \"openai-curated-remote\""
    ));
    assert!(script.contains("OpenAI插件1(Mirror X)"));
    assert!(script.contains("OpenAI插件2(Mirror X)"));
    assert!(script.contains("OpenAI插件3(Mirror X)"));
    assert!(script.contains("method === \"install-plugin\""));
    assert!(script.contains("plugin_marketplace_response_expanded"));
    assert!(script.contains("plugin_build_flavor_filter_bypassed"));
    assert!(script.contains("plugin_install_request_debug"));
    assert!(script.contains("plugin_install_request_failed"));
    assert!(!script.contains("marketplace.path ="));
    assert!(!script.contains("codexPluginMarketplacePathAliasForName"));
    assert!(!script.contains("spoofAnyCodexAuthContext"));
}

#[test]
fn injection_script_preserves_vertical_marketplace_kind_for_official_plugins() {
    let script = assets::injection_script(57321);

    assert!(script.contains("plugin_marketplace_request_expanded"));
    assert!(script.contains("if (!nextKinds.includes(\"vertical\")) nextKinds.push(\"vertical\")"));
    assert!(!script.contains("codexPluginAllowedMarketplaceKinds"));
    assert!(!script.contains("codexPluginExpandedMarketplaceKinds"));
}

#[test]
fn injection_script_logs_marketplace_grouping_diagnostics() {
    let script = assets::injection_script(57321);

    assert!(script.contains("plugin_marketplace_response_debug"));
    assert!(script.contains("marketplaces: result.marketplaces.map"));
    assert!(script.contains("pluginMarketplaceCounts"));
    assert!(script.contains("remoteMarketplaceName"));
}

#[test]
fn injection_script_recovers_plugin_search_from_remote_auth_errors() {
    let cases = run_plugin_marketplace_search_contract_harness();

    assert_eq!(cases["initialKinds"], json!(["local", "vertical"]));
    assert_eq!(cases["latestBroadOmittedHasKinds"], false);
    assert_eq!(cases["latestBroadOmittedKinds"], serde_json::Value::Null);
    assert_eq!(cases["latestBroadNullHasKinds"], false);
    assert_eq!(cases["latestBroadNullKinds"], serde_json::Value::Null);
    assert_eq!(cases["latestExplicitKinds"], json!(["local", "vertical"]));
    assert_eq!(cases["searchKinds"], json!(["created-by-me-remote"]));
    assert_eq!(cases["searchCwds"], serde_json::Value::Null);
    assert_eq!(cases["searchRemoteOnly"], true);
    assert_eq!(cases["responsePatched"], true);
    assert_eq!(cases["responseHasError"], false);
    assert_eq!(cases["fallbackMarketplaceNames"], json!([]));
    assert_eq!(cases["fallbackPluginNames"], json!([]));
    assert_eq!(cases["fallbackFeaturedPluginIds"], json!([]));
    assert_eq!(cases["fallbackMarketplaceLoadErrors"], json!([]));
    assert_eq!(cases["remoteUnavailable"], true);
    assert_eq!(cases["subsequentKinds"], json!(["created-by-me-remote"]));
    assert_eq!(cases["subsequentCwds"], serde_json::Value::Null);
    assert_eq!(
        cases["generalAfterFallbackKinds"],
        json!(["local", "vertical"])
    );
    assert_eq!(
        cases["latestBroadAfterFallbackKinds"],
        json!(["local", "vertical"])
    );
    assert_eq!(cases["generalAfterFallbackCwds"], json!(["C:/workspace"]));
    assert_eq!(
        cases["localFallbackMarketplaceNames"],
        json!(["fixture-local"])
    );
    assert_eq!(cases["localFallbackPluginNames"], json!(["alpha"]));
    assert_eq!(cases["chatGptKinds"], json!(["created-by-me-remote"]));
    assert_eq!(cases["unrelatedErrorMatched"], false);
}

fn run_plugin_marketplace_search_contract_harness() -> serde_json::Value {
    let temp = tempfile::tempdir().expect("temp dir should be created");
    let script_path = temp.path().join("renderer-inject.js");
    let harness_path = temp.path().join("plugin-marketplace-harness.cjs");
    std::fs::write(&script_path, assets::injection_script(57321))
        .expect("injection script should be written");
    let mut harness = std::fs::File::create(&harness_path).expect("harness should be created");
    write!(
        harness,
        r#"
const scriptPath = {script_path};
const store = new Map();
function node() {{
  return {{
    appendChild() {{}}, prepend() {{}}, remove() {{}}, setAttribute() {{}}, removeAttribute() {{}},
    addEventListener() {{}}, querySelector() {{ return null; }}, querySelectorAll() {{ return []; }},
    closest() {{ return null; }},
    classList: {{ add() {{}}, remove() {{}}, toggle() {{}}, contains() {{ return false; }} }},
    dataset: {{}}, style: {{}}, children: [], isConnected: true, textContent: "", innerHTML: "",
  }};
}}
globalThis.window = globalThis;
window.__CODEX_PLUS_TEST_PLUGIN_MARKETPLACE__ = true;
window.addEventListener = () => {{}};
window.removeEventListener = () => {{}};
window.dispatchEvent = () => true;
globalThis.document = {{
  scripts: [], documentElement: node(), body: node(), createElement: () => node(),
  getElementById: () => null, querySelector: () => null, querySelectorAll: () => [],
  addEventListener() {{}}, removeEventListener() {{}},
}};
globalThis.localStorage = {{
  getItem: (key) => store.has(key) ? store.get(key) : null,
  setItem: (key, value) => store.set(key, String(value)), removeItem: (key) => store.delete(key),
}};
globalThis.sessionStorage = globalThis.localStorage;
globalThis.location = {{ href: "https://codex.test/index.html", pathname: "/index.html", search: "", hash: "" }};
window.location = globalThis.location;
globalThis.navigator = {{ userAgent: "node-test", sendBeacon: () => false }};
globalThis.performance = {{ getEntriesByType: () => [] }};
globalThis.fetch = async () => ({{ ok: true, json: async () => ({{}}) }});
require(scriptPath);
window.__CODEX_PLUS_PLUGIN_MARKETPLACES__ = [{{
  name: "fixture-local",
  displayName: "Fixture Local",
  path: "C:/fixture/marketplace.json",
  plugins: [{{ id: "alpha@fixture-local", name: "alpha", marketplaceName: "fixture-local" }}],
}}];
const api = window.__codexPlusPluginMarketplaceTest;
api.reset();
const initial = api.patchRequestParams("list-plugins", {{ cwds: ["C:/workspace"] }});
api.setCodexAppVersion("26.803.41515");
const latestBroadOmitted = api.patchRequestParams("list-plugins", {{ cwds: ["C:/workspace"] }});
const latestBroadNull = api.patchRequestParams("list-plugins", {{ cwds: ["C:/workspace"], marketplaceKinds: null }});
const latestExplicit = api.patchRequestParams("list-plugins", {{ marketplaceKinds: ["local"] }});
api.setCodexAppVersion("");
const searchMessage = api.patchRequestMessage({{
  type: "mcp-request",
  request: {{
    id: "search-1",
    method: "vscode://codex/list-plugins",
    params: {{ marketplaceKinds: ["created-by-me-remote"] }},
  }},
}});
const remoteAuthMessage = "list remote plugin catalog: chatgpt authentication required for remote plugin catalog; api key auth is not supported";
const response = {{
  type: "mcp-response",
  message: {{ id: "search-1", error: {{ code: -32600, message: remoteAuthMessage }} }},
}};
const responsePatched = api.patchResponseData(response);
const subsequent = api.patchRequestParams("list-plugins", {{ marketplaceKinds: ["created-by-me-remote"] }});
const generalAfterFallback = api.patchRequestParams("list-plugins", {{ marketplaceKinds: ["created-by-me-remote", "local", "vertical"] }});
api.setCodexAppVersion("26.803.41515");
const latestBroadAfterFallback = api.patchRequestParams("list-plugins", {{ cwds: ["C:/workspace"] }});
const fallbackMarketplaces = response.message.result?.marketplaces || [];
const localFallbackMarketplaces = api.localFallback().marketplaces || [];
const remoteUnavailable = api.remoteCatalogUnavailable();
api.reset();
const chatGpt = api.patchRequestParams("list-plugins", {{ marketplaceKinds: ["created-by-me-remote"] }});
const cases = {{
  initialKinds: initial.marketplaceKinds,
  latestBroadOmittedHasKinds: Object.prototype.hasOwnProperty.call(latestBroadOmitted, "marketplaceKinds"),
  latestBroadOmittedKinds: latestBroadOmitted.marketplaceKinds ?? null,
  latestBroadNullHasKinds: Object.prototype.hasOwnProperty.call(latestBroadNull, "marketplaceKinds"),
  latestBroadNullKinds: latestBroadNull.marketplaceKinds,
  latestExplicitKinds: latestExplicit.marketplaceKinds,
  searchKinds: searchMessage.request.params.marketplaceKinds,
  searchCwds: searchMessage.request.params.cwds ?? null,
  searchRemoteOnly: api.requestProfile({{ marketplaceKinds: ["created-by-me-remote"] }}).remoteOnly,
  responsePatched,
  responseHasError: Object.prototype.hasOwnProperty.call(response.message, "error"),
  fallbackMarketplaceNames: fallbackMarketplaces.map((marketplace) => marketplace.name),
  fallbackPluginNames: fallbackMarketplaces.flatMap((marketplace) => marketplace.plugins || []).map((plugin) => plugin.name),
  fallbackFeaturedPluginIds: response.message.result?.featuredPluginIds || [],
  fallbackMarketplaceLoadErrors: response.message.result?.marketplaceLoadErrors || [],
  remoteUnavailable,
  subsequentKinds: subsequent.marketplaceKinds,
  subsequentCwds: subsequent.cwds ?? null,
  generalAfterFallbackKinds: generalAfterFallback.marketplaceKinds,
  generalAfterFallbackCwds: generalAfterFallback.cwds,
  latestBroadAfterFallbackKinds: latestBroadAfterFallback.marketplaceKinds,
  localFallbackMarketplaceNames: localFallbackMarketplaces.map((marketplace) => marketplace.name),
  localFallbackPluginNames: localFallbackMarketplaces.flatMap((marketplace) => marketplace.plugins || []).map((plugin) => plugin.name),
  chatGptKinds: chatGpt.marketplaceKinds,
  unrelatedErrorMatched: api.remoteAuthError({{ message: "network unavailable" }}),
}};
process.stdout.write(JSON.stringify(cases));
"#,
        script_path = serde_json::to_string(&script_path.to_string_lossy().to_string())
            .expect("script path should serialize")
    )
    .expect("harness should be written");

    let output = std::process::Command::new("node")
        .arg(&harness_path)
        .output()
        .expect("node should execute plugin marketplace harness");
    assert!(
        output.status.success(),
        "plugin marketplace harness failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .expect("plugin marketplace harness output should be JSON")
}

#[test]
fn injection_script_omits_force_install_unlock_loop() {
    let script = assets::injection_script(57321);

    assert!(!script.contains("codex-force-install-unlocked"));
    assert!(!script.contains("codexForcePluginInstallRefreshIntervalMs"));
    assert!(!script.contains("refreshForcePluginInstallUnlockLoop"));
    assert!(!script.contains("__codexForcePluginInstallRefreshTimer"));
}

#[test]
fn injection_script_loads_backend_settings_before_initial_scan() {
    let script = assets::injection_script(57321);
    let startup_call = script
        .rfind("void loadBackendSettingsForStartup();")
        .expect("script should load backend settings on startup");
    let footer = &script[startup_call..];
    let initial_scan = footer
        .find("scan();")
        .expect("script should perform an initial scan");
    let footer_marker = footer
        .find("window.__codexProjectMoveApplyProjection")
        .expect("script should continue bootstrapping after the initial scan");

    assert!(initial_scan < footer_marker);
    assert!(script.contains("if (attempt < 60)"));
}

#[test]
fn injection_script_exposes_conversation_view_width_control() {
    let script = assets::injection_script(57321);

    assert!(script.contains("conversationView: false"));
    assert!(script.contains("conversationView"));
    assert!(script.contains("conversationViewMaxWidth"));
    assert!(script.contains("对话居中宽度"));
    assert!(script.contains("data-codex-plus-conversation-view-width"));
    assert!(script.contains("conversationViewWidth()"));
    assert!(script.contains("normalizeConversationViewWidth"));
}

#[test]
fn injection_script_exposes_sidebar_thread_id_badge_control() {
    let script = assets::injection_script(57321);

    assert!(script.contains("threadIdBadge: false"));
    assert!(script.contains("threadIdBadge: \"codexAppThreadIdBadge\""));
    assert!(script.contains("会话 ID 标识"));
    assert!(script.contains("data-codex-plus-setting=\"threadIdBadge\""));
    assert!(script.contains("codex-thread-id-badge"));
    assert!(script.contains("data-codex-thread-id-badge-wrap=\"true\""));
    assert!(script.contains("let threadIdBadgeActive = false"));
    assert!(script.contains("if (threadIdBadgeActive)"));
    assert!(script.contains("function refreshThreadIdBadges()"));
    assert!(script.contains("uuidV7TimestampMs(sessionId)"));
    assert!(script.contains("refreshThreadIdBadges();"));
}

#[test]
fn injection_script_keeps_session_action_buttons_in_pr_style() {
    let script = assets::injection_script(57321);

    assert!(script.contains("actionButtonClass = \"codex-session-action-button\""));
    assert!(script.contains("background: transparent;"));
    assert!(script.contains("background: #363839;"));
    assert!(script.contains("cursor: default;"));
}

#[test]
fn injection_script_guards_temporary_new_thread_ids_before_delete() {
    let script = assets::injection_script(57321);

    assert!(script.contains("function isClientNewThreadId(value)"));
    assert!(script.contains("function normalizedCodexThreadUuid(value)"));
    assert!(script.contains("function reactConversationIdFromRow(row)"));
    assert!(script.contains("__reactFiber$"));
    assert!(script.contains("props?.conversationId"));
    assert!(!script.contains("props?.threadId || props?.sessionId"));
    assert!(script.contains("|| /(?:^|[=/])(?:local:)?client-new-thread:/i.test(href)"));
    assert!(script.contains(
        "? canonicalHrefId || (!hrefIsTemporary ? reactConversationIdFromRow(row) : \"\")"
    ));
    assert!(script.contains("const openDeleteConfirm = (event) => openDeleteConfirmForRow(row, deleteButton, sessionRefFromRow(row), event)"));
    assert!(script.contains("会话仍在同步，请稍后重试"));
    assert!(script.contains("attributeFilter: [\"data-app-action-sidebar-thread-id\", \"href\"]"));

    let cases = run_session_ref_contract_harness();
    assert_eq!(
        cases["canonicalAttribute"],
        "11111111-1111-4111-8111-111111111111"
    );
    assert_eq!(
        cases["canonicalHref"],
        "22222222-2222-4222-8222-222222222222"
    );
    assert_eq!(
        cases["conversationId"],
        "33333333-3333-4333-8333-333333333333"
    );
    assert_eq!(cases["unrelatedIds"], "");
    assert_eq!(cases["temporaryHref"], "");
}

fn run_session_ref_contract_harness() -> serde_json::Value {
    let temp = tempfile::tempdir().expect("temp dir should be created");
    let script_path = temp.path().join("renderer-inject.js");
    let harness_path = temp.path().join("session-ref-harness.cjs");
    std::fs::write(&script_path, assets::injection_script(57321))
        .expect("injection script should be written");
    let mut harness = std::fs::File::create(&harness_path).expect("harness should be created");
    write!(
        harness,
        r#"
const scriptPath = {script_path};
function node() {{
  return {{
    appendChild() {{}}, prepend() {{}}, remove() {{}}, setAttribute() {{}}, removeAttribute() {{}},
    addEventListener() {{}}, querySelector() {{ return null; }}, querySelectorAll() {{ return []; }},
    closest() {{ return null; }}, getAttribute() {{ return null; }},
    classList: {{ add() {{}}, remove() {{}}, toggle() {{}}, contains() {{ return false; }} }},
    dataset: {{}}, style: {{}}, children: [], isConnected: true, textContent: "", innerHTML: "",
  }};
}}
globalThis.window = globalThis;
window.__CODEX_PLUS_TEST_SESSION_REF__ = true;
window.addEventListener = () => {{}};
window.removeEventListener = () => {{}};
window.dispatchEvent = () => true;
globalThis.MutationObserver = class {{ observe() {{}} disconnect() {{}} }};
globalThis.ResizeObserver = class {{ observe() {{}} disconnect() {{}} }};
globalThis.IntersectionObserver = class {{ observe() {{}} disconnect() {{}} }};
globalThis.requestAnimationFrame = (callback) => setTimeout(callback, 0);
globalThis.cancelAnimationFrame = (id) => clearTimeout(id);
globalThis.setInterval = () => 0;
globalThis.clearInterval = () => {{}};
globalThis.document = {{
  scripts: [], documentElement: node(), body: node(), createElement: () => node(),
  getElementById: () => null, querySelector: () => null, querySelectorAll: () => [],
  addEventListener() {{}}, removeEventListener() {{}},
}};
globalThis.localStorage = {{ getItem: () => null, setItem() {{}}, removeItem() {{}} }};
globalThis.sessionStorage = globalThis.localStorage;
globalThis.location = {{ href: "https://codex.test/index.html", pathname: "/index.html", search: "", hash: "" }};
window.location = globalThis.location;
globalThis.navigator = {{ userAgent: "node-test", sendBeacon: () => false }};
globalThis.performance = {{ getEntriesByType: () => [] }};
globalThis.fetch = async () => ({{ ok: true, json: async () => ({{}}) }});
require(scriptPath);

const api = window.__codexPlusSessionRefTest;
const placeholder = "local:client-new-thread:fixture";
const row = (attributes, props = null) => {{
  const value = {{
    getAttribute: (name) => attributes[name] || null,
    querySelector: () => null,
    textContent: "Fixture",
  }};
  if (props) value.__reactFiber$fixture = {{ pendingProps: props, memoizedProps: null, return: null }};
  return value;
}};
const sessionId = (value) => api.fromRow(value).session_id;
const cases = {{
  canonicalAttribute: sessionId(row({{ "data-app-action-sidebar-thread-id": "11111111-1111-4111-8111-111111111111" }})),
  canonicalHref: sessionId(row({{
    "data-app-action-sidebar-thread-id": placeholder,
    href: "/thread/22222222-2222-4222-8222-222222222222",
  }})),
  conversationId: sessionId(row(
    {{ "data-app-action-sidebar-thread-id": placeholder }},
    {{ conversationId: "33333333-3333-4333-8333-333333333333" }},
  )),
  unrelatedIds: sessionId(row(
    {{ "data-app-action-sidebar-thread-id": placeholder }},
    {{ threadId: "44444444-4444-4444-8444-444444444444", sessionId: "55555555-5555-4555-8555-555555555555" }},
  )),
  temporaryHref: sessionId(row(
    {{ "data-app-action-sidebar-thread-id": placeholder, href: "/thread/client-new-thread:fixture" }},
    {{ conversationId: "66666666-6666-4666-8666-666666666666" }},
  )),
}};
process.stdout.write(JSON.stringify(cases));
process.exit(0);
"#,
        script_path = serde_json::to_string(&script_path.to_string_lossy().to_string())
            .expect("script path should serialize")
    )
    .expect("harness should be written");

    let output = Command::new("node")
        .arg(&harness_path)
        .output()
        .expect("node should execute session reference harness");
    assert!(
        output.status.success(),
        "session reference harness failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("session reference harness output should be JSON")
}

#[test]
fn injection_script_moves_export_and_project_move_into_more_menu() {
    let script = assets::injection_script(57321).replace("\r\n", "\n");

    assert!(script.contains("moreButtonClass = \"codex-session-more-button\""));
    assert!(script.contains("moreMenuClass = \"codex-session-more-menu\""));
    assert!(script.contains("configureActionButton(moreButton, \"更多操作\", \"…\")"));
    assert!(script.contains("createSessionMoreMenuItem(\"导出\""));
    assert!(script.contains("createSessionMoreMenuItem(\"移动\""));
    assert!(script.contains("group.appendChild(moreButton)"));
    assert!(script.contains("installMoreButtonEvents(row, moreButton, openMoreMenu)"));
    assert!(script.contains("installSessionMoreMenuAutoClose(row, moreMenu)"));
    assert!(script.contains("updateSessionMoreMenuDirection(moreButton, moreMenu)"));
    assert!(script.contains("positionSessionMoreMenu(moreButton, moreMenu)"));
    assert!(script.contains("document.body.appendChild(moreMenu)"));
    assert!(script.contains("position: fixed;"));
    assert!(script.contains("codex-session-more-menu-open-up"));
    assert!(script.contains("transform: translateY(calc(-100% - 34px));"));
    assert!(script.contains("positionSessionMoreMenu(moreButton, moreMenu);"));
    assert!(script.contains("row.classList.toggle(\"codex-session-more-open\""));
    assert!(script.contains(".${actionGroupClass} {"));
    assert!(script.contains("position: absolute;"));
    assert!(script.contains("pointer-events: none;"));
    assert!(script.contains("[data-codex-delete-row=\"true\"]:hover .${actionGroupClass} {\n        opacity: 1;\n        pointer-events: auto;\n      }"));
    assert!(script.contains("[data-codex-delete-row=\"true\"].codex-session-more-open .${actionGroupClass} {\n        opacity: 1;\n        pointer-events: auto;\n        z-index: 2147483201;"));
    assert!(!script.contains("installActionButtonEvents(row, moreButton, openMoreMenu)"));
    assert!(!script.contains("group.appendChild(exportButton)"));
    assert!(!script.contains("group.appendChild(moveButton)"));
}

#[test]
fn injection_script_does_not_add_delete_controls_on_archived_page() {
    let script = assets::injection_script(57321);

    assert!(script.contains("attachArchivedPageDeleteButton"));
    assert!(script.contains("data-codex-archive-row-action"));
    assert!(script.contains("dataset.codexArchiveRowAction = \"export\""));
    assert!(!script.contains("dataset.codexArchiveRowAction = \"delete\""));
    assert!(!script.contains("installArchivedDeleteAllButton"));
    assert!(!script.contains("删除全部归档"));
}

#[test]
fn injection_script_unlocks_custom_model_catalog() {
    let script = assets::injection_script(57321);

    assert!(script.contains("/codex-model-catalog"));
    assert!(script.contains("codexModelCatalog"));
    assert!(script.contains("patchModelArray"));
    assert!(script.contains("patchStatsigModelDynamicConfig"));
    assert!(script.contains("patchModelJsonResponse"));
    assert!(script.contains("modelJsonResponseLooksPatchable"));
    let model_json_patch_start = script
        .find("async function patchModelJsonResponse")
        .expect("model JSON patch should be embedded");
    let model_json_patch_end = script[model_json_patch_start..]
        .find("function installModelJsonResponsePatch")
        .map(|offset| model_json_patch_start + offset)
        .expect("model JSON patch installer should follow the patch function");
    let model_json_patch = &script[model_json_patch_start..model_json_patch_end];
    let non_model_guard = model_json_patch
        .find("if (!modelJsonResponseLooksPatchable(payload)) return payload;")
        .expect("non-model JSON must have an early guard");
    let catalog_load = model_json_patch
        .find("await loadCodexModelCatalog();")
        .expect("model JSON should wait for the catalog before returning");
    assert!(
        non_model_guard < catalog_load,
        "non-model JSON must bypass the catalog bridge before any catalog load"
    );
    assert!(script.contains("installAppServerModelRequestPatch"));
    assert!(script.contains("loadAppServerRequestCandidates"));
    assert!(script.contains("appServerFallbackAssetUrls"));
    assert!(script.contains("collectAppServerRequestCandidatesFromModule"));
    assert!(script.contains("codexAppServerModelRequestPatchVersion = \"15\""));
    assert!(script.contains("codexModelJsonResponsePatchVersion = \"5\""));
    assert!(script.contains("codexModelMessagePatchVersion = \"3\""));
    assert!(script.contains("appServerModelRequestPatchPromise"));
    assert!(script.contains("scheduleAppServerModelRequestPatchRetry"));
    assert!(!script.contains("installAppServerModelDispatcherPatch"));
    assert!(script.contains("list-models-for-host"));
    assert!(script.contains("appServerModelRequestMethod"));
    assert!(script.contains("send-cli-request-for-host"));
    assert!(script.contains("Response.prototype.json"));
    assert!(script.contains("scheduleCodexModelWhitelistRefresh"));
    assert!(script.contains("runCodexModelWhitelistRefreshPass"));
    assert!(script.contains("model_whitelist_refresh_scheduled"));
    assert!(script.contains("available_models"));
    assert!(script.contains("modelWhitelistUnlock"));
    assert!(script.contains("refreshCodexModelWhitelistFromScan"));
    assert!(script.contains("codexPlusModelListRequestIds.size === 0"));
    assert!(!script.contains("function patchReactModelState"));
    assert!(!script.contains("function patchObjectGraphForModels"));
    assert!(script.contains("window.dispatchEvent = function codexPlusModelPatchedDispatchEvent"));
    assert!(script.contains("Promise.resolve(loadCodexModelCatalog(true)).then"));
    assert!(script.contains("codexPlusModelListRequestIds.delete(requestId)"));
    assert!(script.contains("String(name) === \"107580212\""));
    assert!(script.contains("event?.type === \"codex-message-from-view\""));
    assert!(!script.contains("querySelectorAll(\"button, [role='menu']"));
}

#[test]
fn injection_script_exposes_fast_service_tier_control() {
    let script = assets::injection_script(57321);

    assert!(script.contains("default-service-tier"));
    assert!(script.contains("setting-storage-"));
    assert!(script.contains("codexAppAssetUrl"));
    assert!(script.contains("codexThreadServiceTierOverrides"));
    assert!(script.contains("setCodexThreadServiceTierMode"));
    assert!(script.contains("codexServiceTierRequestOverride"));
    assert!(script.contains("codexServiceTierSupportedFastModels"));
    assert!(script.contains("\"gpt-5.4\""));
    assert!(script.contains("\"gpt-5.5\""));
    assert!(script.contains("\"gpt-5.6-sol\""));
    assert!(script.contains("\"gpt-5.6-terra\""));
    assert!(script.contains("\"gpt-5.6-luna\""));
    assert!(script.contains("codexPlusModelMetadata"));
    assert!(script.contains("applyCodexPlusModelMetadata"));
    assert!(script.contains("codexServiceTierFastSupportedForModel"));
    assert!(script.contains("codexServiceTierModelForRequest"));
    assert!(script.contains("codexServiceTierMaybeLoadModelCatalog"));
    assert!(script.contains("fastBlocked"));
    assert!(script.contains("data-tier=\"unsupported\""));
    assert!(script.contains("nextParams.service_tier = override.serviceTier"));
    assert!(script.contains("serviceTierControls: false"));
    assert!(script.contains("data-codex-plus-setting=\"serviceTierControls\""));
    assert!(script.contains("data-codex-service-tier-controls"));
    assert!(script.contains("removeCodexServiceTierBadges"));
    assert!(script.contains("installCodexServiceTierDispatcherPatch"));
    assert!(script.contains("服务模式"));
    assert!(script.contains("data-codex-service-tier-status"));
    assert!(script.contains("data-codex-service-tier-inherit"));
    assert!(script.contains("data-codex-service-tier-standard"));
    assert!(script.contains("data-codex-service-tier-fast"));
    assert!(script.contains("data-codex-service-tier-custom"));
    assert!(script.contains("data-codex-service-tier-thread-inherit"));
    assert!(script.contains("data-codex-service-tier-thread-standard"));
    assert!(script.contains("data-codex-service-tier-thread-fast"));
    assert!(script.contains("global-standard"));
    assert!(script.contains("global-fast"));
    assert!(script.contains("defaultMode"));
    assert!(script.contains("codexServiceTierEffectiveThreadMode"));
    assert!(script.contains("codexServiceTierDefaultModeForControlMode"));
    assert!(script.contains("normalizeCodexServiceTierControlMode(state.mode) !== \"custom\""));
    assert!(script.contains("state.draft = null"));
    assert!(script.contains("后端未连接，无法切换服务模式"));
    assert!(script.contains("未连接"));
    assert!(script.contains("thread/start"));
    assert!(script.contains("thread/resume"));
    assert!(script.contains("turn/start"));
    assert!(script.contains("send-cli-request-for-host"));
    assert!(script.contains("start-conversation"));
    assert!(script.contains("applyCodexServiceTierRequestOverride(\"thread/start\", message)"));
    assert!(script.contains("codex-service-tier-badge"));
    assert!(script.contains("installCodexServiceTierBadge"));
    assert!(script.contains("toggleCodexServiceTierFromBadge"));
    assert!(script.contains("wireCodexServiceTierBadge"));
    assert!(script.contains("codexServiceTierBadgePlacement"));
    assert!(script.contains("codexServiceTierBadgeFooterGroup"));
    assert!(script.contains("codexServiceTierFindComposerEl"));
    assert!(script.contains("codexServiceTierVisibleComposerFooters"));
    assert!(script.contains("codexServiceTierBestComposerFooter"));
    assert!(script.contains("codexServiceTierComposerCandidates"));
    assert!(script.contains("codexServiceTierComposerScore"));
    assert!(script.contains("data-codex-service-tier-badge"));
    assert!(script.contains("codexServiceTierBadgeWired"));
    assert!(script.contains("setAttribute(\"role\", \"button\")"));
    assert!(script.contains("setAttribute(\"tabindex\", \"0\")"));
    assert!(script.contains("继承 config.toml"));
    assert!(script.contains("service_tier=\\\"priority\\\""));
    assert!(script.contains("Fast 仅支持"));
    assert!(script.contains("当前 thread"));
    assert!(script.contains("standard"));
    assert!(script.contains("fast"));
    assert!(script.contains("[\"setting-storage-\", \"app-initial-\"]"));
    assert!(script.contains("[\"setting-storage-\", \"vscode-api-\", \"app-initial-\"]"));
    assert!(script.contains("[\"vscode-api-\", \"app-initial-\"]"));
    assert!(script.contains("codexSettingStorageFromModule"));
    assert!(script.contains("codexStateApiFromModule"));
    assert!(script.contains("message.type === \"fetch\""));
    assert!(script.contains("vscode://codex/"));
    assert!(script.contains("codexRemoteSessionProviderOverrideEnabled"));
    assert!(script.contains("applyCodexRemoteSessionProviderOverride"));
    assert!(script.contains("remote_session_provider_override_applied"));
}

#[test]
fn injection_script_prompts_for_markdown_export_path_when_supported() {
    let script = assets::injection_script(57321);

    assert!(script.contains("showSaveFilePicker"));
    assert!(script.contains("suggestedName: filename"));
    assert!(script.contains("createWritable()"));
    assert!(script.contains("await writable.write(markdown)"));
    assert!(script.contains("status: \"cancelled\""));
    assert!(script.contains("导出已取消"));
}

#[test]
fn injection_script_discovers_vscode_api_asset_without_hardcoded_hash() {
    let script = assets::injection_script(57321);

    assert!(script.contains("[\"vscode-api-\", \"app-initial-\"]"));
    assert!(script.contains("loadCodexAppModule(assetPrefix)"));
    assert!(script.contains("codexAppAssetUrlFromScriptText"));
    assert!(script.contains("fetch(src"));
    assert!(!script.contains("vscode-api-Dc9pX2Bc.js"));
    assert!(!script.contains("import(\"./assets/vscode-api-"));
}

#[test]
fn injection_script_discovers_app_server_clients_without_hardcoded_hash() {
    let script = assets::injection_script(57321);

    assert!(script.contains("loadAppServerRequestCandidates"));
    assert!(script.contains("appServerFallbackAssetUrls"));
    assert!(script.contains("[\"use-host-config-\", \"app-server-manager-signals-\"]"));
    assert!(script.contains("candidateCount: candidates.length"));
    assert!(script.contains("discovery"));
    assert!(script.contains("loadOptionalCodexAppModule(assetPrefix)"));
    assert!(!script.contains("collectAppServerRequestCandidatesFromReactRoot"));
    assert!(!script.contains("window.__codexRoot?._internalRoot"));
    assert!(!script.contains("scheduleAppServerModelReactFallbackPatch"));
    assert!(!script.contains("discovery: \"react-root\""));
    assert!(!script.contains("sources.push(\"react-root\")"));
}

#[test]
fn injection_script_clears_project_state_when_moving_to_projectless() {
    let script = assets::injection_script(57321);

    assert!(script.contains("async function clearThreadWorkspaceHints"));
    assert!(script.contains("async function clearThreadWritableRoots"));
    assert!(script.contains("async function clearThreadProjectlessOutputDirectories"));
    assert!(script.contains("thread-workspace-root-hints"));
    assert!(script.contains("thread-writable-roots"));
    assert!(script.contains("thread-projectless-output-directories"));
    assert!(script.contains("await clearThreadWorkspaceHints(ref)"));
    assert!(script.contains("await clearThreadWritableRoots(ref)"));
    assert!(script.contains("await clearThreadProjectlessOutputDirectories(ref)"));
}

#[test]
fn injection_script_applies_fast_service_tier_contract() {
    let cases = run_service_tier_contract_harness();

    assert_eq!(cases["supportedFast"]["serviceTier"], "priority");
    assert_eq!(cases["supportedFast"]["service_tier"], "priority");

    assert_eq!(
        cases["unsupportedModel"]["serviceTier"],
        serde_json::Value::Null
    );
    assert_eq!(
        cases["unsupportedModel"]["service_tier"],
        serde_json::Value::Null
    );

    assert_eq!(cases["turnWithoutModel"]["serviceTier"], "priority");
    assert_eq!(cases["turnWithoutModelDiagnosticModel"], "gpt-5.4");

    assert_eq!(
        cases["customInheritUnsupported"]["serviceTier"],
        serde_json::Value::Null
    );
    assert_eq!(
        cases["customInheritUnsupported"]["service_tier"],
        serde_json::Value::Null
    );

    assert_eq!(cases["startConversation"]["serviceTier"], "priority");
    assert_eq!(cases["solDescriptor"]["displayName"], "GPT-5.6-Sol");
    assert_eq!(cases["solDescriptor"]["defaultReasoningEffort"], "low");
    assert_eq!(
        cases["solDescriptor"]["supportedReasoningEfforts"][5]["reasoningEffort"],
        "ultra"
    );

    assert_eq!(cases["pureApiThreadStartProvider"], "custom");
    assert_eq!(cases["pureApiLegacyCustomProvider"], "custom");
    assert_eq!(cases["pureApiStaleThreadStartProvider"], "custom");
    assert_eq!(cases["pureApiThreadResumeProvider"], "custom");
    assert_eq!(cases["pureApiTurnStartProvider"], "custom");
    assert_eq!(cases["pureApiTurnWithoutProviderUnchanged"], true);
    assert_eq!(cases["pureApiOtherProviderUnchanged"], true);
    assert_eq!(cases["mixedApiThreadStartProvider"], "custom");
    assert_eq!(cases["mixedApiThreadResumeUnchanged"], true);
    assert_eq!(cases["officialProviderUnchanged"], true);
    assert_eq!(cases["refreshedCustomProviderOverride"], "custom");
    assert_eq!(cases["refreshedOpenAiProvider"], "openai");
    assert_eq!(cases["failedRefreshProviderOmitted"], true);
    assert_eq!(cases["failedRefreshCustomProviderOmitted"], true);
    assert_eq!(cases["timedOutRefreshResolved"], true);
    assert_eq!(cases["timedOutRefreshProviderOmitted"], true);
    assert!(
        cases["timedOutRefreshElapsedMs"]
            .as_u64()
            .expect("timeout duration should be an integer")
            < 6_000
    );
}

fn run_service_tier_contract_harness() -> serde_json::Value {
    let temp = tempfile::tempdir().expect("temp dir should be created");
    let script_path = temp.path().join("renderer-inject.js");
    let harness_path = temp.path().join("service-tier-harness.cjs");
    std::fs::write(&script_path, assets::injection_script(57321))
        .expect("injection script should be written");
    let mut harness = std::fs::File::create(&harness_path).expect("harness should be created");
    write!(
        harness,
        r#"
const scriptPath = {script_path};
const store = new Map();
store.set("codexPlusSettings", JSON.stringify({{ serviceTierControls: true }}));
function node() {{
  return {{
    appendChild() {{}},
    prepend() {{}},
    remove() {{}},
    setAttribute() {{}},
    removeAttribute() {{}},
    addEventListener() {{}},
    querySelector() {{ return null; }},
    querySelectorAll() {{ return []; }},
    closest() {{ return null; }},
    classList: {{ add() {{}}, remove() {{}}, toggle() {{}}, contains() {{ return false; }} }},
    dataset: {{}},
    style: {{}},
    children: [],
    isConnected: true,
    textContent: "",
    innerHTML: "",
  }};
}}
globalThis.window = globalThis;
globalThis.Element = class Element {{}};
window.__CODEX_PLUS_TEST_SERVICE_TIER__ = true;
globalThis.document = {{
  scripts: [],
  documentElement: node(),
  body: node(),
  createElement: () => node(),
  getElementById: () => null,
  querySelector: () => null,
  querySelectorAll: () => [],
  addEventListener() {{}},
  removeEventListener() {{}},
}};
globalThis.localStorage = {{
  getItem: (key) => store.has(key) ? store.get(key) : null,
  setItem: (key, value) => store.set(key, String(value)),
  removeItem: (key) => store.delete(key),
}};
globalThis.location = {{ href: "https://codex.test/thread/thread-12345678", pathname: "/thread/thread-12345678", search: "", hash: "" }};
window.location = globalThis.location;
globalThis.navigator = {{ userAgent: "node-test" }};
globalThis.performance = {{ getEntriesByType: () => [] }};
require(scriptPath);
const api = window.__codexPlusServiceTierTest;
(async () => {{
const appServerCalls = [];
const appServerClient = {{
  async sendRequest(method, params, options) {{
    appServerCalls.push({{ method, params, options }});
    return {{ status: "ok" }};
  }},
}};
api.patchRequestClient(appServerClient);
api.setServiceTierState({{ serviceTier: "priority", fastTierValue: "priority" }});
api.setModelCatalog({{ status: "ok", model: "gpt-5.4", default_model: "gpt-5.4", models: ["gpt-5.4", "gpt-5.5"] }});

api.setThreadState({{ mode: "global-fast", defaultMode: "fast", entries: {{}} }});
const supportedFast = api.applyServiceTierOverride("turn/start", {{
  threadId: "thread-12345678",
  model: "gpt-5.4",
  service_tier: null,
}}, "conv-should-not-be-model");

const unsupportedModel = api.applyServiceTierOverride("turn/start", {{
  threadId: "thread-12345678",
  model: "gpt-4.1",
  service_tier: "priority",
}}, "conv-should-not-be-model");

const turnWithoutModel = api.applyServiceTierOverride("turn/start", {{
  threadId: "thread-12345678",
  service_tier: null,
}}, "conversation-should-not-be-model");
const turnWithoutModelDiagnosticModel = api.diagnostics().at(-1)?.detail?.model;

api.setModelCatalog({{ status: "ok", model: "gpt-4.1", default_model: "gpt-4.1", models: ["gpt-4.1"] }});
api.setThreadState({{ mode: "custom", defaultMode: "inherit", entries: {{}}, draft: {{ mode: "inherit", at: Date.now() }} }});
api.setServiceTierState({{ serviceTier: "priority" }});
const customInheritUnsupported = api.applyServiceTierOverride("turn/start", {{
  threadId: "thread-12345678",
  service_tier: "priority",
}}, "");

api.setModelCatalog({{ status: "ok", model: "gpt-5.5", default_model: "gpt-5.5", models: ["gpt-5.5"] }});
api.setThreadState({{ mode: "global-fast", defaultMode: "fast", entries: {{}} }});
const startConversation = api.requestOverride({{
  type: "start-conversation",
  threadId: "thread-12345678",
  model: "gpt-5.5",
}});

api.setModelCatalog({{
  status: "ok",
  model: "gpt-5.6-sol",
  default_model: "gpt-5.6-sol",
  models: ["gpt-5.6-sol"],
  modelMetadata: {{
    "gpt-5.6-sol": {{
      displayName: "GPT-5.6-Sol",
      description: "Latest frontier agentic coding model.",
      defaultReasoningEffort: "low",
      supportedReasoningEfforts: ["low", "medium", "high", "xhigh", "max", "ultra"].map((reasoningEffort) => ({{ reasoningEffort }})),
    }},
  }},
}});
const solDescriptor = api.modelDescriptor("gpt-5.6-sol");

api.setBackendSettings({{
  relayProfilesEnabled: true,
  activeRelayId: "mirrorplus",
  activeRelaySessionProvider: "custom",
  activeRelayCodexProvider: "custom",
  relayProfiles: [{{ id: "mirrorplus", relayMode: "pureApi", officialMixApiKey: false }}],
}});
api.setModelCatalog({{
  status: "ok",
  model: "gpt-5.5",
  default_model: "gpt-5.5",
  model_provider: "mirrorplus",
  codex_model_provider: "stale_catalog_provider",
  models: ["gpt-5.5"],
}});
const pureApiThreadStartProvider = api.applyProviderOverride("thread/start", {{
  cwd: "C:/mobile",
  modelProvider: "openai",
}})?.modelProvider;
const pureApiLegacyCustomProvider = api.applyProviderOverride("thread/start", {{
  cwd: "C:/mobile",
  modelProvider: "custom",
}})?.modelProvider;
const pureApiStaleThreadStartProvider = api.applyProviderOverride("thread/start", {{
  cwd: "C:/mobile",
  modelProvider: "stale-provider",
}})?.modelProvider;
const pureApiThreadResumeProvider = api.applyProviderOverride("thread/resume", {{
  threadId: "thread-existing",
  model_provider: "openai",
}})?.modelProvider;
const pureApiTurnStartProvider = api.applyProviderOverride("turn/start", {{
  threadId: "thread-existing",
  modelProvider: "openai",
}})?.modelProvider;
const pureApiTurnWithoutProvider = {{ threadId: "thread-existing", model: "gpt-5.5" }};
const pureApiTurnWithoutProviderUnchanged = api.applyProviderOverride("turn/start", pureApiTurnWithoutProvider) === pureApiTurnWithoutProvider;
const pureApiOtherProvider = {{ threadId: "thread-existing", modelProvider: "another-provider" }};
const pureApiOtherProviderUnchanged = api.applyProviderOverride("turn/start", pureApiOtherProvider) === pureApiOtherProvider;
api.setBackendSettings({{
  activeRelayId: "mirrorplus",
  activeRelaySessionProvider: "custom",
  activeRelayCodexProvider: "custom",
  relayProfiles: [{{ id: "mirrorplus", relayMode: "mixedApi", officialMixApiKey: true }}],
}});
const mixedApiThreadStartProvider = api.applyProviderOverride("thread/start", {{
  cwd: "C:/mobile",
  modelProvider: "openai",
}})?.modelProvider;
const mixedApiResumeParams = {{ threadId: "thread-existing", modelProvider: "openai" }};
const mixedApiThreadResumeUnchanged = api.applyProviderOverride("thread/resume", mixedApiResumeParams) === mixedApiResumeParams;
api.setBackendSettings({{
  activeRelayId: "official",
  activeRelaySessionProvider: "openai",
  activeRelayCodexProvider: "openai",
  relayProfiles: [{{ id: "official", relayMode: "official", officialMixApiKey: false }}],
}});
const officialParams = {{ cwd: "C:/mobile", modelProvider: "openai" }};
const officialProviderUnchanged = api.applyProviderOverride("thread/start", officialParams) === officialParams;

api.setBackendSettings({{
  relayProfilesEnabled: false,
  activeRelayId: "official-before-settings-load",
  activeRelaySessionProvider: "openai",
  activeRelayCodexProvider: "openai",
  relayProfiles: [{{ id: "official-before-settings-load", relayMode: "official", officialMixApiKey: false }}],
}});
api.setBackendSettingsLoaded(false);
window.__codexSessionDeleteBridge = async (path) => {{
  if (path !== "/settings/get") return {{ status: "failed" }};
  return {{
    launchMode: "patch",
    enhancementsEnabled: true,
    providerSyncEnabled: true,
    relayProfilesEnabled: true,
    activeRelayId: "refreshed-pure-api",
    activeRelaySessionProvider: "custom",
    activeRelayCodexProvider: "refreshed_vendor",
    relayProfiles: [{{ id: "refreshed-pure-api", relayMode: "pureApi", officialMixApiKey: false }}],
  }};
}};
await appServerClient.sendRequest("thread/start", {{ cwd: "C:/mobile", modelProvider: "custom" }}, {{ signal: "provider-switch" }});
const refreshedCustomProviderOverride = appServerCalls.at(-1)?.params?.modelProvider || "";

api.setBackendSettings({{
  activeRelayId: "stale-official-mix",
  activeRelaySessionProvider: "custom",
  activeRelayCodexProvider: "stale_provider",
  relayProfiles: [{{ id: "stale-official-mix", relayMode: "official", officialMixApiKey: true }}],
}});
window.__codexSessionDeleteBridge = async (path) => {{
  if (path !== "/settings/get") return {{ status: "failed" }};
  return {{
    launchMode: "patch",
    enhancementsEnabled: true,
    providerSyncEnabled: true,
    relayProfilesEnabled: true,
    activeRelayId: "refreshed-official-mix",
    activeRelaySessionProvider: "openai",
    activeRelayCodexProvider: "openai",
    relayProfiles: [{{ id: "refreshed-official-mix", relayMode: "official", officialMixApiKey: true }}],
  }};
}};
await appServerClient.sendRequest("thread/start", {{ cwd: "C:/mobile", modelProvider: "openai" }}, {{ signal: "openai-switch" }});
const refreshedOpenAiProvider = appServerCalls.at(-1)?.params?.modelProvider || "";

api.setBackendSettings({{
  activeRelayId: "stale-pure-api",
  activeRelaySessionProvider: "custom",
  activeRelayCodexProvider: "stale_provider",
  relayProfiles: [{{ id: "stale-pure-api", relayMode: "pureApi", officialMixApiKey: false }}],
}});
window.__codexSessionDeleteBridge = async () => {{
  throw new Error("settings unavailable");
}};
await appServerClient.sendRequest("thread/start", {{ cwd: "C:/mobile", modelProvider: "openai" }}, {{ signal: "provider-refresh-failed" }});
const failedRefreshProviderOmitted = !Object.prototype.hasOwnProperty.call(appServerCalls.at(-1)?.params || {{}}, "modelProvider")
  && !Object.prototype.hasOwnProperty.call(appServerCalls.at(-1)?.params || {{}}, "model_provider");
await appServerClient.sendRequest("thread/start", {{ cwd: "C:/mobile", modelProvider: "custom" }}, {{ signal: "provider-refresh-failed-custom" }});
const failedRefreshCustomProviderOmitted = !Object.prototype.hasOwnProperty.call(appServerCalls.at(-1)?.params || {{}}, "modelProvider")
  && !Object.prototype.hasOwnProperty.call(appServerCalls.at(-1)?.params || {{}}, "model_provider");
window.__codexSessionDeleteBridge = async () => new Promise(() => {{}});
const timedOutRefreshStartedAt = Date.now();
let timedOutRefreshHarnessTimer;
const timedOutRefreshResult = await Promise.race([
  appServerClient
    .sendRequest("thread/start", {{ cwd: "C:/mobile", modelProvider: "openai" }}, {{ signal: "provider-refresh-timeout" }})
    .then(() => "resolved"),
  new Promise((resolve) => {{
    timedOutRefreshHarnessTimer = setTimeout(() => resolve("harness-timeout"), 8000);
  }}),
]);
clearTimeout(timedOutRefreshHarnessTimer);
const timedOutRefreshElapsedMs = Date.now() - timedOutRefreshStartedAt;
const timedOutRefreshResolved = timedOutRefreshResult === "resolved";
const timedOutRefreshProviderOmitted = !Object.prototype.hasOwnProperty.call(appServerCalls.at(-1)?.params || {{}}, "modelProvider")
  && !Object.prototype.hasOwnProperty.call(appServerCalls.at(-1)?.params || {{}}, "model_provider");
delete window.__codexSessionDeleteBridge;

process.stdout.write(JSON.stringify({{
  supportedFast,
  unsupportedModel,
  turnWithoutModel,
  turnWithoutModelDiagnosticModel,
  customInheritUnsupported,
  startConversation,
  solDescriptor,
  pureApiThreadStartProvider,
  pureApiLegacyCustomProvider,
  pureApiStaleThreadStartProvider,
  pureApiThreadResumeProvider,
  pureApiTurnStartProvider,
  pureApiTurnWithoutProviderUnchanged,
  pureApiOtherProviderUnchanged,
  mixedApiThreadStartProvider,
  mixedApiThreadResumeUnchanged,
  officialProviderUnchanged,
  refreshedCustomProviderOverride,
  refreshedOpenAiProvider,
  failedRefreshProviderOmitted,
  failedRefreshCustomProviderOmitted,
  timedOutRefreshResolved,
  timedOutRefreshProviderOmitted,
  timedOutRefreshElapsedMs,
}}));
}})().catch((error) => {{
  console.error(error?.stack || String(error));
  process.exitCode = 1;
}});
"#,
        script_path = serde_json::to_string(&script_path.to_string_lossy().to_string())
            .expect("script path should serialize")
    )
    .expect("harness should be written");
    drop(harness);

    let output = Command::new("node")
        .arg(&harness_path)
        .output()
        .expect("node should run service-tier harness");
    assert!(
        output.status.success(),
        "node harness failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("harness stdout should be JSON")
}

#[test]
fn injection_script_restores_thread_scroll_positions() {
    let script = assets::injection_script(57321);

    assert!(script.contains("threadScrollRestore"));
    assert!(script.contains("codexThreadScroll"));
    assert!(script.contains("installThreadScrollRouteHooks"));
    assert!(script.contains("scheduleThreadScrollSync"));
}

#[test]
fn injection_script_installs_upstream_branch_dropdown_adapter() {
    let script = assets::injection_script(57321);

    assert!(script.contains("installUpstreamBranchDropdownAdapter"));
    assert!(script.contains("installUpstreamPendingWorktreeDispatcherPatch"));
    assert!(script.contains("data-codex-upstream-branch-option"));
    assert!(script.contains("codexUpstreamBranchSelection"));
    assert!(script.contains("/upstream-worktree/defaults"));
    assert!(script.contains("/upstream-worktree/prepare"));
    assert!(script.contains("injectUpstreamBranchOptions"));
    assert!(script.contains("Upstream"));
    assert!(script.contains("data-base-branch"));
    assert!(script.contains("data-project-id"));
    assert!(script.contains("MutationObserver"));
    assert!(script.contains("upstreamWorktreePayloadFromSelection"));
    assert!(script.contains("readUpstreamBranchSelection"));
    assert!(script.contains("writeUpstreamBranchSelection(null)"));
    assert!(script.contains("currentProjectRepoPathFromSelectedProjectButton"));
    assert!(script.contains("currentProjectRepoPathFromStartButton"));
    assert!(script.contains("Start new chat in"));
    assert!(script.contains("codexUpstreamProjectContext"));
    assert!(script.contains("rememberStartNewChatProjectContext"));
    assert!(script.contains("currentProjectContextForBranchMenu"));
    assert!(script.contains("remoteProjectContextFromGlobalState"));
    assert!(script.contains("upstreamBranchDefaultsInflight = new Map()"));
    assert!(script.contains("upstreamRemoteBranchDefaultsCacheTtlMs"));
    assert!(script.contains("upstreamBranchDefaultsInflight.delete(cacheKey)"));
    assert!(script.contains("projectId:"));
    assert!(script.contains("data-codex-upstream-branch-selection-label"));
    assert!(script.contains("syncUpstreamBranchTriggerLabel"));
    assert!(script.contains("syncUpstreamBranchMenuSelection"));
    assert!(script.contains("applyUpstreamPendingWorktreeOverride"));
    assert!(script.contains("pending-worktree-create"));
    assert!(script.contains("qualifiedSourceRef"));
    assert!(script.contains("refs/remotes/${remote}/${baseBranch}"));
    assert!(script.contains("startingState: { ...request.startingState, branchName: sourceRef }"));
    assert!(script.contains("data-codex-upstream-branch-check"));
    assert!(script.contains("data-codex-upstream-branch-icon"));
    assert!(script.contains("branchIconSvg"));
    assert!(script.contains("checkmarkSvg"));
    assert!(script.contains("aria-checked"));
    assert!(script.contains("check.removeAttribute(\"hidden\")"));
    assert!(script.contains("check.setAttribute(\"hidden\", \"\")"));
    assert!(script.contains("handleNativeBranchSelection"));
    assert!(script.contains("clearUpstreamBranchTriggerLabel"));
    assert!(!script.contains(r#"text.includes("/")"#));
    assert!(script.contains("newWorktreeModeActive"));
    assert!(script.contains("effectiveElementRect"));
    assert!(script.contains("removeUpstreamBranchOptions"));
    assert!(script.contains("cleanupInvalidUpstreamBranchOptions"));
    assert!(script.contains("branchMenuInNewWorktreeMode"));
    assert!(script.contains("branchMenuTriggerIsBranchControl"));
    assert!(script.contains("actual-upstream-refs-v16"));
    assert!(script.contains("create and checkout new branch"));
    assert!(script.contains("if (/^start in"));
    assert!(script.contains("if (!branchMenuInNewWorktreeMode(trigger))"));
}

#[test]
fn injection_script_prevents_switching_to_branches_used_by_other_worktrees() {
    let script = assets::injection_script(57321);

    assert!(script.contains("data-codex-branch-worktree-path"));
    assert!(script.contains("annotateBranchMenuWorktreeUsage"));
    assert!(script.contains("branchWorktreePathFromMenuItem"));
    assert!(script.contains("该分支已在另一个 worktree 使用"));
    assert!(script.contains("event.stopImmediatePropagation?.()"));
}

#[test]
fn injection_script_rebuilds_upstream_options_for_each_project_branch_menu() {
    let script = assets::injection_script(57321);

    assert!(script.contains("currentProjectRepoPathForBranchMenu"));
    assert!(script.contains("repoPathFromProjectLabel"));
    assert!(script.contains("projectContextFromProjectLabel"));
    assert!(script.contains("upstreamBranchOptionsMatchRefs"));
    assert!(script.contains("upstreamBranchDefaultsCache = new Map()"));
    assert!(script.contains("actual-upstream-refs-v16"));
}

#[test]
fn manager_ui_exposes_pure_api_relay_mode_button() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("core crate should live under crates/codex-plus-core");
    let source = std::fs::read_to_string(repo.join("apps/codex-plus-manager/src/App.tsx")).unwrap();
    let commands =
        std::fs::read_to_string(repo.join("apps/codex-plus-manager/src-tauri/src/lib.rs")).unwrap();

    assert!(source.contains("官方混入 API Key"));
    assert!(source.contains("纯 API"));
    assert!(source.contains("apply_pure_api_injection"));
    assert!(commands.contains("commands::apply_pure_api_injection"));
}

#[test]
fn manager_ui_disables_plugin_auto_expand_in_compatible_mode() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("core crate should live under crates/codex-plus-core");
    let source = std::fs::read_to_string(repo.join("apps/codex-plus-manager/src/App.tsx")).unwrap();

    assert!(source.contains(
        "checked={form.codexAppPluginAutoExpand} disabled={!masterEnabled || !patchMode}"
    ));
}

#[test]
fn cdp_target_deserializes_websocket_field() {
    let target: CdpTarget = serde_json::from_value(json!({
        "id": "page-1",
        "type": "page",
        "title": "Codex",
        "url": "https://codex.test",
        "webSocketDebuggerUrl": "ws://debug",
    }))
    .expect("target should deserialize");

    assert_eq!(target.target_type, "page");
    assert_eq!(
        target.web_socket_debugger_url.as_deref(),
        Some("ws://debug")
    );
}

#[test]
fn runtime_evaluate_params_sets_expected_flags() {
    let params = bridge::runtime_evaluate_params("1 + 1");

    assert_eq!(params["expression"], "1 + 1");
    assert_eq!(params["awaitPromise"], false);
    assert_eq!(params["allowUnsafeEvalBlockedByCSP"], true);
}

#[test]
fn runtime_evaluate_params_can_await_promise_for_bridge_health_checks() {
    let params = bridge::runtime_evaluate_params_with_await_promise("Promise.resolve(true)", true);

    assert_eq!(params["expression"], "Promise.resolve(true)");
    assert_eq!(params["awaitPromise"], true);
    assert_eq!(params["allowUnsafeEvalBlockedByCSP"], true);
}

#[test]
fn bridge_health_check_script_uses_real_backend_round_trip() {
    let script = bridge::bridge_health_check_script();

    assert!(script.contains("__codexSessionDeleteBridge"));
    assert!(script.contains("/backend/status"));
    assert!(script.contains("Promise.race"));
    assert!(script.contains("setTimeout"));
}

#[test]
fn bridge_result_expressions_json_escape_inputs() {
    let resolve = bridge::resolve_bridge_expression("request\"1", &json!({"status": "ok"}))
        .expect("resolve expression should build");
    let reject = bridge::reject_bridge_expression("request\"1", "bad \"value\"")
        .expect("reject expression should build");

    assert_eq!(
        resolve,
        r#"window.__codexSessionDeleteResolve("request\"1", {"status":"ok"})"#
    );
    assert_eq!(
        reject,
        r#"window.__codexSessionDeleteReject("request\"1", "bad \"value\"")"#
    );
}

#[test]
fn pick_page_target_prefers_codex_title_or_url() {
    let targets = vec![
        target(
            "first",
            "page",
            "Other",
            "https://example.test",
            Some("ws://first"),
        ),
        target(
            "second",
            "page",
            "Codex",
            "https://example.test",
            Some("ws://second"),
        ),
        target(
            "third",
            "page",
            "Other",
            "https://codex.test",
            Some("ws://third"),
        ),
    ];

    let picked = pick_page_target(&targets).expect("target should be selected");

    assert_eq!(picked.id, "second");
}

#[test]
fn pick_page_target_leniently_falls_back_to_first_injectable_page() {
    let targets = vec![
        target(
            "browser",
            "browser",
            "Codex",
            "https://codex.test",
            Some("ws://browser"),
        ),
        target(
            "first",
            "page",
            "Other",
            "https://example.test",
            Some("ws://first"),
        ),
        target(
            "second",
            "page",
            "Other 2",
            "https://example.test/2",
            Some("ws://second"),
        ),
    ];

    let picked = pick_page_target(&targets).expect("target should be selected");

    assert_eq!(picked.id, "first");
}

#[test]
fn pick_page_target_rejects_non_pages_and_pages_without_websocket() {
    let targets = vec![
        target(
            "browser",
            "browser",
            "Codex",
            "https://codex.test",
            Some("ws://browser"),
        ),
        target("page-no-ws", "page", "Codex", "https://codex.test", None),
    ];

    let error = pick_page_target(&targets).expect_err("no injectable page should be selected");

    assert!(
        error
            .to_string()
            .contains("No injectable page target found")
    );
}

#[test]
fn pick_injectable_codex_page_target_rejects_non_codex_pages() {
    let targets = vec![
        target(
            "browser",
            "browser",
            "Codex",
            "https://codex.test",
            Some("ws://browser"),
        ),
        target(
            "other-page",
            "page",
            "Other App",
            "https://example.test",
            Some("ws://other"),
        ),
    ];

    let error = pick_injectable_codex_page_target(&targets)
        .expect_err("non-Codex page must not be selected for injection");

    assert!(
        error
            .to_string()
            .contains("No injectable Codex page target found")
    );
}

#[test]
fn pick_injectable_codex_page_target_requires_websocket() {
    let targets = vec![target("codex", "page", "Codex", "https://codex.test", None)];

    let error = pick_injectable_codex_page_target(&targets)
        .expect_err("Codex page without websocket must not be selected for injection");

    assert!(
        error
            .to_string()
            .contains("No injectable Codex page target found")
    );
}

#[test]
fn pick_injectable_codex_page_target_prefers_app_main_over_incidental_codex_page() {
    let targets = vec![
        target(
            "help",
            "page",
            "Using Codex with your ChatGPT plan",
            "https://help.openai.com/en/articles/using-codex",
            Some("ws://help"),
        ),
        target(
            "main",
            "page",
            "Codex",
            "app://-/index.html",
            Some("ws://main"),
        ),
    ];

    let picked = pick_injectable_codex_page_target(&targets)
        .expect("the exact app main renderer should win regardless of target order");

    assert_eq!(picked.id, "main");
}

#[test]
fn pick_injectable_codex_page_target_rejects_incidental_codex_web_page() {
    let targets = vec![target(
        "help",
        "page",
        "Using Codex with your ChatGPT plan",
        "https://help.openai.com/en/articles/using-codex",
        Some("ws://help"),
    )];

    pick_injectable_codex_page_target(&targets)
        .expect_err("ordinary web content must never become the injection target");
}

#[test]
fn pick_injectable_codex_page_target_accepts_supported_chatgpt_desktop_pages() {
    for (id, url) in [
        ("chatgpt", "https://chatgpt.com/"),
        (
            "chatgpt-error",
            "data:text/html;charset=utf-8,%3Ctitle%3EChatGPT%3C/title%3E",
        ),
    ] {
        let targets = vec![target(id, "page", "ChatGPT", url, Some("ws://chatgpt"))];

        let picked = pick_injectable_codex_page_target(&targets)
            .expect("supported ChatGPT desktop renderer should be selected");

        assert_eq!(picked.id, id);
    }
}

#[test]
fn avatar_overlay_target_detection_is_narrow() {
    let overlay = target(
        "avatar",
        "page",
        "ChatGPT Avatar Overlay",
        "app://-/index.html?initialRoute=%2Favatar-overlay",
        Some("ws://avatar"),
    );
    let main = target(
        "main",
        "page",
        "ChatGPT",
        "https://chatgpt.com/",
        Some("ws://main"),
    );

    assert!(is_avatar_overlay_page_target(&overlay));
    assert!(!is_primary_codex_page_target(&overlay));
    assert!(!is_avatar_overlay_page_target(&main));
    assert!(is_primary_codex_page_target(&main));
    assert!(!is_avatar_overlay_page_target(&target(
        "external",
        "page",
        "avatar-overlay",
        "https://example.test/avatar-overlay",
        Some("ws://external"),
    )));
}

#[test]
fn primary_target_selection_skips_overlay_candidates() {
    let targets = vec![
        target(
            "overlay",
            "page",
            "Codex",
            "app://-/index.html?initialRoute=%2Favatar-overlay",
            Some("ws://overlay"),
        ),
        target(
            "main",
            "page",
            "Codex",
            "app://-/index.html",
            Some("ws://main"),
        ),
    ];

    let selected = pick_injectable_codex_page_target(&targets).unwrap();

    assert_eq!(selected.id, "main");
}

#[test]
fn quick_chat_target_detection_is_narrow() {
    for url in [
        "app://-/index.html?initialRoute=%2Fchatgpt%2Fquick-chat",
        "app://-/index.html?initialRoute=/chatgpt/quick-chat-prewarm",
        "app://-/index.html?initialRoute=%2Fchatgpt%2Fquick-chat%2Fconversation-123",
    ] {
        let quick_chat = target("quick-chat", "page", "Codex", url, Some("ws://quick-chat"));

        assert!(is_quick_chat_page_target(&quick_chat));
        assert!(!is_primary_codex_page_target(&quick_chat));
    }

    assert!(!is_quick_chat_page_target(&target(
        "external",
        "page",
        "Codex",
        "https://example.test/chatgpt/quick-chat-prewarm",
        Some("ws://external"),
    )));
    assert!(!is_quick_chat_page_target(&target(
        "similar-route",
        "page",
        "Codex",
        "app://-/index.html?initialRoute=%2Fchatgpt%2Fquick-chatty",
        Some("ws://similar-route"),
    )));
}

#[test]
fn quick_chat_only_target_is_not_injectable_as_codex_main_page() {
    let targets = vec![target(
        "quick-chat",
        "page",
        "Codex",
        "app://-/index.html?initialRoute=%2Fchatgpt%2Fquick-chat-prewarm",
        Some("ws://quick-chat"),
    )];

    pick_injectable_codex_page_target(&targets)
        .expect_err("Quick Chat helper renderer must not be selected for injection");
}

#[tokio::test]
async fn list_targets_can_query_ipv6_loopback_cdp_endpoint() {
    let listener = TcpListener::bind("[::1]:0")
        .await
        .expect("IPv6 loopback listener should bind");
    let port = listener.local_addr().unwrap().port();
    let body = serde_json::to_vec(&json!([
        {
            "id": "page-1",
            "type": "page",
            "title": "Codex",
            "url": "app://-/index.html",
            "webSocketDebuggerUrl": format!("ws://[::1]:{port}/devtools/page/page-1"),
        }
    ]))
    .unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("request should arrive");
        let mut request = [0_u8; 1024];
        let _ = stream.readable().await;
        let _ = stream.try_read(&mut request);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream
            .try_write(response.as_bytes())
            .expect("response headers should write");
        stream.try_write(&body).expect("response body should write");
    });

    let targets = list_targets(port)
        .await
        .expect("CDP target query should fall back to IPv6 loopback");

    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].id, "page-1");
    server.await.expect("server task should complete");
}

#[tokio::test]
async fn install_bridge_routes_binding_while_waiting_for_command_response() {
    let temp = tempfile::tempdir().unwrap();
    let log_path = temp.path().join("codex-plus.log");
    codex_plus_core::diagnostic_log::set_diagnostic_log_path_for_tests(Some(log_path.clone()));
    let (url, request_rx) = spawn_cdp_server(|mut socket| async move {
        for expected_id in 1..=4 {
            let command = recv_json(&mut socket).await;
            assert_eq!(command["id"], expected_id);
            send_json(&mut socket, json!({ "id": expected_id, "result": {} })).await;
        }

        let evaluate = recv_json(&mut socket).await;
        assert_eq!(evaluate["id"], 5);
        assert_eq!(evaluate["method"], "Runtime.evaluate");
        send_json(
            &mut socket,
            json!({
                "method": "Runtime.bindingCalled",
                "params": {
                    "payload": serde_json::to_string(&json!({
                        "id": "request-1",
                        "path": "delete",
                        "payload": { "target": "session" },
                    })).unwrap(),
                },
            }),
        )
        .await;
        send_json(&mut socket, json!({ "id": 5, "result": {} })).await;

        let response = recv_json(&mut socket).await;
        assert_eq!(response["method"], "Runtime.evaluate");
        assert!(
            response["params"]["expression"]
                .as_str()
                .expect("expression should be string")
                .contains("__codexSessionDeleteResolve")
        );
        send_json(&mut socket, json!({ "id": response["id"], "result": {} })).await;
        close_socket(&mut socket).await;
    })
    .await;

    let handled = Arc::new(AtomicBool::new(false));
    let handler = {
        let handled = Arc::clone(&handled);
        Arc::new(move |path: String, payload: serde_json::Value| {
            let handled = Arc::clone(&handled);
            Box::pin(async move {
                assert_eq!(path, "delete");
                assert_eq!(payload["target"], "session");
                handled.store(true, Ordering::SeqCst);
                Ok(json!({ "status": "ok" }))
            })
                as Pin<Box<dyn Future<Output = anyhow::Result<serde_json::Value>> + Send>>
        })
    };

    tokio::time::timeout(
        Duration::from_secs(2),
        bridge::install_bridge(&url, BRIDGE_BINDING_NAME, handler, &[]),
    )
    .await
    .expect("bridge should not hang while processing interleaved binding call")
    .expect("bridge should keep processing interleaved binding call");
    request_rx
        .await
        .expect("server task should finish without panicking");
    assert!(handled.load(Ordering::SeqCst));
    let contents = std::fs::read_to_string(&log_path).unwrap();
    assert!(contents.contains("bridge.resolve_start"));
    assert!(contents.contains("bridge.resolve_ok"));
    codex_plus_core::diagnostic_log::set_diagnostic_log_path_for_tests(None);
}

#[tokio::test]
async fn install_bridge_immediately_evaluates_new_document_scripts() {
    let (url, request_rx) = spawn_cdp_server(|mut socket| async move {
        for expected_id in 1..=5 {
            let command = recv_json(&mut socket).await;
            assert_eq!(command["id"], expected_id);
            send_json(&mut socket, json!({ "id": expected_id, "result": {} })).await;
        }

        let add_main = recv_json(&mut socket).await;
        assert_eq!(add_main["method"], "Page.addScriptToEvaluateOnNewDocument");
        assert_eq!(add_main["params"]["source"], "window.mainInjected = true;");
        send_json(&mut socket, json!({ "id": add_main["id"], "result": {} })).await;

        let eval_main = recv_json(&mut socket).await;
        assert_eq!(eval_main["method"], "Runtime.evaluate");
        assert_eq!(
            eval_main["params"]["expression"],
            "window.mainInjected = true;"
        );
        send_json(&mut socket, json!({ "id": eval_main["id"], "result": {} })).await;

        let add_user = recv_json(&mut socket).await;
        assert_eq!(add_user["method"], "Page.addScriptToEvaluateOnNewDocument");
        assert_eq!(add_user["params"]["source"], "window.userInjected = true;");
        send_json(&mut socket, json!({ "id": add_user["id"], "result": {} })).await;

        let eval_user = recv_json(&mut socket).await;
        assert_eq!(eval_user["method"], "Runtime.evaluate");
        assert_eq!(
            eval_user["params"]["expression"],
            "window.userInjected = true;"
        );
        send_json(&mut socket, json!({ "id": eval_user["id"], "result": {} })).await;

        close_socket(&mut socket).await;
    })
    .await;

    tokio::time::timeout(
        Duration::from_secs(2),
        bridge::install_bridge(
            &url,
            BRIDGE_BINDING_NAME,
            noop_handler(),
            &[
                "window.mainInjected = true;".to_string(),
                "window.userInjected = true;".to_string(),
            ],
        ),
    )
    .await
    .expect("bridge should not hang while evaluating new document scripts")
    .expect("bridge should evaluate new document scripts immediately");
    request_rx
        .await
        .expect("server task should finish without panicking");
}

#[tokio::test]
async fn install_bridge_returns_after_installing_and_keeps_message_pump_alive() {
    let (url, request_rx) = spawn_cdp_server(|mut socket| async move {
        for expected_id in 1..=5 {
            let command = recv_json(&mut socket).await;
            assert_eq!(command["id"], expected_id);
            send_json(&mut socket, json!({ "id": expected_id, "result": {} })).await;
        }

        let add_script = recv_json(&mut socket).await;
        assert_eq!(
            add_script["method"],
            "Page.addScriptToEvaluateOnNewDocument"
        );
        send_json(&mut socket, json!({ "id": add_script["id"], "result": {} })).await;

        let eval_script = recv_json(&mut socket).await;
        assert_eq!(eval_script["method"], "Runtime.evaluate");
        send_json(
            &mut socket,
            json!({ "id": eval_script["id"], "result": {} }),
        )
        .await;

        send_json(
            &mut socket,
            json!({
                "method": "Runtime.bindingCalled",
                "params": {
                    "payload": serde_json::to_string(&json!({
                        "id": "after-return",
                        "path": "status",
                        "payload": {},
                    })).unwrap(),
                },
            }),
        )
        .await;

        let resolve = recv_json(&mut socket).await;
        assert!(
            resolve["params"]["expression"]
                .as_str()
                .expect("expression should be string")
                .contains("after-return")
        );
        send_json(&mut socket, json!({ "id": resolve["id"], "result": {} })).await;
        close_socket(&mut socket).await;
    })
    .await;

    let handled = Arc::new(AtomicBool::new(false));
    let handler = {
        let handled = Arc::clone(&handled);
        Arc::new(move |_path: String, _payload: serde_json::Value| {
            let handled = Arc::clone(&handled);
            Box::pin(async move {
                handled.store(true, Ordering::SeqCst);
                Ok(json!({ "status": "ok" }))
            })
                as Pin<Box<dyn Future<Output = anyhow::Result<serde_json::Value>> + Send>>
        })
    };

    tokio::time::timeout(
        Duration::from_secs(2),
        bridge::install_bridge(
            &url,
            BRIDGE_BINDING_NAME,
            handler,
            &["window.ready = true;".to_string()],
        ),
    )
    .await
    .expect("bridge install should return after setup")
    .expect("bridge install should succeed");

    request_rx
        .await
        .expect("server task should finish without panicking");
    assert!(handled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn install_bridge_command_error_mentions_method_and_id() {
    let (url, request_rx) = spawn_cdp_server(|mut socket| async move {
        let command = recv_json(&mut socket).await;
        assert_eq!(command["method"], "Runtime.enable");
        send_json(
            &mut socket,
            json!({
                "id": command["id"],
                "error": { "code": -32000, "message": "Runtime disabled" },
            }),
        )
        .await;
        close_socket(&mut socket).await;
    })
    .await;

    let handler = noop_handler();
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        bridge::install_bridge(&url, BRIDGE_BINDING_NAME, handler, &[]),
    )
    .await
    .expect("bridge should not hang on CDP error response")
    .expect_err("CDP error response should fail install");
    let message = error.to_string();

    request_rx
        .await
        .expect("server task should finish without panicking");
    assert!(message.contains("Runtime.enable"), "{message}");
    assert!(message.contains("id 1"), "{message}");
    assert!(message.contains("Runtime disabled"), "{message}");
}

#[tokio::test]
async fn install_bridge_rejects_bad_payload_with_id_and_continues_after_unparseable_payload() {
    let (url, request_rx) = spawn_cdp_server(|mut socket| async move {
        for expected_id in 1..=5 {
            let command = recv_json(&mut socket).await;
            assert_eq!(command["id"], expected_id);
            send_json(&mut socket, json!({ "id": expected_id, "result": {} })).await;
        }

        send_json(
            &mut socket,
            json!({
                "method": "Runtime.bindingCalled",
                "params": { "payload": "{\"id\":\"bad-1\",\"payload\":{}" },
            }),
        )
        .await;
        send_json(
            &mut socket,
            json!({
                "method": "Runtime.bindingCalled",
                "params": { "payload": "not json" },
            }),
        )
        .await;
        send_json(
            &mut socket,
            json!({
                "method": "Runtime.bindingCalled",
                "params": {
                    "payload": serde_json::to_string(&json!({
                        "id": "ok-1",
                        "path": "delete",
                        "payload": {},
                    })).unwrap(),
                },
            }),
        )
        .await;

        let reject = recv_json(&mut socket).await;
        assert!(
            reject["params"]["expression"]
                .as_str()
                .expect("expression should be string")
                .contains("__codexSessionDeleteReject")
        );
        assert!(
            reject["params"]["expression"]
                .as_str()
                .expect("expression should be string")
                .contains("bad-1")
        );
        send_json(&mut socket, json!({ "id": reject["id"], "result": {} })).await;

        let resolve = recv_json(&mut socket).await;
        assert!(
            resolve["params"]["expression"]
                .as_str()
                .expect("expression should be string")
                .contains("__codexSessionDeleteResolve")
        );
        assert!(
            resolve["params"]["expression"]
                .as_str()
                .expect("expression should be string")
                .contains("ok-1")
        );
        send_json(&mut socket, json!({ "id": resolve["id"], "result": {} })).await;
        close_socket(&mut socket).await;
    })
    .await;

    tokio::time::timeout(
        Duration::from_secs(2),
        bridge::install_bridge(&url, BRIDGE_BINDING_NAME, noop_handler(), &[]),
    )
    .await
    .expect("bridge should not hang after bad payload")
    .expect("bad payloads should not terminate the bridge loop");
    request_rx
        .await
        .expect("server task should finish without panicking");
}

#[tokio::test]
async fn install_bridge_queues_consecutive_bindings_without_recursive_dispatch() {
    let (url, request_rx) = spawn_cdp_server(|mut socket| async move {
        for expected_id in 1..=5 {
            let command = recv_json(&mut socket).await;
            assert_eq!(command["id"], expected_id);
            send_json(&mut socket, json!({ "id": expected_id, "result": {} })).await;
        }

        for request_id in ["first", "second", "third"] {
            send_json(
                &mut socket,
                json!({
                    "method": "Runtime.bindingCalled",
                    "params": {
                        "payload": serde_json::to_string(&json!({
                            "id": request_id,
                            "path": "delete",
                            "payload": { "request": request_id },
                        })).unwrap(),
                    },
                }),
            )
            .await;
        }

        let first = recv_json(&mut socket).await;
        assert_eq!(first["method"], "Runtime.evaluate");
        assert_expression_contains_request(&first, "first");
        let second = recv_json(&mut socket).await;
        assert_eq!(second["method"], "Runtime.evaluate");
        assert_expression_contains_request(&second, "second");
        assert_ne!(second["id"], first["id"]);

        let third = recv_json(&mut socket).await;
        assert_eq!(third["method"], "Runtime.evaluate");
        assert_expression_contains_request(&third, "third");
        assert_ne!(third["id"], first["id"]);
        assert_ne!(third["id"], second["id"]);

        close_socket(&mut socket).await;
    })
    .await;

    let handler = Arc::new(|_path: String, payload: serde_json::Value| {
        Box::pin(async move { Ok(json!({ "status": "ok", "request": payload["request"] })) })
            as Pin<Box<dyn Future<Output = anyhow::Result<serde_json::Value>> + Send>>
    });

    tokio::time::timeout(
        Duration::from_secs(2),
        bridge::install_bridge(&url, BRIDGE_BINDING_NAME, handler, &[]),
    )
    .await
    .expect("bridge should not hang while draining queued binding calls")
    .expect("bridge should process queued binding calls");
    request_rx
        .await
        .expect("server task should finish without panicking");
}

#[tokio::test]
async fn install_bridge_does_not_wait_for_resolve_runtime_evaluate_ack() {
    let (url, request_rx) = spawn_cdp_server(|mut socket| async move {
        for expected_id in 1..=5 {
            let command = recv_json(&mut socket).await;
            assert_eq!(command["id"], expected_id);
            send_json(&mut socket, json!({ "id": expected_id, "result": {} })).await;
        }

        send_json(
            &mut socket,
            json!({
                "method": "Runtime.bindingCalled",
                "params": {
                    "payload": serde_json::to_string(&json!({
                        "id": "first",
                        "path": "/backend/status",
                        "payload": {},
                    })).unwrap(),
                },
            }),
        )
        .await;
        let first_resolve = recv_json(&mut socket).await;
        assert_eq!(first_resolve["method"], "Runtime.evaluate");
        assert_expression_contains_request(&first_resolve, "first");

        send_json(
            &mut socket,
            json!({
                "method": "Runtime.bindingCalled",
                "params": {
                    "payload": serde_json::to_string(&json!({
                        "id": "second",
                        "path": "/backend/status",
                        "payload": {},
                    })).unwrap(),
                },
            }),
        )
        .await;
        let second_resolve =
            tokio::time::timeout(Duration::from_millis(500), recv_json(&mut socket))
                .await
                .expect(
                    "second resolve should be sent without waiting for first Runtime.evaluate ack",
                );
        assert_eq!(second_resolve["method"], "Runtime.evaluate");
        assert_expression_contains_request(&second_resolve, "second");
        close_socket(&mut socket).await;
    })
    .await;

    let handler = Arc::new(|_path: String, _payload: serde_json::Value| {
        Box::pin(async { Ok(json!({ "status": "ok" })) })
            as Pin<Box<dyn Future<Output = anyhow::Result<serde_json::Value>> + Send>>
    });

    tokio::time::timeout(
        Duration::from_secs(2),
        bridge::install_bridge(&url, BRIDGE_BINDING_NAME, handler, &[]),
    )
    .await
    .expect("bridge install should not wait for resolve ack")
    .expect("bridge install should survive missing resolve ack");
    request_rx
        .await
        .expect("server task should finish without panicking");
}

#[tokio::test]
async fn install_bridge_keeps_status_responsive_while_generate_is_pending() {
    let generate_started = Arc::new(Notify::new());
    let release_generate = Arc::new(Notify::new());
    let server_generate_started = Arc::clone(&generate_started);
    let server_release_generate = Arc::clone(&release_generate);
    let (url, request_rx) = spawn_cdp_server(move |mut socket| async move {
        acknowledge_bridge_install(&mut socket).await;

        send_json(
            &mut socket,
            json!({
                "method": "Runtime.bindingCalled",
                "params": {
                    "payload": serde_json::to_string(&json!({
                        "id": "generate",
                        "path": "/stepwise/generate",
                        "payload": {},
                    })).unwrap(),
                },
            }),
        )
        .await;
        tokio::time::timeout(
            Duration::from_millis(500),
            server_generate_started.notified(),
        )
        .await
        .expect("generate handler should start before the status probe");

        send_json(
            &mut socket,
            json!({
                "method": "Runtime.bindingCalled",
                "params": {
                    "payload": serde_json::to_string(&json!({
                        "id": "status",
                        "path": "/backend/status",
                        "payload": {},
                    })).unwrap(),
                },
            }),
        )
        .await;

        let status_resolve =
            tokio::time::timeout(Duration::from_millis(500), recv_json(&mut socket))
                .await
                .expect("status should resolve while generate remains pending");
        assert_eq!(status_resolve["method"], "Runtime.evaluate");
        assert_expression_contains_request(&status_resolve, "status");

        server_release_generate.notify_one();
        let generate_resolve = recv_json(&mut socket).await;
        assert_eq!(generate_resolve["method"], "Runtime.evaluate");
        assert_expression_contains_request(&generate_resolve, "generate");
        close_socket(&mut socket).await;
    })
    .await;

    let handler_generate_started = Arc::clone(&generate_started);
    let handler_release_generate = Arc::clone(&release_generate);
    let handler = Arc::new(move |path: String, _payload: serde_json::Value| {
        let generate_started = Arc::clone(&handler_generate_started);
        let release_generate = Arc::clone(&handler_release_generate);
        Box::pin(async move {
            if path == "/stepwise/generate" {
                generate_started.notify_one();
                release_generate.notified().await;
            }
            Ok(json!({ "status": "ok", "path": path }))
        }) as Pin<Box<dyn Future<Output = anyhow::Result<serde_json::Value>> + Send>>
    });

    tokio::time::timeout(
        Duration::from_secs(2),
        bridge::install_bridge(&url, BRIDGE_BINDING_NAME, handler, &[]),
    )
    .await
    .expect("bridge install should return while generate is pending")
    .expect("bridge install should start the concurrent message pump");
    request_rx
        .await
        .expect("server task should finish without panicking");
}

#[tokio::test]
async fn superseded_bridge_session_stops_answering_binding_calls() {
    let (url, stale_rx, fresh_rx) = spawn_two_session_cdp_server().await;

    bridge::install_bridge(&url, BRIDGE_BINDING_NAME, noop_handler(), &[])
        .await
        .expect("first bridge install should succeed");
    bridge::install_bridge(&url, BRIDGE_BINDING_NAME, noop_handler(), &[])
        .await
        .expect("second bridge install should succeed");

    let stale_resolved = stale_rx
        .await
        .expect("stale server task should finish without panicking");
    assert!(
        !stale_resolved,
        "superseded session must not resolve bridge requests"
    );
    fresh_rx
        .await
        .expect("fresh server task should finish without panicking");
}

#[tokio::test]
async fn superseded_idle_bridge_session_closes_without_inbound_message() {
    let (url, stale_closed_rx, fresh_rx) = spawn_two_idle_session_cdp_server().await;

    bridge::install_bridge(&url, BRIDGE_BINDING_NAME, noop_handler(), &[])
        .await
        .expect("first bridge install should succeed");
    bridge::install_bridge(&url, BRIDGE_BINDING_NAME, noop_handler(), &[])
        .await
        .expect("second bridge install should succeed");

    assert!(
        stale_closed_rx
            .await
            .expect("stale server task should finish without panicking"),
        "superseded idle session must close without waiting for inbound CDP traffic"
    );
    fresh_rx
        .await
        .expect("fresh server task should finish without panicking");
}

#[tokio::test]
async fn failed_bridge_reinstall_keeps_existing_session_current() {
    let (url, active_rx, failed_rx) = spawn_failed_reinstall_cdp_server().await;

    bridge::install_bridge(&url, BRIDGE_BINDING_NAME, noop_handler(), &[])
        .await
        .expect("first bridge install should succeed");
    let error = bridge::install_bridge(&url, BRIDGE_BINDING_NAME, noop_handler(), &[])
        .await
        .expect_err("second bridge install should fail");
    assert!(error.to_string().contains("Runtime.addBinding"));

    assert!(
        active_rx
            .await
            .expect("active server task should finish without panicking"),
        "failed reinstall must not supersede the existing bridge session"
    );
    failed_rx
        .await
        .expect("failed reinstall server task should finish without panicking");
}

type TestSocket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

async fn spawn_two_session_cdp_server() -> (String, oneshot::Receiver<bool>, oneshot::Receiver<()>)
{
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let address = listener.local_addr().expect("listener should have address");
    let (stale_tx, stale_rx) = oneshot::channel();
    let (fresh_tx, fresh_rx) = oneshot::channel();

    tokio::spawn(async move {
        let (stale_stream, _) = listener
            .accept()
            .await
            .expect("stale client should connect");
        let mut stale = accept_async(stale_stream)
            .await
            .expect("stale websocket should upgrade");
        acknowledge_bridge_install(&mut stale).await;

        let (fresh_stream, _) = listener
            .accept()
            .await
            .expect("fresh client should connect");
        let mut fresh = accept_async(fresh_stream)
            .await
            .expect("fresh websocket should upgrade");
        acknowledge_bridge_install(&mut fresh).await;

        send_json(
            &mut stale,
            json!({
                "method": "Runtime.bindingCalled",
                "params": {
                    "payload": serde_json::to_string(&json!({
                        "id": "stale",
                        "path": "/backend/status",
                        "payload": {},
                    })).unwrap(),
                },
            }),
        )
        .await;

        let stale_resolved =
            tokio::time::timeout(Duration::from_millis(500), recv_text_message(&mut stale))
                .await
                .is_ok_and(|message| message.is_some_and(|text| text.contains("Runtime.evaluate")));
        let _ = stale_tx.send(stale_resolved);
        close_socket(&mut fresh).await;
        let _ = fresh_tx.send(());
    });

    (websocket_url(address), stale_rx, fresh_rx)
}

async fn spawn_two_idle_session_cdp_server()
-> (String, oneshot::Receiver<bool>, oneshot::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let address = listener.local_addr().expect("listener should have address");
    let (stale_closed_tx, stale_closed_rx) = oneshot::channel();
    let (fresh_tx, fresh_rx) = oneshot::channel();

    tokio::spawn(async move {
        let (stale_stream, _) = listener
            .accept()
            .await
            .expect("stale client should connect");
        let mut stale = accept_async(stale_stream)
            .await
            .expect("stale websocket should upgrade");
        acknowledge_bridge_install(&mut stale).await;

        let (fresh_stream, _) = listener
            .accept()
            .await
            .expect("fresh client should connect");
        let mut fresh = accept_async(fresh_stream)
            .await
            .expect("fresh websocket should upgrade");
        acknowledge_bridge_install(&mut fresh).await;

        let stale_closed = tokio::time::timeout(Duration::from_secs(2), stale.next())
            .await
            .is_ok_and(|message| {
                message.is_none_or(|message| message.is_ok_and(|message| message.is_close()))
            });
        let _ = stale_closed_tx.send(stale_closed);
        close_socket(&mut fresh).await;
        let _ = fresh_tx.send(());
    });

    (websocket_url(address), stale_closed_rx, fresh_rx)
}

async fn spawn_failed_reinstall_cdp_server()
-> (String, oneshot::Receiver<bool>, oneshot::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let address = listener.local_addr().expect("listener should have address");
    let (active_tx, active_rx) = oneshot::channel();
    let (failed_tx, failed_rx) = oneshot::channel();

    tokio::spawn(async move {
        let (active_stream, _) = listener
            .accept()
            .await
            .expect("active client should connect");
        let mut active = accept_async(active_stream)
            .await
            .expect("active websocket should upgrade");
        acknowledge_bridge_install(&mut active).await;

        let (failed_stream, _) = listener
            .accept()
            .await
            .expect("failed client should connect");
        let mut failed = accept_async(failed_stream)
            .await
            .expect("failed websocket should upgrade");
        for expected_id in 1..=2 {
            let command = recv_json(&mut failed).await;
            assert_eq!(command["id"], expected_id);
            send_json(&mut failed, json!({ "id": expected_id, "result": {} })).await;
        }
        let add_binding = recv_json(&mut failed).await;
        assert_eq!(add_binding["id"], 3);
        assert_eq!(add_binding["method"], "Runtime.addBinding");
        send_json(
            &mut failed,
            json!({
                "id": 3,
                "error": { "code": -32000, "message": "binding install failed" }
            }),
        )
        .await;
        close_socket(&mut failed).await;
        let _ = failed_tx.send(());

        send_json(
            &mut active,
            json!({
                "method": "Runtime.bindingCalled",
                "params": {
                    "payload": serde_json::to_string(&json!({
                        "id": "active",
                        "path": "/backend/status",
                        "payload": {},
                    })).unwrap(),
                },
            }),
        )
        .await;
        let active_resolved =
            tokio::time::timeout(Duration::from_millis(500), recv_json(&mut active))
                .await
                .is_ok_and(|message| {
                    message["method"] == "Runtime.evaluate"
                        && message["params"]["expression"]
                            .as_str()
                            .is_some_and(|expression| expression.contains("active"))
                });
        let _ = active_tx.send(active_resolved);
        close_socket(&mut active).await;
    });

    (websocket_url(address), active_rx, failed_rx)
}

async fn acknowledge_bridge_install(socket: &mut TestSocket) {
    for expected_id in 1..=5 {
        let command = recv_json(socket).await;
        assert_eq!(command["id"], expected_id);
        send_json(socket, json!({ "id": expected_id, "result": {} })).await;
    }
}

async fn recv_text_message(socket: &mut TestSocket) -> Option<String> {
    match socket.next().await {
        Some(Ok(Message::Text(text))) => Some(text.to_string()),
        _ => None,
    }
}

async fn spawn_cdp_server<F, Fut>(handler: F) -> (String, oneshot::Receiver<()>)
where
    F: FnOnce(TestSocket) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let address = listener.local_addr().expect("listener should have address");
    let (done_tx, done_rx) = oneshot::channel();

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("client should connect");
        let socket = accept_async(stream)
            .await
            .expect("websocket should upgrade");
        handler(socket).await;
        let _ = done_tx.send(());
    });

    (websocket_url(address), done_rx)
}

fn websocket_url(address: SocketAddr) -> String {
    format!("ws://{address}")
}

async fn recv_json(socket: &mut TestSocket) -> serde_json::Value {
    let message = socket
        .next()
        .await
        .expect("client should send message")
        .expect("message should be readable");
    let Message::Text(text) = message else {
        panic!("expected text websocket message");
    };
    serde_json::from_str(&text).expect("message should be JSON")
}

async fn send_json(socket: &mut TestSocket, value: serde_json::Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .expect("message should send");
}

fn assert_expression_contains_request(command: &serde_json::Value, request_id: &str) {
    let expression = command["params"]["expression"]
        .as_str()
        .expect("expression should be string");
    assert!(
        expression.contains("__codexSessionDeleteResolve"),
        "{expression}"
    );
    assert!(expression.contains(request_id), "{expression}");
}

async fn close_socket(socket: &mut TestSocket) {
    socket.close(None).await.expect("websocket should close");
    let _ = tokio::time::timeout(Duration::from_millis(200), socket.next()).await;
}

fn noop_handler() -> bridge::BridgeHandler {
    Arc::new(|_, _| {
        Box::pin(async { Ok(json!({ "status": "ok" })) })
            as Pin<Box<dyn Future<Output = anyhow::Result<serde_json::Value>> + Send>>
    })
}
