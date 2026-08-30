# mirror x codex 最终方案调研与第一性原理复盘

> 调研日期：2026-07-17
> 证据范围：Codex 官方手册、GitHub 项目/README、当前仓库源码、mirror+ 公共接口探测。

## 1. 调研结论

市场已经有成熟的“多供应商、多工具、全功能管理器”，继续横向扩展无法建立优势。mirror+ 的最优定位是单服务垂直工具：把复杂配置、模型发现、会话修复和恢复责任全部收进一个可逆事务，让用户只做 Key 和模式两个决定。

最终方案不是删减版 CC Switch，而是不同目标函数：

> CC Switch 优化管理广度；mirror+ 优化接入确定性与退出自由。

## 2. 证据摘要

### 2.1 Codex 官方契约

来源：Codex Manual，2026-07-17 本机拉取的 current 版本。

- 用户级配置位于 `~/.codex/config.toml`；凭据可能在 `auth.json`，也可能在 OS keyring。
- 自定义 provider 使用 `[model_providers.<id>]`，支持 `base_url`、`wire_api` 和认证配置。
- `model_catalog_json` 是官方支持的模型目录入口。
- 官方明确说明 Chat Completions provider 支持已弃用，未来将移除。
- 项目级 `.codex/config.toml` 不允许覆盖 provider/auth 等安全敏感键，因此接管必须发生在用户级配置。

产品含义：默认只使用 Responses API；恢复不能假定凭据永远只存在于 `auth.json`；配置编辑必须保留未知字段与其他层级。

### 2.2 CC Switch

来源：https://github.com/farion1231/cc-switch

- 117k+ stars，覆盖 7 种工具和 50+ provider presets。
- 关键工程实践是 SQLite SSOT、原子写入、自动备份、双向同步。
- FAQ 仍要求用户创建“官方 provider”并重新登录，说明“切回官方”和“恢复使用前状态”不是同一能力。
- 功能面包含 MCP、skills、proxy、usage、sessions、cloud sync，学习成本高。

产品含义：原子写入和备份值得复用；供应商市场、通用编辑器与跨工具能力不应进入 mirror+。

### 2.3 Codex Switch

来源：https://github.com/gstranded/codex-switch

- 把 provider 切换与历史 bucket 同步绑定，验证了“配置切换成功但历史消失”是独立问题。
- 同时改写 rollout JSONL 与 `state_5.sqlite`，并在修改前备份。
- 明确承认跨 provider 的旧会话可能受内部字段和加密内容影响。

产品含义：会话修复必须默认开启，但结果应允许 degraded，不能伪报 100% 成功。

### 2.4 轻量 Provider Switcher

来源：

- https://github.com/ga626/codex-provider-switcher
- https://github.com/RomaCredit/codex-provider-switcher
- GitHub 搜索 `codex provider switch`

共同能力包括配置验证、自动备份、恢复、只读 `/models` 刷新和原子切换。多数项目 stars 很低，说明只提供“可编辑配置表单”不足以形成产品价值。

产品含义：模型刷新是基础能力；差异化必须落在单 Key 零配置和可验证完整恢复。

### 2.5 mirror+ 服务探测

来源：`https://api.jingziai.club/v1/models`，2026-07-17 未带 Key 请求。

- 返回 HTTP 401 JSON，而非 404/HTML。
- 响应头显示 New API/OneAPI 系实现，标准 Bearer `/models` 路径成立。
- pricing 页面可访问，但抓取到的静态正文为空，不能依赖该页面提供机器可读模型元数据。

产品含义：客户端可以通过 `/models` 验证 Key 和发现 ID；上下文窗口最好由服务端增加专用 metadata 字段或 endpoint，不能长期靠客户端猜测。

## 3. 第一性原理

### 3.1 用户真正购买的不是“配置能力”

用户目标是让 Codex 调用模型，而不是学习 provider。每暴露一个 `base_url`、protocol、model slug 或上下文窗口字段，都是把服务方本应承担的复杂性转嫁给用户。

因此：base URL、provider ID、protocol、模型来源全部内置；模型列表动态发现；上下文元数据由服务端负责。

### 3.2 可逆性比功能数量更重要

工具写入的是用户已有生产环境。真正的信任不是“有备份按钮”，而是：写前已存在可校验 baseline、失败自动恢复、恢复后有读后校验、baseline 永不被后续接管覆盖。

因此：接管是事务，不是若干独立按钮。

### 3.3 “原样恢复”与“继续看见新会话”存在冲突

逐字节恢复旧 SQLite/rollout 会删除接管期间产生的新会话；只恢复 config 又会让新会话留在 mirrorplus bucket。正确语义是：配置文件按 baseline 原样恢复，会话数据不倒退，而是同步回 baseline provider。

因此：恢复分为配置恢复和会话归属归一化两个阶段。

### 3.4 动态模型列表不等于真实模型能力

`/models` 通常只返回 ID。客户端若统一写 272K，会把未知值包装成确定事实；若不写 catalog，模型可能不出现在 Codex 选择器。

最终策略：

1. 优先读取服务端 `context_window`、`max_context_window` 等可识别字段。
2. 缺失时使用兼容 fallback，但在 manifest 标记 `context_source = fallback`。
3. 中期要求 mirror+ 提供稳定模型 metadata endpoint，这是服务端发布依赖。

### 3.5 Key 安全与混合模式有结构性矛盾

现有混合模式把 bearer token 写入 provider TOML，保留 ChatGPT auth；纯 API 写入 `auth.json`。两者都可能在本地明文存在。MVP 必须坦诚本地存储事实并严格禁止日志/回显；后续应迁移到 Codex command-backed auth 或 OS keyring，而不是自造加密。

## 4. 逆向失败推演

| 失败情景 | 当前风险 | 最终防线 |
|---|---|---|
| Key 无效 | 写完配置才发现 | 先 `/models` 验证，零写入 |
| `/models` 暂时失败 | 用户无法接管 | 有旧验证缓存时允许明确的离线重用，否则阻止首次接管 |
| 写 auth 成功、写 config 失败 | 半接管 | 操作前快照 + 原子写 + 自动恢复 |
| Codex 正在运行占用 SQLite | 会话部分修复 | 跳过锁文件并返回 degraded，可重试 |
| 用户手改了接管后的 config | 恢复覆盖其新改动 | 恢复前展示 drift；baseline 恢复是明确用户动作，操作前再建 pre-restore 快照 |
| baseline 损坏 | 一键恢复造成更大破坏 | SHA-256 校验失败时禁止自动恢复，保留人工恢复路径 |
| 新版 Codex 改 schema | 修复逻辑误写 | schema 探测、未知列不写、备份、版本化 adapter |
| 服务端模型 ID 变化 | 默认模型失效 | 每次启用刷新模型；当前默认不存在时选服务端首个可用模型 |
| 用户卸载工具 | Codex 留在 mirror+ 状态 | 卸载器提示先恢复；配置本身仍可独立运行 |

## 5. 最终技术方案

### 5.1 新增受管接入域

- `mirror_access`：状态、Key 验证、模型发现、baseline、接管、恢复与校验。
- 不让前端直接拼 TOML/JSON。
- 使用固定 provider ID `mirrorplus` 和固定 Responses base URL。

### 5.2 持久化布局

```text
~/.mirrorplus/
  managed-access.json
  baseline-v1/
    manifest.json
    config.toml
    auth.json
    manager-settings.json
  operations/
    <timestamp>/manifest.json
```

baseline 只创建一次；operation snapshot 每次接管/恢复前创建并轮转。

### 5.3 API

- `get_mirror_access_status`
- `validate_mirror_key`
- `enable_mirror_access`
- `repair_mirror_sessions`
- `restore_pre_mirror_state`

### 5.4 单页 UI

- 顶栏：mirror+、连接状态、刷新状态。
- 主区：Key、模式 segmented control、验证模型摘要、主按钮。
- 已接管态：当前模式、模型数、会话修复状态、启动 Codex、恢复原始 Codex。
- 诊断区：baseline 时间、最后操作、备份路径，不展示密钥或原文件内容。

## 6. 相对当前项目的取舍

### 复用

- Tauri 壳、launcher、Codex 路径检测。
- TOML 解析、原子写入、模型 catalog 模板。
- provider session sync 与备份。
- Windows/macOS 安装器基础。

### 产品隐藏

- 多供应商 profile 编辑器、aggregate、广告、推荐、Zed、脚本市场、context 管理、增强功能页。

### 暂不删除

MVP 先让新单页成为唯一入口，保留成熟旧代码以降低回归面。通过验收后再做物理删除和依赖瘦身。

## 7. 证据局限

- 未获得可用 mirror+ Key，因此只能确认 `/v1/models` 的认证边界，不能验证真实模型响应、字段和上下文元数据。
- `agent-reach` CLI 在当前 PowerShell 未注册；本次按其回退规则使用 GitHub CLI、官方 Codex Manual、Jina Reader 和直接 HTTP 探测。社区平台覆盖不足，不把低 stars 搜索结果解释为市场规模。
- Codex 内部 SQLite/rollout schema 会变化，当前结论只对本仓库已覆盖版本成立。
