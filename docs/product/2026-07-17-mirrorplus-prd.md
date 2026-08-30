# mirror x codex 可逆接入工具 PRD

> 版本：1.0
> 日期：2026-07-17
> 产品阶段：MVP 收敛重构
> 产品定义：为 Codex 提供镜子AI单服务、零配置、可验证、可完整恢复的 API 接入。

## 1. 产品结论

mirror x codex 不是通用供应商管理器，也不是 Codex 增强平台。用户只需要提供一个镜子AI API Key，选择接入模式，然后继续使用 Codex。工具必须保证任何接管动作都可追踪、失败自动回滚，并能一键恢复到首次使用前的状态。

一句话价值主张：

> 填入一个 Key，一键使用镜子AI全部模型；随时一键恢复原来的 Codex。

## 2. 问题定义

目标用户当前需要理解 `config.toml`、`auth.json`、provider、Responses API、模型目录和会话 provider bucket。手工接入存在五类高频风险：

1. 配置写错导致 Codex 无法启动或模型不可用。
2. 切换 provider 后历史会话在列表中消失。
3. API Key 无效时配置已被覆盖。
4. 第三方工具卸载或切回官方后无法还原原配置。
5. 工具提供过多无关能力，用户无法判断哪些开关是必需的。

## 3. 目标与非目标

### 3.1 产品目标

- 首次成功接管不超过 60 秒，用户输入项只有 API Key 和模式。
- 自动验证 Key，并从 mirror+ `/v1/models` 获取当前可用模型。
- 支持混合 API 和纯 API 两种明确模式。
- 首次接管前创建不可覆盖的 baseline。
- 接管后默认执行一次会话归属修复。
- 一键恢复 baseline，并把会话归属同步回原 provider。
- 所有写入均使用原子写入；任一步失败时自动恢复本次操作前状态。
- API Key 不回显、不进入日志、不进入诊断包。

### 3.2 非目标

- 不支持任意第三方供应商。
- 不提供供应商 CRUD、聚合轮转、价格比较或多 Key 管理。
- 不管理 MCP、skills、plugins、Zed、脚本市场或桌面美化。
- 不把 Chat Completions 作为默认长期协议；mirror+ 主链路使用 Responses API。
- 不承诺恢复 Codex 官方未公开、已加密或未来新增的内部状态。

## 4. 用户与核心场景

### 4.1 用户画像

- 已安装 Codex Desktop，但不熟悉 TOML/JSON。
- 已获得 mirror+ API Key，希望直接使用全部可用模型。
- 可能已有 ChatGPT 登录态、项目配置、MCP 与历史会话，不能被破坏。

### 4.2 核心用户旅程

#### 首次接管

1. 工具检测 Codex home、当前 provider 和配置健康度。
2. 用户输入 API Key。
3. 工具调用 `/v1/models` 验证 Key 并取得模型。
4. 用户选择混合 API 或纯 API。
5. 工具创建 baseline，显示将触碰的文件摘要。
6. 工具写入配置并进行读后校验。
7. 工具同步历史会话到 mirrorplus provider。
8. 工具显示“已接管”，用户可启动 Codex。

#### 一键恢复

1. 用户点击“恢复原始 Codex”。
2. 工具确认 baseline 完整且校验通过。
3. 恢复原 `config.toml`、`auth.json` 和 manager settings 的存在性与内容。
4. 将现有会话同步回 baseline 中记录的原 provider。
5. 删除 mirror+ 生成的模型 catalog，保留审计记录和 baseline。
6. 读后校验成功，状态变为“未接管”。

## 5. 模式定义

| 模式 | ChatGPT 登录态 | mirror+ Key 存储位置 | 使用场景 |
|---|---|---|---|
| 混合 API | 保留 | mirrorplus provider 配置 | 希望保留官方登录态，同时使用 mirror+ 模型 |
| 纯 API | 接管期间由 API Key auth 替代 | `auth.json` 或 Codex 支持的凭据层 | 只使用 mirror+，不依赖 ChatGPT 登录 |

两种模式都使用 `wire_api = "responses"`。Chat Completions 只作为未来兼容降级，不在 MVP 暴露。

## 6. 功能需求

### FR-01 环境检测

- 检测 Codex home、`config.toml`、`auth.json`、可写性和当前 provider。
- 检测 mirror+ 是否已接管，以及 baseline 是否存在、是否完整。
- 不读取或展示 API Key 明文。

### FR-02 Key 验证与模型发现

- 使用 `Authorization: Bearer <key>` 请求 `https://api.jingziai.club/v1/models`。
- 401/403 明确显示 Key 无效；超时与服务端错误分别提示。
- 只接受非空、去重后的模型 ID。
- 没有取得任何模型时禁止首次接管，避免生成不可用配置。
- 模型上下文元数据缺失时不得伪装为已知真实窗口；使用兼容默认值并标记来源。

### FR-03 baseline

- baseline 在首次写入前创建，后续接管不得覆盖。
- 记录每个文件当时是否存在、SHA-256、原 provider、创建时间和 schema version。
- baseline 至少覆盖 `config.toml`、`auth.json`、mirror+ manager settings。
- baseline 写入完成并重新读取校验后才允许接管。

### FR-04 原子接管

- 只修改 mirror+ 所需 root key 和 `[model_providers.mirrorplus]`。
- 保留项目、MCP、features、sandbox、approval 及其他未知配置。
- 生成模型 catalog 后再修改 `config.toml` 指针。
- 写入失败自动恢复操作前文件。
- 写后解析 TOML/JSON 并验证 provider、base URL、协议和默认模型。

### FR-05 会话修复

- 接管成功后默认运行，目标 provider 为 `mirrorplus`。
- 修改 rollout JSONL、SQLite 和 workspace roots 前创建备份。
- 文件被占用、加密内容或 schema 不兼容时给出明确降级状态。
- 会话修复失败不撤销已经可用的 API 配置，但整体状态标记为“已接管，需修复会话”。

### FR-06 恢复

- 恢复操作不依赖当前 Key 是否有效。
- 按 baseline 还原文件内容和“原本不存在”的状态。
- 恢复后把全部现有会话同步到 baseline provider，使接管期间新会话仍可见。
- 恢复失败时保留可重试状态，不覆盖 baseline。

### FR-07 UI

- 单页，无侧边栏和功能导航。
- 只显示状态、Key、模式、模型验证结果、启用/恢复/启动三个动作和最近一次操作结果。
- Key 默认遮蔽，离开输入框后不再显示原值。
- 高级诊断折叠显示，不提供配置编辑器。

## 7. 状态机

| 状态 | 含义 | 可用动作 |
|---|---|---|
| `unmanaged` | 未接管 | 验证并启用 |
| `validating` | 验证 Key/模型 | 取消 |
| `applying` | 建 baseline 和写配置 | 等待 |
| `active` | 接管成功且会话已修复 | 启动、切换模式、恢复 |
| `active_degraded` | API 可用但会话修复不完整 | 重试修复、恢复 |
| `restoring` | 正在恢复 | 等待 |
| `restore_failed` | 恢复未完整完成 | 重试恢复、查看诊断 |

## 8. 数据与安全

- API Key 只用于验证和本机 Codex 配置，不上传到其他服务。
- 日志只能记录 Key 是否存在和长度区间，不记录值、前缀或后缀。
- baseline 含敏感凭据，只允许当前用户访问；诊断导出必须排除其内容。
- 所有网络请求必须设置连接和总超时。
- 恢复与接管使用进程内互斥锁，拒绝并发操作。

## 9. 成功指标

- 有效 Key 首次接管成功率 >= 98%。
- 接管失败后原配置恢复率 = 100%。
- baseline 完整恢复测试通过率 = 100%。
- 用户从打开应用到启动 Codex 的交互不超过 4 次。
- 默认界面暴露的可编辑字段不超过 2 个。

## 10. 发布阻断条件

- 任一密钥泄漏测试失败。
- baseline 可被覆盖或恢复结果未做校验。
- 无效 Key 仍会修改 Codex 配置。
- 未保留未知 TOML 配置段。
- 会话修复没有备份或无法标记部分失败。
- Windows 安装包无法从旧 CodexPlusPlus/mirror+ 路径升级。
