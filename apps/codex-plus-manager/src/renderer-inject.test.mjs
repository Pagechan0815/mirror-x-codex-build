import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { readFile } from "node:fs/promises";

function sourceSection(source, startMarker, endMarker) {
  const start = source.indexOf(startMarker);
  const end = source.indexOf(endMarker, start + startMarker.length);
  assert.notEqual(start, -1, `missing source marker: ${startMarker}`);
  assert.notEqual(end, -1, `missing source marker: ${endMarker}`);
  return source.slice(start, end);
}

describe("renderer injection header compatibility", () => {
  it("refuses to run in embedded or non-Codex renderer documents", async () => {
    const renderer = await readFile(
      new URL("../../../assets/inject/renderer-inject.js", import.meta.url),
      "utf8",
    );

    assert.match(renderer, /window\.top !== window \|\| window\.self !== window/);
    assert.match(renderer, /!window\.electronBridge/);
    assert.match(renderer, /!\/\^app:/);
  });

  it("anchors the Mirror X Codex menu to current and legacy application top bars only", async () => {
    const renderer = await readFile(
      new URL("../../../assets/inject/renderer-inject.js", import.meta.url),
      "utf8",
    );

    assert.match(
      renderer,
      /appHeader:\s*'[^"]*\[class\*="ApplicationMenuTopBar"\][^']*\.app-header-tint'/,
    );
    assert.doesNotMatch(renderer, /document\.querySelector\(["']header["']\)/);
    assert.match(
      renderer,
      /isApplicationMenuTopBar\s*\?\s*Math\.max\(4, headerRect\.top\)/,
    );
    assert.match(
      renderer,
      /isApplicationMenuTopBar\s*\?\s*28\s*:\s*headerRect\.height/,
    );
  });

  it("reinstalls the current pure API session routing patch", async () => {
    const renderer = await readFile(
      new URL("../../../assets/inject/renderer-inject.js", import.meta.url),
      "utf8",
    );

    assert.match(
      renderer,
      /codexAppServerModelRequestPatchVersion\s*=\s*"15"/,
    );
    assert.match(renderer, /codexModelJsonResponsePatchVersion\s*=\s*"5"/);
    assert.match(renderer, /codexModelMessagePatchVersion\s*=\s*"3"/);
    assert.match(renderer, /function codexRelayConfigModelProvider/);
    assert.match(renderer, /const fromConfig = codexRelayConfigModelProvider\(profile\?\.configContents/);
    assert.match(renderer, /requestedProvider\s*===\s*"custom"/);
    assert.match(renderer, /deferNewThreadProviderToValidatedConfig/);
    assert.match(renderer, /remote_session_provider_deferred_to_config/);
    assert.doesNotMatch(renderer, /installAppServerModelDispatcherPatch/);
    assert.doesNotMatch(renderer, /model_dispatcher_patch_installed/);
  });

  it("bounds failed app asset discovery instead of rescanning forever", async () => {
    const renderer = await readFile(
      new URL("../../../assets/inject/renderer-inject.js", import.meta.url),
      "utf8",
    );

    assert.match(renderer, /codexAppModuleRetryCooldownMs\s*=\s*30000/);
    assert.match(renderer, /codexAppModuleMaxAttempts\s*=\s*8/);
    assert.match(renderer, /serviceTierDispatcherPatchPromise/);
    assert.match(renderer, /pluginMarketplaceRequestPatchPromise/);
    assert.match(renderer, /plugin_marketplace_request_patch_skipped/);
    assert.match(renderer, /changedElements\.every\(isExtensionUiNode\)/);
  });

  it("resolves pure, mixed, and catalog fallback providers like upstream", async () => {
    const renderer = await readFile(
      new URL("../../../assets/inject/renderer-inject.js", import.meta.url),
      "utf8",
    );
    const providerRoutingSource = sourceSection(
      renderer,
      "function codexRemoteSessionActiveProfile",
      "function deferNewThreadProviderToValidatedConfig",
    );
    const createProviderRouting = (settings, catalog = {}) => {
      const diagnostics = [];
      const routing = Function(
        "codexPlusBackendSettings",
        "codexModelCatalog",
        "sendCodexPlusDiagnostic",
        `"use strict"; ${providerRoutingSource}\nreturn {
          target: codexRemoteSessionTargetProvider,
          enabled: codexRemoteSessionProviderOverrideEnabled,
          apply: applyCodexRemoteSessionProviderOverride,
        };`,
      )(
        settings,
        catalog,
        (event, detail) => diagnostics.push({ event, detail }),
      );
      return { ...routing, diagnostics };
    };
    const profileSettings = (relayMode, extra = {}, profileExtra = {}) => ({
      relayProfilesEnabled: true,
      activeRelayId: "relay",
      relayProfiles: [{ id: "relay", relayMode, ...profileExtra }],
      ...extra,
    });

    const pure = createProviderRouting(profileSettings("pureApi", {
      activeRelaySessionProvider: "",
      activeRelayCodexProvider: "",
    }));
    assert.equal(pure.target(), "custom");
    assert.equal(pure.enabled(), true);
    assert.deepEqual(
      pure.apply("thread/start", { modelProvider: "openai", model: "gpt-5.6-sol" }),
      { modelProvider: "custom", model: "gpt-5.6-sol" },
    );
    assert.equal(pure.diagnostics.at(-1)?.detail?.to, "custom");

    const namedPure = createProviderRouting(profileSettings("pureApi", {
      activeRelayCodexProvider: "stale-provider",
    }, {
      configContents: 'model = "deepseek-v4"\nmodel_provider = "deepseek"\n[model_providers.deepseek]',
    }));
    assert.equal(namedPure.target(), "deepseek");
    assert.deepEqual(
      namedPure.apply("thread/start", { modelProvider: "openai" }),
      { modelProvider: "deepseek" },
    );

    const stalePure = createProviderRouting(profileSettings("pureApi", {
      activeRelayCodexProvider: "stale-provider",
    }));
    assert.equal(stalePure.target(), "custom");

    const mixed = createProviderRouting(
      profileSettings("mixedApi", {
        activeRelaySessionProvider: "custom",
        activeRelayCodexProvider: "enterprise",
      }),
      { codex_model_provider: "catalog-provider" },
    );
    assert.equal(mixed.target(), "enterprise");
    assert.deepEqual(
      mixed.apply("thread/start", { model_provider: "openai" }),
      { modelProvider: "enterprise" },
    );

    const catalogFallbackSettings = profileSettings("mixedApi", {
      activeRelaySessionProvider: "custom",
      activeRelayCodexProvider: "",
    });
    assert.equal(
      createProviderRouting(catalogFallbackSettings, { codex_model_provider: "catalog-snake" }).target(),
      "catalog-snake",
    );
    assert.equal(
      createProviderRouting(catalogFallbackSettings, { codexModelProvider: "catalog-camel" }).target(),
      "catalog-camel",
    );
    assert.equal(
      createProviderRouting(catalogFallbackSettings, { model_provider: "catalog-generic" }).target(),
      "catalog-generic",
    );
    assert.equal(
      createProviderRouting(catalogFallbackSettings, { modelProvider: "catalog-generic-camel" }).target(),
      "catalog-generic-camel",
    );
  });

  it("normalizes current and legacy model methods while keeping bootstrap requests outside enhancement waits", async () => {
    const renderer = await readFile(
      new URL("../../../assets/inject/renderer-inject.js", import.meta.url),
      "utf8",
    );
    const requestMethodSource = sourceSection(
      renderer,
      "function appServerModelRequestMethod",
      "function codexOfficialBootstrapRequest",
    );
    const requestMethod = Function(`"use strict"; return (${requestMethodSource.trim()});`)();
    const bootstrapSource = sourceSection(
      renderer,
      "function codexOfficialBootstrapRequest",
      "function patchAppServerModelResult",
    );
    const isBootstrapRequest = Function(`"use strict"; return (${bootstrapSource.trim()});`)();

    assert.equal(
      requestMethod("send-cli-request-for-host", { method: "list-models-for-host" }),
      "list-models-for-host",
    );
    assert.equal(
      requestMethod("send-cli-request-for-host", { method: "model/list" }),
      "list-models-for-host",
    );
    assert.equal(requestMethod("model/list", {}), "list-models-for-host");
    assert.equal(
      requestMethod("vscode://codex/model/list?includeHidden=true", {}),
      "list-models-for-host",
    );
    assert.equal(
      requestMethod("send-cli-request-for-host", {
        method: "vscode://codex/model/list?includeHidden=true",
      }),
      "list-models-for-host",
    );
    assert.equal(requestMethod("vscode://codex/list-plugins", {}), "list-plugins");
    assert.equal(isBootstrapRequest("initialize"), true);
    assert.equal(isBootstrapRequest("configRequirements/read"), true);
    assert.equal(isBootstrapRequest("windowsSandbox/readiness"), true);
    assert.equal(isBootstrapRequest("windowsSandbox/setupStart"), true);
    assert.equal(isBootstrapRequest("config/read"), true);
    assert.equal(isBootstrapRequest("thread/start"), false);
    assert.equal(isBootstrapRequest("model/list"), false);

    const requestPatch = sourceSection(
      renderer,
      "function patchAppServerModelRequestClient",
      "const appServerModelRequestPatchMaxMisses",
    );
    assert.match(
      requestPatch,
      /if \(codexOfficialBootstrapRequest\(requestMethod\)\) \{\s*return originalSendRequest\(method, params, options\);/,
    );
    assert.match(
      requestPatch,
      /if \(!providerRequest && !modelListRequest\) \{\s*return originalSendRequest\(method, params, options\);/,
    );
    assert.match(
      requestPatch,
      /if \(!codexPlusModelNames\(\)\.length\) await loadCodexModelCatalog\(true\);/,
    );

    let catalogLoadCalls = 0;
    let catalogReady = false;
    let resolveCatalogLoad;
    const catalogLoadGate = new Promise((resolve) => {
      resolveCatalogLoad = () => {
        catalogReady = true;
        resolve();
      };
    });
    let patchedModelListCalls = 0;
    const patchRequestClient = Function(
      "codexAppServerModelRequestPatchVersion",
      "appServerModelRequestMethod",
      "codexOfficialBootstrapRequest",
      "codexRemoteSessionProviderRequestMethod",
      "codexPlusBackendSettingsLoaded",
      "codexRemoteSessionProviderPatchEnabled",
      "refreshBackendSettingsForProviderRequest",
      "sendCodexPlusDiagnostic",
      "deferNewThreadProviderToValidatedConfig",
      "applyCodexRemoteSessionProviderOverride",
      "codexPlusModelUnlockEnabled",
      "codexPlusModelNames",
      "loadCodexModelCatalog",
      "patchAppServerModelResult",
      `"use strict"; return (${requestPatch.trim()});`,
    )(
      "15",
      requestMethod,
      isBootstrapRequest,
      (method) => method === "thread/start",
      true,
      () => true,
      () => new Promise(() => {}),
      () => {},
      (_method, params) => params,
      (_method, params) => params,
      () => true,
      () => (catalogReady ? ["gpt-5.6-sol"] : []),
      async () => {
        catalogLoadCalls += 1;
        await catalogLoadGate;
      },
      (method, result) => {
        patchedModelListCalls += 1;
        return { ...result, requestMethod: method, models: ["gpt-5.6-sol"] };
      },
    );
    const originalCalls = [];
    const client = {
      async sendRequest(method, params, options) {
        originalCalls.push({ method, params, options });
        if (method === "model/list"
            || method.startsWith("vscode://codex/model/list")
            || (method === "send-cli-request-for-host"
              && ["model/list", "list-models-for-host"].includes(params?.method))) {
          return { data: [], nextCursor: null, method };
        }
        return { status: "official", method };
      },
    };
    assert.equal(patchRequestClient(client), true);

    let providerRequestSettled = false;
    void client.sendRequest("thread/start", { modelProvider: "openai" }).then(
      () => { providerRequestSettled = true; },
      () => { providerRequestSettled = true; },
    );
    await Promise.resolve();
    assert.equal(providerRequestSettled, false);

    const bootstrapResult = await Promise.race([
      client.sendRequest("configRequirements/read", { cwd: "C:/workspace" }),
      new Promise((resolve) => setTimeout(() => resolve("blocked"), 100)),
    ]);
    assert.deepEqual(bootstrapResult, { status: "official", method: "configRequirements/read" });
    assert.equal(catalogLoadCalls, 0);
    assert.equal(originalCalls.length, 1);
    assert.equal(providerRequestSettled, false);

    let firstModelListSettled = false;
    const firstModelListRequest = client.sendRequest("model/list", {}).then((value) => {
      firstModelListSettled = true;
      return value;
    });
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(firstModelListSettled, false);
    assert.equal(catalogLoadCalls, 1);
    assert.equal(patchedModelListCalls, 0);
    resolveCatalogLoad();
    const firstModelListResult = await firstModelListRequest;
    assert.deepEqual(firstModelListResult, {
      data: [],
      nextCursor: null,
      method: "model/list",
      requestMethod: "list-models-for-host",
      models: ["gpt-5.6-sol"],
    });
    assert.equal(catalogLoadCalls, 1);
    assert.equal(patchedModelListCalls, 1);

    const legacyModelListResult = await client.sendRequest(
      "send-cli-request-for-host",
      { method: "list-models-for-host" },
    );
    assert.deepEqual(legacyModelListResult, {
      data: [],
      nextCursor: null,
      method: "send-cli-request-for-host",
      requestMethod: "list-models-for-host",
      models: ["gpt-5.6-sol"],
    });
    const vscodeModelListResult = await client.sendRequest(
      "vscode://codex/model/list?includeHidden=true",
      {},
    );
    assert.equal(vscodeModelListResult.requestMethod, "list-models-for-host");
    assert.deepEqual(vscodeModelListResult.models, ["gpt-5.6-sol"]);
    assert.equal(catalogLoadCalls, 1);
    assert.equal(patchedModelListCalls, 3);
  });

  it("waits for the catalog bridge instead of turning a slow startup into an empty catalog", async () => {
    const renderer = await readFile(
      new URL("../../../assets/inject/renderer-inject.js", import.meta.url),
      "utf8",
    );
    const postJson = sourceSection(
      renderer,
      "async function postJson",
      "function downloadMarkdownFallback",
    );
    assert.doesNotMatch(postJson, /model_catalog_bridge_timeout/);
    assert.doesNotMatch(postJson, /path === "\/codex-model-catalog"/);
    assert.match(postJson, /return await window\.__codexSessionDeleteBridge\(path, payload\);/);
    const diagnostics = [];
    let resolveCatalog;
    let timeoutCalls = 0;
    const delayedCatalog = new Promise((resolve) => {
      resolveCatalog = resolve;
    });
    const postJsonRequest = Function(
      "window",
      "helperBase",
      "sendCodexPlusDiagnostic",
      "fetch",
      "setTimeout",
      `"use strict"; return (${postJson.trim()});`,
    )(
      { __codexSessionDeleteBridge: () => delayedCatalog },
      "http://127.0.0.1:0",
      (event, detail) => diagnostics.push({ event, detail }),
      () => Promise.reject(new Error("unexpected fetch")),
      () => {
        timeoutCalls += 1;
        return 1;
      },
    );
    let settled = false;
    const catalogRequest = postJsonRequest("/codex-model-catalog", {}).then((value) => {
      settled = true;
      return value;
    });
    await Promise.resolve();
    assert.equal(settled, false);
    assert.equal(timeoutCalls, 0);
    resolveCatalog({ status: "ok", models: ["gpt-5.6-sol"] });
    assert.deepEqual(await catalogRequest, { status: "ok", models: ["gpt-5.6-sol"] });
    assert.equal(diagnostics.length, 0);

    const modelJsonPatch = sourceSection(
      renderer,
      "function codexModelDescriptorLooksPatchable",
      "function patchStatsigModelDynamicConfig",
    );
    assert.match(modelJsonPatch, /descriptorArrays\.some\(\(value\) => codexModelDescriptorArrayLooksPatchable\(value\)\)/);
    assert.match(modelJsonPatch, /const hasModelContainerSignal = "defaultModel" in value/);
    assert.match(modelJsonPatch, /if \(!modelJsonResponseLooksPatchable\(payload\)\) return payload;/);
    assert.match(modelJsonPatch, /if \(!codexPlusModelNames\(\)\.length\) await loadCodexModelCatalog\(\);/);
    assert.ok(
      modelJsonPatch.indexOf("if (!modelJsonResponseLooksPatchable(payload)) return payload;")
        < modelJsonPatch.indexOf("if (!codexPlusModelNames().length)"),
      "non-model JSON must bypass the model catalog before any bridge wait",
    );

    const modelArrayLooksPatchableSource = sourceSection(
      renderer,
      "function modelArrayLooksPatchable",
      "function stringArrayLooksPatchable",
    );
    const modelArrayLooksPatchable = Function(
      `"use strict"; return (${modelArrayLooksPatchableSource.trim()});`,
    )();
    const modelJsonClassifierSource = sourceSection(
      renderer,
      "function codexModelDescriptorLooksPatchable",
      "async function patchModelJsonResponse",
    );
    const modelJsonResponseLooksPatchable = Function(
      "modelArrayLooksPatchable",
      "stringArrayLooksPatchable",
      `"use strict"; ${modelJsonClassifierSource}\nreturn modelJsonResponseLooksPatchable;`,
    )(
      modelArrayLooksPatchable,
      (value) => Array.isArray(value) && value.every((item) => typeof item === "string"),
    );

    const officialModel = {
      id: "gpt-5.6-sol",
      model: "gpt-5.6-sol",
      displayName: "GPT-5.6 Sol",
      defaultReasoningEffort: "medium",
      supportedReasoningEfforts: [{ reasoningEffort: "medium", description: "Medium" }],
      isDefault: true,
      hidden: false,
    };
    assert.equal(modelJsonResponseLooksPatchable({ data: [officialModel], nextCursor: null }), true);
    assert.equal(modelJsonResponseLooksPatchable({
      result: {
        models: [{
          model: "legacy-model",
          display_name: "Legacy model",
          default_reasoning_effort: "high",
          supported_reasoning_efforts: [{ reasoning_effort: "high" }],
          is_default: false,
        }],
      },
    }), true);
    assert.equal(modelJsonResponseLooksPatchable({ models: [], defaultModel: null }), true);
    assert.equal(modelJsonResponseLooksPatchable({ data: [{ model: "ThinkPad" }], nextCursor: null }), false);
    assert.equal(modelJsonResponseLooksPatchable({ result: [{ model: "worker-a" }] }), false);
    assert.equal(modelJsonResponseLooksPatchable({ pages: [{ data: [{ model: "invoice" }], nextCursor: null }] }), false);
    assert.equal(modelJsonResponseLooksPatchable({
      message: { result: { data: [officialModel] } },
    }), false);

    const patchModelJsonSource = sourceSection(
      renderer,
      "async function patchModelJsonResponse",
      "function modelJsonResponseBypassesPatch",
    );
    let catalogLoads = 0;
    let catalogReady = false;
    let resolveCatalogLoad;
    const catalogLoadGate = new Promise((resolve) => {
      resolveCatalogLoad = () => {
        catalogReady = true;
        resolve();
      };
    });
    let modelContainerPatches = 0;
    const patchModelJsonResponse = Function(
      "codexPlusModelUnlockEnabled",
      "codexPlusModelNames",
      "loadCodexModelCatalog",
      "modelJsonResponseLooksPatchable",
      "patchModelContainer",
      "window",
      `"use strict"; return (${patchModelJsonSource.trim()});`,
    )(
      () => true,
      () => (catalogReady ? ["gpt-5.6-sol"] : []),
      async () => {
        catalogLoads += 1;
        await catalogLoadGate;
      },
      modelJsonResponseLooksPatchable,
      (payload) => {
        modelContainerPatches += 1;
        payload.models = ["gpt-5.6-sol"];
      },
      {},
    );
    const unrelatedPayload = {
      requirements: {
        allowedWindowsSandboxImplementations: ["elevated", "unelevated"],
      },
    };
    assert.equal(await patchModelJsonResponse(unrelatedPayload), unrelatedPayload);
    assert.equal(catalogLoads, 0);
    assert.equal(modelContainerPatches, 0);

    let blockedCatalogLoads = 0;
    const patchModelJsonWithBlockedCatalog = Function(
      "codexPlusModelUnlockEnabled",
      "codexPlusModelNames",
      "loadCodexModelCatalog",
      "modelJsonResponseLooksPatchable",
      "patchModelContainer",
      "window",
      `"use strict"; return (${patchModelJsonSource.trim()});`,
    )(
      () => true,
      () => [],
      async () => {
        blockedCatalogLoads += 1;
        await new Promise(() => {});
      },
      (payload) => Array.isArray(payload?.models) && Object.prototype.hasOwnProperty.call(payload, "defaultModel"),
      () => {
        throw new Error("Windows requirements response must not be patched as a model response");
      },
      {},
    );
    let timeoutId;
    const bootstrapBypass = await Promise.race([
      patchModelJsonWithBlockedCatalog(unrelatedPayload).then((value) => ({ status: "resolved", value })),
      new Promise((resolve) => {
        timeoutId = setTimeout(() => resolve({ status: "timed-out" }), 100);
      }),
    ]);
    clearTimeout(timeoutId);
    assert.equal(bootstrapBypass.status, "resolved");
    assert.equal(bootstrapBypass.value, unrelatedPayload);
    assert.equal(blockedCatalogLoads, 0);

    const firstModelPayload = { models: [], defaultModel: null };
    let firstModelResponseSettled = false;
    const firstModelResponse = patchModelJsonResponse(firstModelPayload).then((value) => {
      firstModelResponseSettled = true;
      return value;
    });
    await Promise.resolve();
    assert.equal(firstModelResponseSettled, false);
    assert.deepEqual(firstModelPayload.models, []);
    assert.equal(catalogLoads, 1);
    assert.equal(modelContainerPatches, 0);
    resolveCatalogLoad();
    assert.equal(await firstModelResponse, firstModelPayload);
    assert.deepEqual(firstModelPayload.models, ["gpt-5.6-sol"]);
    assert.equal(catalogLoads, 1);
    assert.equal(modelContainerPatches, 1);

    const modelJsonBypassSource = sourceSection(
      renderer,
      "function modelJsonResponseBypassesPatch",
      "function installModelJsonResponsePatch",
    );
    const modelJsonResponseBypassesPatch = Function(
      "window",
      "helperBase",
      `"use strict"; return (${modelJsonBypassSource.trim()});`,
    )(
      { location: { href: "app://-/index.html" } },
      "http://127.0.0.1:57321",
    );
    assert.equal(modelJsonResponseBypassesPatch({ url: "http://127.0.0.1:57321/backend/status" }), true);
    assert.equal(modelJsonResponseBypassesPatch({ url: "https://example.test/codex-model-catalog" }), true);
    assert.equal(modelJsonResponseBypassesPatch({ url: "https://api.openai.com/v1/models" }), false);

    const installModelJsonResponsePatchSource = sourceSection(
      renderer,
      "function installModelJsonResponsePatch",
      "function patchStatsigModelDynamicConfig",
    );
    class FakeResponse {
      constructor(payload, url = "https://api.openai.com/v1/models") {
        this.payload = payload;
        this.url = url;
      }

      async json() {
        return this.payload;
      }
    }
    const nativeResponseJson = FakeResponse.prototype.json;
    let staleWrapperCalls = 0;
    let currentWrapperCalls = 0;
    FakeResponse.prototype.json = async function staleResponseJson() {
      staleWrapperCalls += 1;
      await new Promise(() => {});
    };
    const upgradeWindow = {
      __codexPlusModelJsonResponsePatchInstalled: "4",
      __codexPlusModelJsonResponseOriginals: { responseJson: nativeResponseJson },
    };
    const installModelJsonResponsePatch = Function(
      "window",
      "Response",
      "codexModelJsonResponsePatchVersion",
      "patchModelJsonResponse",
      "modelJsonResponseBypassesPatch",
      `"use strict"; return (${installModelJsonResponsePatchSource.trim()});`,
    )(
      upgradeWindow,
      FakeResponse,
      "5",
      async (payload) => {
        currentWrapperCalls += 1;
        return payload;
      },
      modelJsonResponseBypassesPatch,
    );
    installModelJsonResponsePatch();
    let upgradeTimeoutId;
    const upgradedResponse = await Promise.race([
      new FakeResponse(unrelatedPayload).json().then((value) => ({ status: "resolved", value })),
      new Promise((resolve) => {
        upgradeTimeoutId = setTimeout(() => resolve({ status: "timed-out" }), 100);
      }),
    ]);
    clearTimeout(upgradeTimeoutId);
    assert.equal(upgradeWindow.__codexPlusModelJsonResponsePatchInstalled, "5");
    assert.equal(upgradedResponse.status, "resolved");
    assert.equal(upgradedResponse.value, unrelatedPayload);
    assert.equal(staleWrapperCalls, 0);
    assert.equal(currentWrapperCalls, 1);
    const localCatalogPayload = { status: "ok", models: [], default_model: "" };
    assert.equal(
      await new FakeResponse(localCatalogPayload, "http://127.0.0.1:57321/codex-model-catalog").json(),
      localCatalogPayload,
    );
    assert.equal(currentWrapperCalls, 1);
  });

  it("delays one MCP model/list dispatch until the catalog can patch its first response", async () => {
    const renderer = await readFile(
      new URL("../../../assets/inject/renderer-inject.js", import.meta.url),
      "utf8",
    );
    const messagePatchSource = sourceSection(
      renderer,
      "function patchAppServerModelMessages",
      "function appServerModelRequestMethod",
    );
    assert.match(messagePatchSource, /window\.dispatchEvent\s*=\s*function codexPlusModelPatchedDispatchEvent/);
    const trackedRequestIds = new Set();
    let catalogLoadCalls = 0;
    let activeModelNames = [];
    let resolveCatalogLoad;
    const catalogLoad = new Promise((resolve) => {
      resolveCatalogLoad = () => {
        activeModelNames = ["gpt-5.6-sol"];
        resolve();
      };
    });
    const listeners = new Map();
    const deliveredEvents = [];
    const windowStub = {
      dispatchEvent(event) {
        deliveredEvents.push(event);
        return false;
      },
      addEventListener(type, listener) {
        listeners.set(type, listener);
      },
      setTimeout() {
        return 1;
      },
    };
    const messagePatch = Function(
      "codexModelMessagePatchVersion",
      "codexPlusModelListRequestIds",
      "codexPlusModelUnlockEnabled",
      "codexPlusModelNames",
      "loadCodexModelCatalog",
      "patchModelArray",
      "window",
      `"use strict"; ${messagePatchSource}\nreturn { install: patchAppServerModelMessages, patchResponse: patchMcpModelResponseData };`,
    )(
      "3",
      trackedRequestIds,
      () => true,
      () => activeModelNames,
      async () => {
        catalogLoadCalls += 1;
        await catalogLoad;
      },
      (models, allowEmpty) => {
        if (!Array.isArray(models) || (!allowEmpty && models.length === 0)) return false;
        activeModelNames.forEach((model) => models.push({ model }));
        return true;
      },
      windowStub,
    );

    messagePatch.install();
    assert.equal(windowStub.__codexPlusModelMessagePatchInstalled, "3");
    assert.equal(typeof listeners.get("message"), "function");

    const untracked = { type: "mcp-response", message: { id: "other", result: { data: [] } } };
    listeners.get("message")({ data: untracked });
    assert.deepEqual(untracked.message.result.data, []);

    const requestEvent = {
      type: "codex-message-from-view",
      detail: { type: "mcp-request", request: { id: "model-1", method: "model/list", params: {} } },
    };
    assert.equal(windowStub.dispatchEvent(requestEvent), true);
    assert.equal(requestEvent.detail.request.params.includeHidden, true);
    assert.equal(trackedRequestIds.has("model-1"), true);
    assert.equal(catalogLoadCalls, 1);
    assert.equal(deliveredEvents.length, 0);

    resolveCatalogLoad();
    await catalogLoad;
    await Promise.resolve();
    assert.equal(deliveredEvents.length, 1);
    assert.equal(deliveredEvents[0], requestEvent);

    const tracked = { type: "mcp-response", message: { id: "model-1", result: { data: [] } } };
    listeners.get("message")({ data: tracked });
    assert.deepEqual(tracked.message.result.data, [{ model: "gpt-5.6-sol" }]);
    assert.equal(trackedRequestIds.has("model-1"), false);
    assert.equal(catalogLoadCalls, 1);
  });

  it("retries a startup catalog failure after the bridge and settings become ready", async () => {
    const renderer = await readFile(
      new URL("../../../assets/inject/renderer-inject.js", import.meta.url),
      "utf8",
    );
    const catalogLoaderSource = sourceSection(
      renderer,
      "async function loadCodexModelCatalog",
      "function codexPlusModelMetadata",
    );
    let catalogRequests = 0;
    const responses = [
      { status: "failed", message: "bridge unavailable" },
      { status: "ok", default_model: "gpt-5.6-sol", models: ["gpt-5.6-sol"] },
    ];
    const catalogHarness = Function(
      "postJson",
      `"use strict";
       let codexModelCatalogPromise = null;
       let codexModelCatalogLoadedAt = 0;
       let codexModelCatalog = {};
       const renderCodexPlusMenu = () => {};
       const scheduleCodexModelWhitelistRefresh = () => {};
       const sendCodexPlusDiagnostic = () => {};
       ${catalogLoaderSource}
       return {
         load: loadCodexModelCatalog,
         state: () => ({ catalog: codexModelCatalog, loadedAt: codexModelCatalogLoadedAt }),
       };`,
    )(async () => {
      const response = responses[catalogRequests];
      catalogRequests += 1;
      return response;
    });

    assert.equal((await catalogHarness.load()).status, "failed");
    assert.equal(catalogHarness.state().loadedAt, 0);
    assert.equal((await catalogHarness.load()).status, "ok");
    assert.equal(catalogRequests, 2);
    assert.deepEqual(catalogHarness.state().catalog.models, ["gpt-5.6-sol"]);
    assert.ok(catalogHarness.state().loadedAt > 0);

    const backendLoadSource = sourceSection(
      renderer,
      "async function loadBackendSettings()",
      "function loadBackendSettingsForStartup",
    );
    assert.match(backendLoadSource, /loadCodexModelCatalog\(true\)/);

    const installSource = sourceSection(
      renderer,
      "function ensureCodexModelWhitelistInstalls",
      "function runCodexModelWhitelistRefreshPass",
    );
    assert.doesNotMatch(installSource, /if \(!codexPlusBackendSettingsLoaded\) return;/);

    const messagePatchSource = sourceSection(
      renderer,
      "function patchAppServerModelMessages",
      "function appServerModelRequestMethod",
    );
    assert.match(messagePatchSource, /loadCodexModelCatalog\(true\)/);
  });

  it("uses the active relay profile as a synchronous model source", async () => {
    const renderer = await readFile(
      new URL("../../../assets/inject/renderer-inject.js", import.meta.url),
      "utf8",
    );
    const modelNamesSource = sourceSection(
      renderer,
      "function codexPlusModelNames",
      "async function loadCodexModelCatalog",
    );
    let activeProfile = {
      model: "gpt-5.6-sol",
      modelList: "gpt-5.6-sol, gpt-5.6-terra\ngpt-5.6-luna",
    };
    const modelNames = Function(
      "codexRemoteSessionActiveProfile",
      "codexModelCatalog",
      "uniqueValues",
      `"use strict"; return (${modelNamesSource.trim()});`,
    )(
      () => activeProfile,
      { default_model: "", model: "", models: [] },
      (values) => [...new Set(values.filter(
        (value) => typeof value === "string" && value.trim().length > 0,
      ))],
    );

    assert.deepEqual(modelNames(), ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"]);
    activeProfile = { modelList: ["gpt-5.6-sol", "gpt-5.6-terra"] };
    assert.deepEqual(modelNames(), ["gpt-5.6-sol", "gpt-5.6-terra"]);
  });

  it("limits model whitelist patches to the upstream Statsig config", async () => {
    const renderer = await readFile(
      new URL("../../../assets/inject/renderer-inject.js", import.meta.url),
      "utf8",
    );
    const whitelistPatch = sourceSection(
      renderer,
      "function patchStatsigModelWhitelist",
      "function patchAppServerModelMessages",
    );

    assert.match(whitelistPatch, /String\(name\) === "107580212"/);
    assert.doesNotMatch(whitelistPatch, /function patchReactModelState/);
    assert.doesNotMatch(whitelistPatch, /function patchObjectGraphForModels/);
    assert.doesNotMatch(whitelistPatch, /__reactFiber|__reactInternalInstance|__reactProps/);
  });

  it("bounds only the provider refresh and lets startup settings finish", async () => {
    const renderer = await readFile(
      new URL("../../../assets/inject/renderer-inject.js", import.meta.url),
      "utf8",
    );
    const startupLoader = sourceSection(
      renderer,
      "function loadBackendSettingsState",
      "async function refreshBackendSettingsForProviderRequest",
    );
    const providerRefresh = sourceSection(
      renderer,
      "async function refreshBackendSettingsForProviderRequest",
      "async function loadBackendSettings",
    );
    const requestPatch = sourceSection(
      renderer,
      "function patchAppServerModelRequestClient",
      "const appServerModelRequestPatchMaxMisses",
    );

    assert.doesNotMatch(startupLoader, /Promise\.race|settings bridge timeout/);
    assert.match(providerRefresh, /Promise\.race/);
    assert.match(providerRefresh, /settings bridge timeout/);
    assert.match(providerRefresh, /3000/);
    assert.match(requestPatch, /await refreshBackendSettingsForProviderRequest\(\)/);
    assert.doesNotMatch(requestPatch, /await loadBackendSettingsState\(\)/);
  });

  it("never scans React RPC targets for instance sendRequest properties", async () => {
    const renderer = await readFile(
      new URL("../../../assets/inject/renderer-inject.js", import.meta.url),
      "utf8",
    );
    const candidateLoaderSource = sourceSection(
      renderer,
      "async function loadAppServerRequestCandidates",
      "function codexSettingStorageFromModule",
    );
    assert.doesNotMatch(renderer, /collectAppServerRequestCandidatesFromReactRoot/);
    assert.doesNotMatch(renderer, /reactRequestCandidates/);
    const installerSource = sourceSection(
      renderer,
      "const appServerModelRequestPatchMaxMisses",
      "function ensureCodexModelWhitelistInstalls",
    );
    assert.match(installerSource, /appServerModelRequestPatchPromise/);
    assert.match(installerSource, /scheduleAppServerModelRequestPatchRetry/);
    assert.doesNotMatch(installerSource, /installAppServerModelDispatcherPatch/);

    const diagnostics = [];
    const scheduledCallbacks = [];
    const windowStub = {
      setTimeout(callback) {
        scheduledCallbacks.push(callback);
        return scheduledCallbacks.length;
      },
    };
    const installer = Function(
      "codexRemoteSessionProviderPatchEnabled",
      "window",
      "loadAppServerRequestCandidates",
      "patchAppServerModelRequestClient",
      "sendCodexPlusDiagnostic",
      "clearTimeout",
      "codexAppServerModelRequestPatchVersion",
      `"use strict"; ${installerSource}\nreturn { install: installAppServerModelRequestPatch, pending: () => appServerModelRequestPatchPromise };`,
    )(
      () => true,
      windowStub,
      async () => ({ modules: [], candidates: [], sources: [], discovery: "named-assets" }),
      () => {
        throw new Error("a non-module RpcTarget must never be inspected");
      },
      (event, detail) => diagnostics.push({ event, detail }),
      () => {},
      "15",
    );

    installer.install();
    installer.install();
    await installer.pending();
    assert.equal(windowStub.__codexPlusAppServerModelRequestPatchInstalled, undefined);
    assert.equal(diagnostics.at(-1)?.event, "model_app_server_request_patch_not_found");
    assert.equal(scheduledCallbacks.length, 1);
    assert.doesNotMatch(candidateLoaderSource, /__codexRoot|sendRequest/);
  });

  it("automatically renames a session through the native title suggestion", async () => {
    const renderer = await readFile(
      new URL("../../../assets/inject/renderer-inject.js", import.meta.url),
      "utf8",
    );

    assert.match(renderer, /自动重命名当前会话/);
    assert.match(renderer, /activateSessionAutoRenameMenuItem/);
    assert.match(renderer, /input\[aria-label="聊天标题"\], input\[aria-label="Chat title"\]/);
    assert.match(renderer, /button\.classList\.contains\("text-info"\)/);
    assert.match(renderer, /\^\(保存\|Save\)\$/);
    assert.match(renderer, /Codex 未能生成新名称/);
  });
});
