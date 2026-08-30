export type GuardPreflightCheck = {
  id: string;
  ready: boolean;
  label: string;
  detail: string;
};

export type GuardPreflight = {
  ready: boolean;
  codexInstalled: boolean;
  codexAppPath: string | null;
  checks: GuardPreflightCheck[];
};

export type GuardKeyGroup = {
  label: string;
  apiKey: string;
  verified: boolean;
  selectedModelIds: string[];
};

export type ApplyGuardInput = {
  busy: boolean;
  stateUnreadable: boolean;
  baselineExists: boolean;
  preflight: GuardPreflight | null;
  sandboxSupported: boolean | null;
  sandboxReady: boolean;
  sandboxStatus: string | null;
  mode: "mixedApi" | "pureApi";
  mixedAuthReady: boolean | null;
  mixedAuthMessage: string;
  groups: GuardKeyGroup[];
  defaultModel: string;
  imagegenEnabled: boolean;
  imagegenConfigured: boolean;
  imageApiKey: string;
  imageKeyValidated: boolean;
};

export type SandboxRecoveryAction = "enable" | "update_codex" | null;

export function getSandboxRecoveryAction(
  status: string | null | undefined,
  updateAction: string | null | undefined,
): SandboxRecoveryAction {
  if (status === "update_required" && updateAction === "codex_app") return "update_codex";
  if (["not_configured", "full_access_not_configured", "sandbox_state_invalid"].includes(status ?? "")) {
    return "enable";
  }
  if (status === "update_required" && updateAction === "sandbox_environment") return "enable";
  return null;
}

export function getApplyBlockReason(input: ApplyGuardInput): string | null {
  if (input.stateUnreadable) {
    return input.baselineExists
      ? "接管状态无法读取，已停止写入。请先使用还原点灾难恢复，然后重新检测。"
      : "接管状态无法读取且没有可验证还原点，已停止写入。请重新检测；仍无法恢复时请联系支持。";
  }
  if (input.busy) return "请等待当前操作完成。";
  if (!input.preflight) return "正在完成本机体检，请稍候。";
  if (!input.preflight.ready) {
    const failedCheck = input.preflight.checks.find((check) => !check.ready);
    return failedCheck
      ? `本机体检未通过：${failedCheck.label} - ${failedCheck.detail}`
      : "本机体检未通过，请展开上方检查结果处理后重新检测。";
  }
  // Official Windows Sandbox readiness is diagnostic-only. Normal Mirror
  // access uses Codex's native top-level danger-full-access mode and must not
  // be blocked by, or trigger, the desktop sandbox setup workflow.
  if (input.mode === "mixedApi" && input.mixedAuthReady !== true) {
    return input.mixedAuthReady === null
      ? "正在确认当前 Codex 的 ChatGPT 登录状态，请稍候。"
      : input.mixedAuthMessage || "混合 API 需要先在 Codex 中完成 ChatGPT 登录；也可以选择纯 API。";
  }

  const configuredGroups = input.groups.filter(
    (group) => group.apiKey.trim().length > 0 || group.verified,
  );
  if (configuredGroups.length === 0) return "请先填写并验证至少一个分组 Key。";

  const incompleteGroup = configuredGroups.find(
    (group) => !group.apiKey.trim() || !group.verified,
  );
  if (incompleteGroup) return `请先验证 ${incompleteGroup.label}，旧验证结果不会继续使用。`;

  const emptyGroup = configuredGroups.find((group) => group.selectedModelIds.length === 0);
  if (emptyGroup) return `${emptyGroup.label} 至少需要选择一个模型。`;

  const selectedModelIds = configuredGroups.flatMap((group) => group.selectedModelIds);
  if (new Set(selectedModelIds).size !== selectedModelIds.length) {
    return "同一个模型不能同时分配给两个 Key，请取消一处勾选。";
  }
  if (!input.defaultModel || !selectedModelIds.includes(input.defaultModel)) {
    return "请从已勾选模型中指定一个默认模型。";
  }

  if (input.imagegenEnabled && !input.imageApiKey.trim() && !input.imagegenConfigured) {
    return "启用生图需要填写并确认镜子AI Image Key 的 gpt-image-2 权限。";
  }
  if (input.imagegenEnabled && input.imageApiKey.trim() && !input.imageKeyValidated) {
    return "请先确认镜子AI Image Key 的 gpt-image-2 权限。";
  }
  return null;
}

export function isCodexLaunchReady(preflight: GuardPreflight | null): boolean {
  if (!preflight?.codexInstalled || !preflight.codexAppPath?.trim()) return false;
  return preflight.checks.some((check) => check.id === "codex" && check.ready);
}
