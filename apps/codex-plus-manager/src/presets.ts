/**
 * mirror x codex 供应商预设
 * 基于 cc-switch (MIT) 的 codexProviderPresets.ts，作者 Jason Young
 * https://github.com/farion1231/cc-switch
 *
 * 提供一键填充供应商配置的预设模板，包括 Base URL、协议、模型列表等。
 * 去掉了 cc-switch 原始的商业合作标记（isPartner、partnerPromotionKey）。
 */

export type PresetCategory = "official" | "aggregator" | "third_party" | "cn_official";

export type RelayProtocol = "responses" | "chatCompletions";

export interface ProviderPreset {
  id: string;
  name: string;
  websiteUrl?: string;
  apiKeyUrl?: string;
  category: PresetCategory;
  baseUrl: string;
  protocol: RelayProtocol;
  model: string;
  modelList?: string[];
}

/**
 * 预设列表。选择任一预设会自动填充：
 * - name     → 供应商名称
 * - baseUrl  → API 端点
 * - protocol → responses / chatCompletions（根据上游实际协议）
 * - model    → 默认模型名
 * - modelList → 可选模型清单（换行分隔）
 */
export const PRESETS: ProviderPreset[] = [
  {
    id: "jingziai",
    name: "mirror x codex",
    category: "aggregator",
    websiteUrl: "https://api.jingziai.club/pricing",
    apiKeyUrl: "https://api.jingziai.club/pricing",
    baseUrl: "https://api.jingziai.club/v1",
    protocol: "responses",
    model: "gpt-5.5",
    modelList: ["gpt-5.4", "gpt-5.5"],
  },
];
