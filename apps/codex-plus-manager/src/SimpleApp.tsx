import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import {
  Activity,
  AlertTriangle,
  Blocks,
  Cable,
  Check,
  ChevronDown,
  CircleX,
  Download,
  Eye,
  EyeOff,
  ExternalLink,
  FolderOpen,
  Globe2,
  Image,
  KeyRound,
  LoaderCircle,
  RefreshCw,
  Rocket,
  RotateCcw,
  Search,
  ShieldCheck,
  Smartphone,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";

import { MobileControlPanel } from "./MobileControlPanel";
import {
  getApplyBlockReason,
  isCodexLaunchReady,
} from "./simple-app-guards";
import "./simple-app.css";

type AccessMode = "mixedApi" | "pureApi";

type AccessStatus = {
  phase: "unmanaged" | "active" | "active_degraded" | "restore_failed" | "state_unreadable";
  active: boolean;
  mode: AccessMode | null;
  modelCount: number;
  defaultModel: string;
  currentProvider: string;
  originalProvider: string | null;
  baselineExists: boolean;
  baselineCreatedAtMs: number | null;
  sessionSyncStatus: string;
  mcpServerCount: number;
  pluginMarketplaceStatus: string;
  lastMessage: string;
  lastOperationAtMs: number | null;
};

type MirrorModel = {
  id: string;
  displayName: string;
  contextWindow: number | null;
  contextSource: string;
};

type Discovery = {
  models: MirrorModel[];
  defaultModel: string;
};

type PreflightCheck = {
  id: string;
  ready: boolean;
  label: string;
  detail: string;
};

type Preflight = {
  ready: boolean;
  codexInstalled: boolean;
  codexAppPath: string | null;
  codexVersion: string | null;
  codexRunning?: boolean;
  codexHome: string;
  checks: PreflightCheck[];
};

type ImagegenStatus = {
  enabled: boolean;
  configured: boolean;
  managed: boolean;
  helperAvailable: boolean;
  skillAvailable: boolean;
  skillPath: string;
  configPath: string;
  sourceCommit: string;
};

type MixedAuthStatus = {
  ready: boolean;
  method: string;
  message: string;
};

type SessionIndexCleanupPreview = {
  snapshotSha256: string;
  candidates: Array<{ id: string; threadName: string; updatedAt: string }>;
};

type CommandResult<T extends object> = {
  status: string;
  message: string;
} & T;

type OperationFeedback = {
  title: string;
  detail: string;
};

const operationFeedback = (operation: string | null): OperationFeedback | null => {
  if (!operation) return null;
  if (operation === "loading") {
    return { title: "正在检查本机环境", detail: "只读取 Codex 安装、配置和接入状态，不会修改文件。" };
  }
  if (operation === "validate-codexpro" || operation === "validate-enterprise") {
    return { title: "正在验证 Key 与模型", detail: "每个 Key 只发送一次真实流式请求；验证结果会短时保留供应用接入复用。" };
  }
  if (operation === "validate-image") {
    return { title: "正在确认生图权限", detail: "只检查 gpt-image-2 模型权限，不会发起真实生图。" };
  }
  if (operation === "enable") {
    return {
      title: "正在安全应用接入",
      detail: "正在复用已验证结果、建立还原点、写入并回读配置。此阶段不会重复发送付费请求。",
    };
  }
  if (operation === "restore") {
    return {
      title: "正在恢复使用前状态",
      detail: "正在按还原点恢复配置和会话归属。请不要启动 Codex 或关闭此工具。",
    };
  }
  if (operation === "recover-baseline") {
    return {
      title: "正在从还原点灾难恢复",
      detail: "正在校验首次接入还原点并恢复受管配置；损坏文件和操作前快照都会保留。",
    };
  }
  if (operation === "repair") {
    return { title: "正在修复会话归属", detail: "配置保持不变，只重试未完成的会话同步或恢复步骤。" };
  }
  if (operation === "cleanup-sessions") {
    return { title: "正在整理会话列表", detail: "正在核对索引、备份原文件并原子更新。" };
  }
  if (operation === "install-codex") {
    return { title: "正在安装 Codex Desktop", detail: "下载和安装时间取决于网络；完成后会重新检查安装位置。" };
  }
  if (operation === "choose-codex") {
    return { title: "正在确认 Codex 路径", detail: "只保存所选应用路径，不会启动或修改 Codex。" };
  }
  if (operation === "enable-sandbox") {
    return {
      title: "正在启用完整文件能力",
      detail: "先备份关键状态，再调用真实 Codex CLI 完成官方 Windows 初始化与回读验证。",
    };
  }
  if (operation === "update-codex") {
    return {
      title: "正在更新 Codex",
      detail: "仅调用官方 winget / Microsoft Store 更新入口，不会启动 Codex 或修改会话。",
    };
  }
  if (operation === "launch") {
    return { title: "正在打开 Codex", detail: "正在使用已确认的应用路径启动。" };
  }
  return { title: "正在处理", detail: "完成前请保持此窗口打开。" };
};

const emptyStatus: AccessStatus = {
  phase: "unmanaged",
  active: false,
  mode: null,
  modelCount: 0,
  defaultModel: "",
  currentProvider: "openai",
  originalProvider: null,
  baselineExists: false,
  baselineCreatedAtMs: null,
  sessionSyncStatus: "not_run",
  mcpServerCount: 0,
  pluginMarketplaceStatus: "missing",
  lastMessage: "",
  lastOperationAtMs: null,
};

const emptyImagegenStatus: ImagegenStatus = {
  enabled: false,
  configured: false,
  managed: false,
  helperAvailable: false,
  skillAvailable: false,
  skillPath: "",
  configPath: "",
  sourceCommit: "",
};

const previewPreflight: Preflight = {
  ready: true,
  codexInstalled: true,
  codexAppPath: "preview/Codex.exe",
  codexVersion: "preview",
  codexHome: "~/.codex",
  checks: [
    { id: "codex", ready: true, label: "Codex Desktop", detail: "已检测到 Codex 应用" },
    { id: "home", ready: true, label: "配置目录", detail: "可安全写入" },
    { id: "config", ready: true, label: "原始配置", detail: "格式正常，可以备份和回滚" },
  ],
};

const previewMixedAuth: MixedAuthStatus = {
  ready: true,
  method: "chatgpt",
  message: "已通过真实 Codex CLI 确认 ChatGPT 登录，可使用混合 API。",
};

const previewCodexProDiscovery: Discovery = {
  defaultModel: "gpt-5.4",
  models: [
    { id: "gpt-5.4", displayName: "GPT-5.4", contextWindow: 400000, contextSource: "service" },
    {
      id: "gpt-5.3-codex",
      displayName: "GPT-5.3 Codex",
      contextWindow: 272000,
      contextSource: "service",
    },
    {
      id: "grok-4.1",
      displayName: "Grok 4.1",
      contextWindow: 256000,
      contextSource: "service",
    },
  ],
};

const previewEnterpriseDiscovery: Discovery = {
  defaultModel: "gpt-5.5",
  models: [
    {
      id: "gpt-5.5",
      displayName: "GPT-5.5",
      contextWindow: 400000,
      contextSource: "service",
    },
    {
      id: "gpt-5.4",
      displayName: "GPT-5.4",
      contextWindow: 400000,
      contextSource: "service",
    },
  ],
};

function isFailure(status: string) {
  return ["failed", "error"].includes(status);
}

export function SimpleApp() {
  const [access, setAccess] = useState<AccessStatus>(emptyStatus);
  const [preflight, setPreflight] = useState<Preflight | null>(null);
  const [apiKey, setApiKey] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [enterpriseApiKey, setEnterpriseApiKey] = useState("");
  const [showEnterpriseKey, setShowEnterpriseKey] = useState(false);
  const [imageApiKey, setImageApiKey] = useState("");
  const [showImageKey, setShowImageKey] = useState(false);
  const [imagegen, setImagegen] = useState<ImagegenStatus>(emptyImagegenStatus);
  const [mixedAuth, setMixedAuth] = useState<MixedAuthStatus | null>(null);
  const [imagegenEnabled, setImagegenEnabled] = useState(false);
  const [imageKeyValidated, setImageKeyValidated] = useState(false);
  const [mode, setMode] = useState<AccessMode>("mixedApi");
  const [discovery, setDiscovery] = useState<Discovery | null>(null);
  const [selectedModelIds, setSelectedModelIds] = useState<string[]>([]);
  const [enterpriseDiscovery, setEnterpriseDiscovery] = useState<Discovery | null>(null);
  const [enterpriseSelectedModelIds, setEnterpriseSelectedModelIds] = useState<string[]>([]);
  const [defaultModel, setDefaultModel] = useState("");
  const [modelFilters, setModelFilters] = useState({ codexpro: "", enterprise: "" });
  const [preflightOpen, setPreflightOpen] = useState(false);
  const [connectionOpen, setConnectionOpen] = useState(true);
  const [busy, setBusy] = useState<string | null>("loading");
  const [restoreConfirmOpen, setRestoreConfirmOpen] = useState(false);
  const [baselineRecoveryConfirmOpen, setBaselineRecoveryConfirmOpen] = useState(false);
  const [notice, setNotice] = useState<{ kind: "ok" | "error" | "info"; text: string } | null>(null);
  const validationEpoch = useRef({ codexpro: 0, enterprise: 0, image: 0 });
  const operationToken = useRef<symbol | null>(null);
  const noticeRef = useRef<HTMLDivElement>(null);

  const beginOperation = useCallback((operation: string) => {
    if (operationToken.current) return null;
    const token = Symbol(operation);
    operationToken.current = token;
    setBusy(operation);
    return token;
  }, []);

  const endOperation = useCallback((token: symbol) => {
    if (operationToken.current !== token) return;
    operationToken.current = null;
    setBusy(null);
  }, []);

  const refresh = useCallback(async (ownerToken?: symbol) => {
    const usingOwner = ownerToken !== undefined && operationToken.current === ownerToken;
    const token = usingOwner
      ? ownerToken
      : beginOperation("loading");
    if (!token) return;
    const ownsOperation = !usingOwner;
    if (!("__TAURI_INTERNALS__" in window)) {
      setAccess(emptyStatus);
      setPreflight(previewPreflight);
      setMixedAuth(previewMixedAuth);
      setNotice((current) => current?.kind === "error" ? null : current);
      if (ownsOperation) endOperation(token);
      return;
    }
    try {
      const [statusResult, preflightResult, imagegenResult, mixedAuthResult] = await Promise.allSettled([
        invoke<CommandResult<{ access: AccessStatus }>>("get_mirror_access_status"),
        invoke<CommandResult<{ preflight: Preflight }>>("get_mirror_preflight"),
        invoke<CommandResult<{ imagegen: ImagegenStatus }>>("get_mirror_imagegen_status"),
        invoke<CommandResult<{ mixedAuth: MixedAuthStatus }>>("get_mirror_mixed_auth_status"),
      ]);
      const errors: string[] = [];
      if (statusResult.status === "fulfilled") {
        setAccess(statusResult.value.access);
        if (statusResult.value.access.mode) setMode(statusResult.value.access.mode);
        if (isFailure(statusResult.value.status)) errors.push(statusResult.value.message);
      } else {
        errors.push(`接入状态：${String(statusResult.reason)}`);
      }
      if (preflightResult.status === "fulfilled") {
        setPreflight(preflightResult.value.preflight);
      } else {
        errors.push(`环境检查：${String(preflightResult.reason)}`);
      }
      if (imagegenResult.status === "fulfilled") {
        setImagegen(imagegenResult.value.imagegen);
        setImagegenEnabled(imagegenResult.value.imagegen.enabled);
      } else {
        errors.push(`生图状态：${String(imagegenResult.reason)}`);
      }
      if (mixedAuthResult.status === "fulfilled") {
        setMixedAuth(mixedAuthResult.value.mixedAuth);
        if (isFailure(mixedAuthResult.value.status)) errors.push(mixedAuthResult.value.message);
      } else {
        setMixedAuth(null);
        errors.push(`ChatGPT 登录：${String(mixedAuthResult.reason)}`);
      }
      if (errors.length) {
        setNotice({ kind: "error", text: `部分状态加载失败。${errors.join("；")}` });
      } else {
        setNotice((current) => current?.kind === "error" ? null : current);
      }
    } catch (error) {
      setNotice({ kind: "error", text: String(error) });
    } finally {
      if (ownsOperation) endOperation(token);
    }
  }, [beginOperation, endOperation]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (preflight && !preflight.ready) setPreflightOpen(true);
  }, [preflight]);

  useEffect(() => {
    setConnectionOpen(!access.active);
  }, [access.active]);

  useEffect(() => {
    if (notice?.kind !== "error") return;
    const frame = window.requestAnimationFrame(() => {
      noticeRef.current?.scrollIntoView({ behavior: "smooth", block: "center" });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [notice]);

  useEffect(() => {
    if (!restoreConfirmOpen && !baselineRecoveryConfirmOpen) return undefined;
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape" && busy === null) {
        setRestoreConfirmOpen(false);
        setBaselineRecoveryConfirmOpen(false);
      }
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => {
      document.body.style.overflow = previousOverflow;
      window.removeEventListener("keydown", closeOnEscape);
    };
  }, [baselineRecoveryConfirmOpen, busy, restoreConfirmOpen]);

  const clearGroupValidation = (groupId: "codexpro" | "enterprise") => {
    if (groupId === "enterprise") {
      const invalidatedModelIds = enterpriseSelectedModelIds;
      setEnterpriseDiscovery(null);
      setEnterpriseSelectedModelIds([]);
      setDefaultModel((current) => invalidatedModelIds.includes(current) ? selectedModelIds[0] ?? "" : current);
    } else {
      const invalidatedModelIds = selectedModelIds;
      setDiscovery(null);
      setSelectedModelIds([]);
      setDefaultModel((current) => invalidatedModelIds.includes(current) ? enterpriseSelectedModelIds[0] ?? "" : current);
    }
  };

  const validate = async (groupId: "codexpro" | "enterprise") => {
    const isEnterprise = groupId === "enterprise";
    const key = isEnterprise ? enterpriseApiKey : apiKey;
    const label = isEnterprise ? "企业GPT专线（极稳）" : "CodexPro";
    const token = beginOperation(`validate-${groupId}`);
    if (!token) return;
    const epoch = ++validationEpoch.current[groupId];
    clearGroupValidation(groupId);
    setNotice(null);
    if (!key.trim()) {
      setNotice({ kind: "error", text: `请输入 ${label} Key。` });
      endOperation(token);
      return;
    }
    if (!("__TAURI_INTERNALS__" in window)) {
      const preview = isEnterprise ? previewEnterpriseDiscovery : previewCodexProDiscovery;
      if (isEnterprise) {
        setEnterpriseDiscovery(preview);
        setEnterpriseSelectedModelIds(preview.models.map((model) => model.id));
      } else {
        setDiscovery(preview);
        setSelectedModelIds(preview.models.map((model) => model.id));
      }
      setDefaultModel((current) => current || preview.defaultModel);
      setModelFilters((current) => ({ ...current, [groupId]: "" }));
      setNotice({ kind: "ok", text: `预览模式：已加载 ${label} 模型示例。` });
      endOperation(token);
      return;
    }
    try {
      const result = await invoke<CommandResult<{ discovery: Discovery | null }>>("validate_mirror_key", {
        apiKey: key.trim(),
      });
      if (validationEpoch.current[groupId] !== epoch) return;
      if (isFailure(result.status) || !result.discovery) {
        clearGroupValidation(groupId);
        setNotice({ kind: "error", text: result.message });
        return;
      }
      if (isEnterprise) {
        setEnterpriseDiscovery(result.discovery);
        setEnterpriseSelectedModelIds(result.discovery.models.map((model) => model.id));
      } else {
        setDiscovery(result.discovery);
        setSelectedModelIds(result.discovery.models.map((model) => model.id));
      }
      setDefaultModel((current) => current || result.discovery!.defaultModel);
      setModelFilters((current) => ({ ...current, [groupId]: "" }));
      setNotice({ kind: "ok", text: `${label} ${result.message}` });
    } catch (error) {
      if (validationEpoch.current[groupId] !== epoch) return;
      clearGroupValidation(groupId);
      setNotice({ kind: "error", text: String(error) });
    } finally {
      endOperation(token);
    }
  };

  const validateImageKey = async () => {
    setImageKeyValidated(false);
    if (!imageApiKey.trim()) {
      setNotice({ kind: "error", text: "请输入镜子AI Image Key。" });
      return;
    }
    const token = beginOperation("validate-image");
    if (!token) return;
    if (!("__TAURI_INTERNALS__" in window)) {
      setImageKeyValidated(true);
      setNotice({ kind: "ok", text: "预览模式：已模拟确认 gpt-image-2 模型权限，未发送真实生图请求。" });
      endOperation(token);
      return;
    }
    const epoch = ++validationEpoch.current.image;
    setNotice(null);
    try {
      const result = await invoke<CommandResult<{ valid: boolean; model: string | null }>>(
        "validate_mirror_image_key",
        { apiKey: imageApiKey.trim() },
      );
      if (validationEpoch.current.image !== epoch) return;
      if (isFailure(result.status) || !result.valid) {
        setNotice({ kind: "error", text: result.message });
        return;
      }
      setImageKeyValidated(true);
      setNotice({ kind: "ok", text: result.message });
    } catch (error) {
      if (validationEpoch.current.image !== epoch) return;
      setImageKeyValidated(false);
      setNotice({ kind: "error", text: String(error) });
    } finally {
      endOperation(token);
    }
  };

  const enable = async () => {
    if (applyBlockReason) {
      if (!preflight?.ready) setPreflightOpen(true);
      setNotice({ kind: "error", text: applyBlockReason });
      return;
    }
    const configuredGroups = [
      {
        id: "codexpro",
        label: "CodexPro（GPT / Grok）",
        apiKey: apiKey.trim(),
        discovery,
        selectedModelIds,
      },
      {
        id: "enterprise",
        label: "企业GPT专线（极稳）",
        apiKey: enterpriseApiKey.trim(),
        discovery: enterpriseDiscovery,
        selectedModelIds: enterpriseSelectedModelIds,
      },
    ].filter((group) => group.apiKey || group.discovery);
    if (configuredGroups.length === 0) {
      setNotice({ kind: "error", text: "请至少填写并验证一个分组 Key。" });
      return;
    }
    if (configuredGroups.some((group) => !group.apiKey || !group.discovery)) {
      setNotice({ kind: "error", text: "每个已填写的 Key 都必须先单独验证。" });
      return;
    }
    if (configuredGroups.some((group) => group.selectedModelIds.length === 0)) {
      setNotice({ kind: "error", text: "每个已连接分组至少勾选一个模型。" });
      return;
    }
    const allSelectedModelIds = configuredGroups.flatMap((group) => group.selectedModelIds);
    if (new Set(allSelectedModelIds).size !== allSelectedModelIds.length) {
      setNotice({ kind: "error", text: "同一个模型不能同时分配给两个 Key，请取消一处勾选。" });
      return;
    }
    if (!allSelectedModelIds.includes(defaultModel)) {
      setNotice({ kind: "error", text: "请从已勾选模型中指定一个默认模型。" });
      return;
    }
    if (imagegenEnabled && !imageApiKey.trim() && !imagegen.configured) {
      setNotice({ kind: "error", text: "启用生图功能需要填写并确认镜子AI Image Key 的 gpt-image-2 模型权限。" });
      return;
    }
    if (imagegenEnabled && imageApiKey.trim() && !imageKeyValidated) {
      setNotice({ kind: "error", text: "请先确认镜子AI Image Key 的 gpt-image-2 模型权限。" });
      return;
    }
    const token = beginOperation("enable");
    if (!token) return;
    if (!("__TAURI_INTERNALS__" in window)) {
      const now = Date.now();
      setAccess({
        ...emptyStatus,
        phase: "active",
        active: true,
        mode,
        modelCount: allSelectedModelIds.length,
        defaultModel,
        currentProvider: "mirrorplus",
        originalProvider: "openai",
        baselineExists: true,
        baselineCreatedAtMs: now,
        sessionSyncStatus: "synced",
        mcpServerCount: 1,
        pluginMarketplaceStatus: "ready",
        lastMessage: "预览模式：接入流程已完成。",
        lastOperationAtMs: now,
      });
      if (imagegenEnabled) {
        setImagegen({
          ...emptyImagegenStatus,
          enabled: true,
          configured: true,
          managed: true,
          helperAvailable: true,
          skillAvailable: true,
          sourceCommit: "preview",
        });
      }
      setApiKey("");
      setDiscovery(null);
      setSelectedModelIds([]);
      setEnterpriseApiKey("");
      setEnterpriseDiscovery(null);
      setEnterpriseSelectedModelIds([]);
      setDefaultModel("");
      setImageApiKey("");
      setImageKeyValidated(false);
      setNotice({ kind: "ok", text: "预览模式：已模拟完成安全接入，未写入任何本机文件。" });
      endOperation(token);
      return;
    }
    try {
      const result = await invoke<
        CommandResult<{
          access: AccessStatus;
          models: MirrorModel[];
          imagegen?: ImagegenStatus;
          fullyReady?: boolean;
        }>
      >(
        "enable_mirror_access",
        {
          apiKey: configuredGroups[0].apiKey,
          mode,
          selectedModelIds: configuredGroups[0].selectedModelIds,
          defaultModel,
          keyGroups: configuredGroups.map((group) => ({
            id: group.id,
            label: group.label,
            apiKey: group.apiKey,
            selectedModelIds: group.selectedModelIds,
          })),
          imagegenEnabled,
          imageApiKey: imageApiKey.trim() || null,
          replaceExistingGroups: true,
        },
      );
      if (result.access) {
        setAccess(result.access);
        if (result.imagegen) setImagegen(result.imagegen);
      }
      if (isFailure(result.status) || !result.access) {
        setNotice({ kind: "error", text: result.message });
        return;
      }
      setApiKey("");
      setDiscovery(null);
      setSelectedModelIds([]);
      setEnterpriseApiKey("");
      setEnterpriseDiscovery(null);
      setEnterpriseSelectedModelIds([]);
      setDefaultModel("");
      setImageApiKey("");
      setImageKeyValidated(false);
      setShowKey(false);
      setShowEnterpriseKey(false);
      setShowImageKey(false);
      setNotice({
        kind: result.access.phase === "active" && result.fullyReady !== false ? "ok" : "info",
        text: result.message,
      });
    } catch (error) {
      setNotice({ kind: "error", text: String(error) });
    } finally {
      endOperation(token);
    }
  };

  const repair = async () => {
    const token = beginOperation("repair");
    if (!token) return;
    if (!("__TAURI_INTERNALS__" in window)) {
      setAccess((current) => ({
        ...current,
        phase: current.active ? "active" : current.phase,
        sessionSyncStatus: current.active ? "synced" : current.sessionSyncStatus,
        lastMessage: "预览模式：会话归属检查已完成。",
        lastOperationAtMs: Date.now(),
      }));
      setNotice({ kind: "ok", text: "预览模式：已模拟完成会话修复，未读取或修改历史会话。" });
      endOperation(token);
      return;
    }
    try {
      const result = await invoke<CommandResult<{ access: AccessStatus }>>("repair_mirror_sessions");
      if (result.access) setAccess(result.access);
      setNotice({ kind: isFailure(result.status) ? "error" : result.status === "degraded" ? "info" : "ok", text: result.message });
    } catch (error) {
      setNotice({ kind: "error", text: String(error) });
    } finally {
      endOperation(token);
    }
  };

  const cleanupSessionIndex = async () => {
    const token = beginOperation("cleanup-sessions");
    if (!token) return;
    if (!("__TAURI_INTERNALS__" in window)) {
      setNotice({ kind: "info", text: "预览模式不会读取或整理本机会话。" });
      endOperation(token);
      return;
    }
    try {
      const preview = await invoke<CommandResult<SessionIndexCleanupPreview>>("preview_session_index_cleanup");
      if (isFailure(preview.status)) {
        setNotice({ kind: "error", text: preview.message });
        return;
      }
      if (preview.candidates.length === 0) {
        setNotice({ kind: "ok", text: "会话列表无需整理。" });
        return;
      }
      const names = preview.candidates
        .slice(0, 6)
        .map((candidate) => candidate.threadName || candidate.id)
        .join("\n");
      const remaining = preview.candidates.length > 6 ? `\n另有 ${preview.candidates.length - 6} 条` : "";
      if (!window.confirm(`发现 ${preview.candidates.length} 条失效会话索引：\n\n${names}${remaining}\n\n继续前会完整备份原索引。`)) {
        setNotice({ kind: "info", text: "未执行会话列表整理。" });
        return;
      }
      const result = await invoke<CommandResult<{ prunedEntries: number; backupDir: string | null }>>(
        "apply_session_index_cleanup",
        {
          snapshotSha256: preview.snapshotSha256,
          threadIds: preview.candidates.map((candidate) => candidate.id),
        },
      );
      setNotice({ kind: isFailure(result.status) ? "error" : "ok", text: result.message });
    } catch (error) {
      setNotice({ kind: "error", text: String(error) });
    } finally {
      endOperation(token);
    }
  };

  const restore = async () => {
    const token = beginOperation("restore");
    if (!token) return;
    setRestoreConfirmOpen(false);
    if (!("__TAURI_INTERNALS__" in window)) {
      setAccess(emptyStatus);
      setImagegen(emptyImagegenStatus);
      setImagegenEnabled(false);
      setApiKey("");
      setDiscovery(null);
      setSelectedModelIds([]);
      setEnterpriseApiKey("");
      setEnterpriseDiscovery(null);
      setEnterpriseSelectedModelIds([]);
      setDefaultModel("");
      setImageApiKey("");
      setImageKeyValidated(false);
      setNotice({ kind: "ok", text: "预览模式：已模拟恢复使用前状态，未写入任何本机文件。" });
      endOperation(token);
      return;
    }
    try {
      const result = await invoke<CommandResult<{ access: AccessStatus; imagegen?: ImagegenStatus }>>("restore_pre_mirror_state");
      if (result.access) setAccess(result.access);
      setDiscovery(null);
      setSelectedModelIds([]);
      setEnterpriseApiKey("");
      setEnterpriseDiscovery(null);
      setEnterpriseSelectedModelIds([]);
      setDefaultModel("");
      setApiKey("");
      setImageApiKey("");
      setImageKeyValidated(false);
      if (result.imagegen) {
        setImagegen(result.imagegen);
        setImagegenEnabled(result.imagegen.enabled);
      } else if (result.status === "ok") {
        setImagegen(emptyImagegenStatus);
        setImagegenEnabled(false);
      }
      setNotice({ kind: isFailure(result.status) ? "error" : result.status === "degraded" ? "info" : "ok", text: result.message });
    } catch (error) {
      setNotice({ kind: "error", text: String(error) });
    } finally {
      endOperation(token);
    }
  };

  const recoverFromBaseline = async () => {
    const token = beginOperation("recover-baseline");
    if (!token) return;
    setBaselineRecoveryConfirmOpen(false);
    if (!("__TAURI_INTERNALS__" in window)) {
      setAccess(emptyStatus);
      setImagegen(emptyImagegenStatus);
      setImagegenEnabled(false);
      setNotice({ kind: "ok", text: "预览模式：已模拟从首次还原点恢复，未写入任何本机文件。" });
      endOperation(token);
      return;
    }
    try {
      const result = await invoke<CommandResult<{ access: AccessStatus; imagegen?: ImagegenStatus }>>(
        "recover_mirror_from_baseline",
      );
      if (result.access) setAccess(result.access);
      if (result.imagegen) {
        setImagegen(result.imagegen);
        setImagegenEnabled(result.imagegen.enabled);
      }
      if (isFailure(result.status)) {
        setNotice({ kind: "error", text: result.message });
        return;
      }
      setApiKey("");
      setDiscovery(null);
      setSelectedModelIds([]);
      setEnterpriseApiKey("");
      setEnterpriseDiscovery(null);
      setEnterpriseSelectedModelIds([]);
      setDefaultModel("");
      setImageApiKey("");
      setImageKeyValidated(false);
      setNotice({ kind: result.status === "degraded" ? "info" : "ok", text: result.message });
      await refresh(token);
    } catch (error) {
      setNotice({ kind: "error", text: String(error) });
    } finally {
      endOperation(token);
    }
  };

  const launch = async () => {
    const token = beginOperation("launch");
    if (!token) return;
    if (!("__TAURI_INTERNALS__" in window)) {
      setNotice({ kind: "info", text: "预览模式不会实际启动 Codex。" });
      endOperation(token);
      return;
    }
    try {
      const latestPreflightResult = await invoke<CommandResult<{ preflight: Preflight }>>("get_mirror_preflight");
      const latestPreflight = latestPreflightResult.preflight;
      if (latestPreflight) setPreflight(latestPreflight);
      const existingCodex = latestPreflight?.codexRunning === true;
      if (isFailure(latestPreflightResult.status) || (!existingCodex && !isCodexLaunchReady(latestPreflight))) {
        setPreflightOpen(true);
        setNotice({
          kind: "error",
          text: isFailure(latestPreflightResult.status)
            ? latestPreflightResult.message
            : "未检测到可真实启动的 Codex，请重新检测、安装或选择 Codex.exe。",
        });
        return;
      }
      const result = await invoke<CommandResult<Record<string, unknown>>>("launch_codex_plus", {
        request: { appPath: latestPreflight.codexAppPath },
      });
      setNotice({
        kind: isFailure(result.status) ? "error" : result.status === "degraded" ? "info" : "ok",
        text: result.message,
      });
    } catch (error) {
      setNotice({ kind: "error", text: String(error) });
    } finally {
      endOperation(token);
    }
  };

  const openPricing = async () => {
    try {
      const result = await invoke<CommandResult<Record<string, unknown>>>("open_external_url", {
        url: "https://api.jingziai.club/pricing",
      });
      if (isFailure(result.status)) {
        setNotice({ kind: "error", text: result.message });
      }
    } catch (error) {
      setNotice({ kind: "error", text: String(error) });
    }
  };

  const installCodex = async () => {
    const token = beginOperation("install-codex");
    if (!token) return;
    if (!("__TAURI_INTERNALS__" in window)) {
      setNotice({ kind: "info", text: "桌面应用中将通过 winget 或 Microsoft Store 安装 Codex。" });
      endOperation(token);
      return;
    }
    try {
      const result = await invoke<CommandResult<Record<string, unknown>>>("install_codex_desktop");
      setNotice({
        kind: isFailure(result.status) ? "error" : result.status === "degraded" ? "info" : "ok",
        text: result.message,
      });
      await refresh(token);
    } catch (error) {
      setNotice({ kind: "error", text: String(error) });
    } finally {
      endOperation(token);
    }
  };

  const chooseCodex = async () => {
    const token = beginOperation("choose-codex");
    if (!token) return;
    if (!("__TAURI_INTERNALS__" in window)) {
      setNotice({ kind: "info", text: "桌面应用中可手动选择 Codex.exe。" });
      endOperation(token);
      return;
    }
    try {
      const selected = await open({
        directory: false,
        multiple: false,
        title: "选择 Codex.exe",
        filters: [{ name: "Codex Desktop", extensions: ["exe"] }],
      });
      if (typeof selected !== "string") return;
      const result = await invoke<CommandResult<Record<string, unknown>>>("set_codex_app_path", {
        appPath: selected,
      });
      setNotice({ kind: isFailure(result.status) ? "error" : "ok", text: result.message });
      if (!isFailure(result.status)) await refresh(token);
    } catch (error) {
      setNotice({ kind: "error", text: String(error) });
    } finally {
      endOperation(token);
    }
  };

  const active = access.active;
  const degraded = access.phase === "active_degraded";
  const restoreFailed = access.phase === "restore_failed";
  const stateUnreadable = access.phase === "state_unreadable";
  const preflightChecks = preflight?.checks ?? [];
  const readyCheckCount = preflightChecks.filter((check) => check.ready).length;
  const selectedModelCount = selectedModelIds.length + enterpriseSelectedModelIds.length;
  const showImagegenOptions = true;
  const currentOperation = operationFeedback(busy);
  const codexLaunchReady = isCodexLaunchReady(preflight);
  const environmentReady = preflight?.ready === true;
  const environmentChecking = preflight === null;
  const applyBlockReason = getApplyBlockReason({
    busy: busy !== null,
    stateUnreadable,
    baselineExists: access.baselineExists,
    preflight,
    sandboxSupported: false,
    sandboxReady: true,
    sandboxStatus: null,
    mode,
    mixedAuthReady: mixedAuth?.ready ?? null,
    mixedAuthMessage: mixedAuth?.message ?? "",
    groups: [
      {
        label: "CodexPro Key",
        apiKey,
        verified: discovery !== null,
        selectedModelIds,
      },
      {
        label: "企业GPT专线（极稳）",
        apiKey: enterpriseApiKey,
        verified: enterpriseDiscovery !== null,
        selectedModelIds: enterpriseSelectedModelIds,
      },
    ],
    defaultModel,
    imagegenEnabled,
    imagegenConfigured: imagegen.configured,
    imageApiKey,
    imageKeyValidated,
  });

  return (
    <main aria-busy={busy !== null} className="simple-shell">
      <fieldset
        aria-label="Mirror X Codex 管理操作"
        disabled={busy !== null}
        style={{ border: 0, display: "contents", margin: 0, minWidth: 0, padding: 0 }}
      >
      <header className="simple-header">
        <div className="simple-brand">
          <div className="simple-brand-mark" aria-hidden="true">M×C</div>
          <div>
            <strong>Mirror X Codex</strong>
            <span>镜子AI · Codex 接入工具</span>
          </div>
        </div>
        <div className="simple-header-actions">
          <button className="server-link" onClick={() => void openPricing()} type="button">
            <Globe2 size={15} />镜子AI<ExternalLink size={13} />
          </button>
          <button className="icon-button" onClick={() => void refresh()} disabled={busy !== null} title="刷新状态" type="button">
            <RefreshCw size={18} className={busy === "loading" ? "spin" : ""} />
          </button>
        </div>
      </header>

      <section className={`status-band ${restoreFailed || degraded || stateUnreadable ? "warning" : active ? "connected" : "idle"}`}>
        <div className="status-icon">
          {active && !degraded ? <Activity size={24} /> : degraded || stateUnreadable ? <AlertTriangle size={24} /> : <KeyRound size={24} />}
        </div>
        <div className="status-copy">
          <span>当前状态</span>
          <strong>
            {stateUnreadable
              ? "接管状态无法读取，修改已停止"
              : restoreFailed
              ? "恢复未完成，可继续重试"
              : active
                ? (degraded
                  ? "接入生效，会话待修复"
                  : "镜子AI接入已完整生效")
                : "原始 Codex 环境"}
          </strong>
          <small>
            {stateUnreadable
              ? access.lastMessage
              : active
              ? `${access.mode === "pureApi" ? "纯 API" : "混合 API"} · ${access.modelCount} 个模型 · ${access.defaultModel}`
              : `当前 Provider · ${access.currentProvider}`}
          </small>
        </div>
        {active ? (
          <button
            className="status-launch-button"
            disabled={busy !== null || !codexLaunchReady}
            onClick={() => void launch()}
            title={codexLaunchReady ? "打开已确认的 Codex" : "请先重新检测、安装或选择可启动的 Codex"}
            type="button"
          >
            {busy === "launch" ? <LoaderCircle className="spin" size={17} /> : <Rocket size={17} />}
            打开 Codex
          </button>
        ) : null}
      </section>

      {notice ? (
        <div
          className={`simple-notice ${notice.kind}`}
          ref={noticeRef}
          role={notice.kind === "error" ? "alert" : "status"}
        >
          {notice.kind === "ok" ? <Check size={17} /> : notice.kind === "error" ? <AlertTriangle size={17} /> : <ShieldCheck size={17} />}
          <span>{notice.text}</span>
          <button onClick={() => setNotice(null)} title="关闭" type="button">×</button>
        </div>
      ) : null}

      {currentOperation ? (
        <div className="operation-feedback" role="status" aria-live="polite">
          <LoaderCircle className="spin" size={18} aria-hidden="true" />
          <div>
            <strong>{currentOperation.title}</strong>
            <span>{currentOperation.detail}</span>
          </div>
          <div className="operation-progress" aria-hidden="true"><span /></div>
        </div>
      ) : null}

      {stateUnreadable ? (
        <section className="state-recovery-panel" role="alert">
          <span className="state-recovery-icon" aria-hidden="true"><AlertTriangle size={19} /></span>
          <div className="state-recovery-copy">
            <strong>接管状态损坏，所有新写入已停止</strong>
            <span>
              {access.baselineExists
                ? "检测到首次接入时校验通过的还原点，可在二次确认后执行灾难恢复；历史会话不会被删除。"
                : "没有可验证的首次接入还原点，本工具不会猜测或覆盖配置。请重新检测；仍无法恢复时请保留诊断日志并联系支持。"}
            </span>
          </div>
          <div className="state-recovery-actions">
            {access.baselineExists ? (
              <button
                className="baseline-recovery-button"
                disabled={preflight?.codexRunning === true}
                onClick={() => setBaselineRecoveryConfirmOpen(true)}
                title={preflight?.codexRunning ? "请先停止任务并完全退出 Codex" : "从校验通过的首次接入还原点恢复"}
                type="button"
              >
                <RotateCcw size={16} />{preflight?.codexRunning ? "退出 Codex 后恢复" : "从还原点灾难恢复"}
              </button>
            ) : null}
            <button className="secondary-button" onClick={() => void refresh()} type="button">
              <RefreshCw size={15} />重新检测
            </button>
          </div>
        </section>
      ) : null}

      <details
        className={`preflight-section ${environmentReady ? "ready" : "blocked"}`}
        onToggle={(event) => setPreflightOpen(event.currentTarget.open)}
        open={preflightOpen}
      >
        <summary className="preflight-heading">
          <div>
            <span className="preflight-summary-icon">
              {environmentReady
                ? <Check size={16} />
                : environmentChecking
                  ? <LoaderCircle className="spin" size={16} />
                  : <AlertTriangle size={16} />}
            </span>
            <span className="preflight-summary-copy">
              <strong>{environmentReady ? "本机环境已就绪" : environmentChecking ? "正在检查本机环境" : "本机环境需要处理"}</strong>
              <small>
                {preflight
                  ? `${readyCheckCount} / ${preflightChecks.length} 项基础检查通过`
                  : "检查 Codex、配置目录与原始配置"}
              </small>
            </span>
          </div>
          <ChevronDown className="disclosure-chevron" size={18} />
        </summary>
        <div className="preflight-content">
          <div className="preflight-grid">
            {preflightChecks.map((check) => (
              <div className={check.ready ? "preflight-item ready" : "preflight-item blocked"} key={check.id}>
                {check.ready ? <Check size={17} /> : <CircleX size={17} />}
                <div><strong>{check.label}</strong><span>{check.detail}</span></div>
              </div>
            ))}
          </div>
          <div className="preflight-tools">
            <div className="context-strip" aria-label="Codex 能力保留状态">
              <span className="context-ready">
                <Cable size={15} />MCP {access.mcpServerCount > 0 ? `${access.mcpServerCount} 个已保留` : "配置保持不变"}
              </span>
              <span className={access.pluginMarketplaceStatus === "ready" ? "context-ready" : "context-pending"}>
                <Blocks size={15} />插件市场 {pluginMarketplaceLabel(access.pluginMarketplaceStatus)}
              </span>
            </div>
            <button className="site-link" onClick={() => void refresh()} disabled={busy !== null} type="button">
              <RefreshCw size={14} />重新检测
            </button>
          </div>
          {preflight && !preflight.codexInstalled ? (
            <div className="codex-install-actions">
              {!preflight.codexInstalled ? (
                <button
                  className="install-codex-button"
                  disabled={busy !== null}
                  onClick={() => void installCodex()}
                  type="button"
                >
                  {busy === "install-codex" ? <LoaderCircle className="spin" size={17} /> : <Download size={17} />}
                  自动安装 Codex
                </button>
              ) : null}
              <button
                className="choose-codex-button"
                disabled={busy !== null}
                onClick={() => void chooseCodex()}
                type="button"
              >
                {busy === "choose-codex" ? <LoaderCircle className="spin" size={17} /> : <FolderOpen size={17} />}
                选择 Codex.exe
              </button>
            </div>
          ) : null}
        </div>
      </details>

      <details
        className="connect-section"
        onToggle={(event) => setConnectionOpen(event.currentTarget.open)}
        open={connectionOpen}
      >
        <summary className="section-heading">
          <div>
            <span>{active ? "更新连接" : "连接镜子AI"}</span>
            <h1>{active ? "更新 API Key 或连接模式" : "连接镜子AI模型"}</h1>
          </div>
          <div className="section-heading-meta">
            {access.baselineExists ? <span className="baseline-badge"><ShieldCheck size={15} /> 已保护原始配置</span> : null}
            <ChevronDown className="disclosure-chevron" size={20} />
          </div>
        </summary>
        <div className="connect-content">

        <div className="field-heading group-intro">
          <span className="field-label">1 · 提供 Key</span>
          <button className="site-link" onClick={() => void openPricing()} type="button">
            前往镜子AI获取 Key
            <ExternalLink size={14} />
          </button>
        </div>

        <div className="key-group-grid">
        <ModelKeyGroup
          apiKey={apiKey}
          busy={busy === "validate-codexpro"}
          defaultModel={defaultModel}
          discovery={discovery}
          fallbackDefault={enterpriseSelectedModelIds[0] ?? ""}
          filter={modelFilters.codexpro}
          groupId="codexpro"
          hint="GPT 与 Grok 共用 CodexPro 分组 Key"
          label="CodexPro Key"
          onApiKeyChange={(value) => {
            validationEpoch.current.codexpro += 1;
            if (selectedModelIds.includes(defaultModel)) {
              setDefaultModel(enterpriseSelectedModelIds[0] ?? "");
            }
            setApiKey(value);
            setDiscovery(null);
            setSelectedModelIds([]);
          }}
          onDefaultModelChange={setDefaultModel}
          onFilterChange={(value) => setModelFilters((current) => ({ ...current, codexpro: value }))}
          onSelectedModelIdsChange={setSelectedModelIds}
          onShowKeyChange={setShowKey}
          onValidate={() => void validate("codexpro")}
          selectedModelIds={selectedModelIds}
          showKeyInput
          showKey={showKey}
        />

        <ModelKeyGroup
          apiKey={enterpriseApiKey}
          busy={busy === "validate-enterprise"}
          defaultModel={defaultModel}
          discovery={enterpriseDiscovery}
          fallbackDefault={selectedModelIds[0] ?? ""}
          filter={modelFilters.enterprise}
          groupId="enterprise"
          hint="更稳定的企业 GPT 专线；使用该分组独立生成的 Key"
          label="企业GPT专线（极稳）"
          onApiKeyChange={(value) => {
            validationEpoch.current.enterprise += 1;
            if (enterpriseSelectedModelIds.includes(defaultModel)) {
              setDefaultModel(selectedModelIds[0] ?? "");
            }
            setEnterpriseApiKey(value);
            setEnterpriseDiscovery(null);
            setEnterpriseSelectedModelIds([]);
          }}
          onDefaultModelChange={setDefaultModel}
          onFilterChange={(value) => setModelFilters((current) => ({ ...current, enterprise: value }))}
          onSelectedModelIdsChange={setEnterpriseSelectedModelIds}
          onShowKeyChange={setShowEnterpriseKey}
          onValidate={() => void validate("enterprise")}
          selectedModelIds={enterpriseSelectedModelIds}
          showKeyInput
          showKey={showEnterpriseKey}
        />
        </div>

        <div className={`connection-options-grid ${showImagegenOptions ? "with-imagegen" : ""}`}>
        {showImagegenOptions ? (
        <section className={`imagegen-card ${imagegenEnabled ? "enabled" : ""}`}>
          <div className="imagegen-heading">
            <div className="imagegen-title">
              <span className="imagegen-icon"><Image size={17} /></span>
              <div>
                <strong>镜子AI生图</strong>
                <span>直接描述想生成的图片，Codex 自动完成生图</span>
              </div>
            </div>
            <label className="switch-control">
              <input
                checked={imagegenEnabled}
                onChange={(event) => setImagegenEnabled(event.target.checked)}
                type="checkbox"
              />
              <span aria-hidden="true" />
              <strong>{imagegenEnabled ? "已选择启用" : "未启用"}</strong>
            </label>
          </div>

          {imagegenEnabled ? (
            <>
              <div className="key-group-input-row image-key-row">
                <div className="key-field">
                  <KeyRound size={17} />
                  <input
                    autoComplete="off"
                    onChange={(event) => {
                      validationEpoch.current.image += 1;
                      setImageApiKey(event.target.value);
                      setImageKeyValidated(false);
                    }}
                    placeholder={imagegen.configured ? "已保存 Image Key；留空可继续使用" : "粘贴镜子AI Image Key"}
                    spellCheck={false}
                    type={showImageKey ? "text" : "password"}
                    value={imageApiKey}
                  />
                  <button
                    aria-label={showImageKey ? "隐藏 Image Key" : "显示 Image Key"}
                    className="field-icon"
                    onClick={() => setShowImageKey((current) => !current)}
                    type="button"
                  >
                    {showImageKey ? <EyeOff size={17} /> : <Eye size={17} />}
                  </button>
                </div>
                <button
                  className="validate-group-button"
                  disabled={busy !== null || !imageApiKey.trim()}
                  onClick={() => void validateImageKey()}
                  type="button"
                >
                  {busy === "validate-image" ? <LoaderCircle className="spin" size={15} /> : <Check size={15} />}
                  {imageKeyValidated ? "权限已确认" : "检查权限"}
                </button>
              </div>
              <p className="imagegen-note">
                此处只检查 gpt-image-2 模型权限，不发送真实生图请求。配置一次后，无需输入 Skill 名称或命令。
              </p>
            </>
          ) : null}
        </section>
        ) : null}

        <section className="mode-panel">
        <div className="mode-label">2 · 选择接入模式</div>
        <div className="mode-control" role="radiogroup" aria-label="接入模式">
          <button className={mode === "mixedApi" ? "selected" : ""} onClick={() => setMode("mixedApi")} role="radio" aria-checked={mode === "mixedApi"} type="button">
            <strong>混合 API</strong>
            <span>保留 ChatGPT 登录与官方能力</span>
          </button>
          <button className={mode === "pureApi" ? "selected" : ""} onClick={() => setMode("pureApi")} role="radio" aria-checked={mode === "pureApi"} type="button">
            <strong>纯 API</strong>
            <span>全部模型请求通过镜子AI</span>
          </button>
        </div>
        <div className={`mixed-auth-status ${mode === "pureApi" || mixedAuth?.ready ? "ready" : "blocked"}`}>
          <span aria-hidden="true">
            {mode === "pureApi" || mixedAuth?.ready ? <Check size={15} /> : <AlertTriangle size={15} />}
          </span>
          <div>
            <strong>
              {mode === "pureApi"
                ? "纯 API 不依赖 ChatGPT 登录"
                : mixedAuth?.ready
                  ? "ChatGPT 登录已确认"
                  : mixedAuth
                    ? "混合 API 暂不可用"
                    : "正在确认 ChatGPT 登录"}
            </strong>
            <small>
              {mode === "pureApi"
                ? "所有模型请求使用已验证的镜子AI Key。"
                : mixedAuth?.message ?? "等待真实 Codex CLI 返回登录状态。"}
            </small>
          </div>
          {mode === "mixedApi" && !mixedAuth?.ready ? (
            <div className="mixed-auth-actions">
              {codexLaunchReady ? (
                <button onClick={() => void launch()} type="button">
                  <Rocket size={14} />打开 Codex 登录
                </button>
              ) : null}
              <button onClick={() => void refresh()} type="button">
                <RefreshCw size={14} />重新检测
              </button>
            </div>
          ) : null}
        </div>
        </section>
        </div>

        <p className="group-routing-note">
          应用期间请完全退出 Codex；完成后从本工具打开 Codex。
        </p>

        <div className="primary-actions">
          <span className={`apply-readiness ${applyBlockReason ? "blocked" : "ready"}`} role="status">
            {applyBlockReason ? <AlertTriangle size={15} /> : <Check size={15} />}
            <span>
              {applyBlockReason
                ?? `已验证 ${selectedModelCount} 个模型，可以安全${active ? "更新" : "应用"}接入。`}
            </span>
          </span>
          <button
            className="primary-button"
            disabled={applyBlockReason !== null}
            onClick={() => void enable()}
            type="button"
          >
            {busy === "enable" ? <LoaderCircle className="spin" size={18} /> : <Rocket size={18} />}
            {active ? "更新连接" : "应用接入"}
          </button>
        </div>
        </div>
      </details>

      {active || restoreFailed ? (
        <section className="active-actions">
          {active ? (
            <button
              className="launch-button"
              disabled={busy !== null || !codexLaunchReady}
              onClick={() => void launch()}
              title={codexLaunchReady ? "打开已确认的 Codex" : "请先重新检测、安装或选择可启动的 Codex"}
              type="button"
            >
              {busy === "launch" ? <LoaderCircle className="spin" size={18} /> : <Rocket size={18} />}
              打开 Codex
            </button>
          ) : null}
          {degraded || restoreFailed ? (
            <button className="secondary-button" disabled={busy !== null} onClick={() => void repair()} type="button">
              {busy === "repair" ? <LoaderCircle className="spin" size={18} /> : <RefreshCw size={18} />}
              {restoreFailed ? "重试会话归属恢复" : "重试会话修复"}
            </button>
          ) : null}
          {active ? (
            <button className="secondary-button" disabled={busy !== null} onClick={() => void cleanupSessionIndex()} type="button">
              {busy === "cleanup-sessions" ? <LoaderCircle className="spin" size={18} /> : <Search size={18} />}
              整理会话列表
            </button>
          ) : null}
          <button className="restore-button" disabled={busy !== null || !access.baselineExists} onClick={() => setRestoreConfirmOpen(true)} type="button">
            {busy === "restore" ? <LoaderCircle className="spin" size={18} /> : <RotateCcw size={18} />}
            恢复使用前状态
          </button>
        </section>
      ) : null}

      <details className="mobile-control-disclosure">
        <summary>
          <span className="mobile-disclosure-icon"><Smartphone size={17} /></span>
          <span>
            <strong>手机远程控制</strong>
            <small>按需开启手机查看与续聊</small>
          </span>
          <ChevronDown className="disclosure-chevron" size={18} />
        </summary>
        <div className="mobile-control-body">
          <MobileControlPanel onNotice={setNotice} />
        </div>
      </details>

      {restoreConfirmOpen ? (
        <div
          className="confirm-overlay"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget && busy === null) setRestoreConfirmOpen(false);
          }}
          role="presentation"
        >
          <section
            aria-describedby="restore-confirm-description restore-confirm-warning"
            aria-labelledby="restore-confirm-title"
            aria-modal="true"
            className="confirm-dialog"
            onKeyDown={(event) => {
              if (event.key !== "Tab") return;
              const buttons = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>("button:not(:disabled)"));
              const first = buttons[0];
              const last = buttons.at(-1);
              if (!first || !last) return;
              if (event.shiftKey && document.activeElement === first) {
                event.preventDefault();
                last.focus();
              } else if (!event.shiftKey && document.activeElement === last) {
                event.preventDefault();
                first.focus();
              }
            }}
            role="dialog"
          >
            <div className="confirm-dialog-icon" aria-hidden="true"><AlertTriangle size={22} /></div>
            <div className="confirm-dialog-copy">
              <h2 id="restore-confirm-title">恢复首次接入前状态？</h2>
              <p id="restore-confirm-description">这会撤销 Mirror X Codex 接入，并按首次接入时创建的还原点恢复配置和会话归属，不会删除历史会话。</p>
              <small id="restore-confirm-warning">恢复期间 Codex 必须完全退出；如果仍在运行，操作会被拒绝，不会强行修改。</small>
            </div>
            <div className="confirm-dialog-actions">
              <button autoFocus className="secondary-button" onClick={() => setRestoreConfirmOpen(false)} type="button">
                保留当前接入
              </button>
              <button className="confirm-restore-button" onClick={() => void restore()} type="button">
                <RotateCcw size={17} />确认恢复
              </button>
            </div>
          </section>
        </div>
      ) : null}

      {baselineRecoveryConfirmOpen ? (
        <div
          className="confirm-overlay"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget && busy === null) setBaselineRecoveryConfirmOpen(false);
          }}
          role="presentation"
        >
          <section
            aria-describedby="baseline-recovery-description baseline-recovery-warning"
            aria-labelledby="baseline-recovery-title"
            aria-modal="true"
            className="confirm-dialog"
            onKeyDown={(event) => {
              if (event.key !== "Tab") return;
              const buttons = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>("button:not(:disabled)"));
              const first = buttons[0];
              const last = buttons.at(-1);
              if (!first || !last) return;
              if (event.shiftKey && document.activeElement === first) {
                event.preventDefault();
                last.focus();
              } else if (!event.shiftKey && document.activeElement === last) {
                event.preventDefault();
                first.focus();
              }
            }}
            role="dialog"
          >
            <div className="confirm-dialog-icon" aria-hidden="true"><AlertTriangle size={22} /></div>
            <div className="confirm-dialog-copy">
              <h2 id="baseline-recovery-title">从首次接入还原点灾难恢复？</h2>
              <p id="baseline-recovery-description">
                仅在接管状态损坏时使用。工具会先保留操作前快照，再校验还原点并恢复 Codex 配置、生图配置和会话归属，不会删除历史会话。
              </p>
              <small id="baseline-recovery-warning">
                这是二次确认。请先完全退出 Codex；还原点校验失败时会立即停止，不会使用不可信内容覆盖当前文件。
              </small>
            </div>
            <div className="confirm-dialog-actions">
              <button autoFocus className="secondary-button" onClick={() => setBaselineRecoveryConfirmOpen(false)} type="button">
                返回诊断
              </button>
              <button className="confirm-recovery-button" onClick={() => void recoverFromBaseline()} type="button">
                <RotateCcw size={17} />确认灾难恢复
              </button>
            </div>
          </section>
        </div>
      ) : null}

      <footer className="simple-footer">
        <span>Mirror X Codex · 本机配置保护：{access.baselineExists ? formatDate(access.baselineCreatedAtMs) : "首次连接时创建"}</span>
        <span>会话状态：{sessionLabel(access.sessionSyncStatus)}</span>
      </footer>
      </fieldset>
    </main>
  );
}

type ModelKeyGroupProps = {
  apiKey: string;
  busy: boolean;
  defaultModel: string;
  discovery: Discovery | null;
  fallbackDefault: string;
  filter: string;
  groupId: string;
  hint: string;
  label: string;
  selectedModelIds: string[];
  showKeyInput: boolean;
  showKey: boolean;
  onApiKeyChange: (value: string) => void;
  onDefaultModelChange: (value: string) => void;
  onFilterChange: (value: string) => void;
  onSelectedModelIdsChange: (value: string[]) => void;
  onShowKeyChange: (value: boolean) => void;
  onValidate: () => void;
};

function ModelKeyGroup({
  apiKey,
  busy,
  defaultModel,
  discovery,
  fallbackDefault,
  filter,
  groupId,
  hint,
  label,
  selectedModelIds,
  showKeyInput,
  showKey,
  onApiKeyChange,
  onDefaultModelChange,
  onFilterChange,
  onSelectedModelIdsChange,
  onShowKeyChange,
  onValidate,
}: ModelKeyGroupProps) {
  return (
    <div className="key-group">
      <div className="key-group-heading">
        <div>
          <strong>{label}</strong>
          <span>{hint}</span>
        </div>
        {discovery ? <span className="key-verified"><Check size={13} />已验证</span> : null}
      </div>
      {showKeyInput ? (
      <div className="key-group-input-row">
        <div className="key-field">
          <KeyRound size={18} />
          <input
            id={`${groupId}-api-key`}
            aria-label={label}
            autoComplete="off"
            onChange={(event) => onApiKeyChange(event.currentTarget.value)}
            placeholder="sk-..."
            spellCheck={false}
            type={showKey ? "text" : "password"}
            value={apiKey}
          />
          <button
            className="field-icon"
            onClick={() => onShowKeyChange(!showKey)}
            title={showKey ? "隐藏 Key" : "显示 Key"}
            type="button"
          >
            {showKey ? <EyeOff size={18} /> : <Eye size={18} />}
          </button>
        </div>
        <button
          className="validate-group-button"
          disabled={busy || !apiKey.trim()}
          onClick={onValidate}
          type="button"
        >
          {busy ? <LoaderCircle className="spin" size={17} /> : <ShieldCheck size={17} />}
          验证
        </button>
      </div>
      ) : null}

      {discovery ? (
        <div className="model-picker">
          <div className="model-picker-heading">
            <div>
              <strong>选择此 Key 负责的模型</strong>
              <span>已选 {selectedModelIds.length} / {discovery.models.length}</span>
            </div>
            <div className="model-picker-actions">
              <button
                onClick={() => {
                  onSelectedModelIdsChange(discovery.models.map((model) => model.id));
                  if (!defaultModel) onDefaultModelChange(discovery.defaultModel);
                }}
                type="button"
              >
                全选
              </button>
              <button
                onClick={() => {
                  onSelectedModelIdsChange([]);
                  if (selectedModelIds.includes(defaultModel)) {
                    onDefaultModelChange(fallbackDefault);
                  }
                }}
                type="button"
              >
                清空
              </button>
            </div>
          </div>
          <label className="model-search">
            <Search size={15} />
            <input
              aria-label={`搜索 ${label} 模型`}
              onChange={(event) => onFilterChange(event.currentTarget.value)}
              placeholder="搜索模型"
              type="search"
              value={filter}
            />
          </label>
          <div className="model-list" role="group" aria-label={`${label} 可插入 Codex 的模型`}>
            {discovery.models
              .filter((model) =>
                `${model.displayName} ${model.id}`
                  .toLowerCase()
                  .includes(filter.trim().toLowerCase()),
              )
              .map((model) => {
                const selected = selectedModelIds.includes(model.id);
                return (
                  <div className={`model-row ${selected ? "selected" : ""}`} key={model.id}>
                    <label className="model-check">
                      <input
                        checked={selected}
                        onChange={(event) => {
                          if (event.currentTarget.checked) {
                            onSelectedModelIdsChange([...selectedModelIds, model.id]);
                            if (!defaultModel) onDefaultModelChange(model.id);
                          } else {
                            const remaining = selectedModelIds.filter((id) => id !== model.id);
                            onSelectedModelIdsChange(remaining);
                            if (defaultModel === model.id) {
                              onDefaultModelChange(remaining[0] ?? fallbackDefault);
                            }
                          }
                        }}
                        type="checkbox"
                      />
                      <span>
                        <strong>{model.displayName}</strong>
                        <small>{model.id} · {formatContextWindow(model.contextWindow)}</small>
                      </span>
                    </label>
                    <label className={`default-choice ${selected ? "" : "disabled"}`}>
                      <input
                        checked={defaultModel === model.id}
                        disabled={!selected}
                        name="default-model"
                        onChange={() => onDefaultModelChange(model.id)}
                        type="radio"
                      />
                      默认
                    </label>
                  </div>
                );
              })}
          </div>
        </div>
      ) : null}
    </div>
  );
}

function formatDate(timestamp: number | null) {
  if (!timestamp) return "未知";
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(timestamp));
}

function sessionLabel(status: string) {
  const labels: Record<string, string> = {
    synced: "已修复",
    degraded: "待重试",
    pending: "等待修复",
    pending_restore: "等待恢复",
    not_run: "未运行",
  };
  return labels[status] ?? status;
}

function pluginMarketplaceLabel(status: string) {
  const labels: Record<string, string> = {
    ready: "已就绪",
    cached: "等待注册",
    missing: "首次连接时准备",
  };
  return labels[status] ?? status;
}

function formatContextWindow(value: number | null) {
  if (!value) return "上下文由服务端决定";
  if (value >= 1_000_000) return `${value / 1_000_000}M 上下文`;
  if (value >= 1_000) return `${Math.round(value / 1_000)}K 上下文`;
  return `${value} 上下文`;
}
