# mirror x codex 产品化全量任务拆解

## P0：可逆接管内核

- [ ] 新增 mirror access domain model 与状态查询。
- [ ] Key 验证和 `/models` 解析，覆盖 401、空列表、重复模型、超时。
- [ ] baseline manifest、存在性记录和 SHA-256 校验。
- [ ] 混合/纯 API 的配置生成与原子写入。
- [ ] 写后校验与失败自动回滚。
- [ ] 接入 provider session sync，支持 degraded。
- [ ] baseline 恢复与恢复后 session 归一化。
- [ ] 并发互斥和密钥脱敏测试。

## P0：单页产品

- [ ] 新建 SimpleApp，替换默认入口。
- [ ] 未接管、验证中、已接管、降级、恢复失败五类状态。
- [ ] Key 输入、模式 segmented control、验证与启用。
- [ ] 启动 Codex、重试会话修复、一键恢复。
- [ ] 响应式桌面布局和键盘可访问性。

## P1：兼容与迁移

- [ ] 旧 `~/.codex-session-delete/settings.json` 到 `~/.mirrorplus` 的一次性迁移。
- [ ] 旧 CodexPlusPlus/mirror+ 配置识别，不覆盖已有 baseline。
- [ ] Windows/macOS 升级路径与卸载前恢复提示。
- [ ] 自有更新源和签名发布链路。

## P1：服务端依赖

- [ ] `/v1/models` 稳定返回可用于 Codex 的模型 ID。
- [ ] 增加模型上下文窗口、显示名和能力元数据。
- [ ] 提供健康检查与版本字段。
- [ ] 明确 Key 撤销、额度耗尽和限流错误码。

## 验证矩阵

- [ ] config/auth 原本存在与不存在的 4 种组合。
- [ ] ChatGPT auth、API auth、keyring 三类初始状态。
- [ ] 混合 -> 纯 API -> 混合 -> 恢复。
- [ ] 无效 Key、401、403、429、500、超时、非 JSON、空模型。
- [ ] 写 config/auth/catalog 任一步失败。
- [ ] baseline 被篡改、缺文件、hash 不匹配。
- [ ] SQLite 锁、rollout 锁、加密内容、无 Codex home。
- [ ] 接管期间创建新会话后恢复。
- [ ] Windows 10/11 与 macOS Intel/Apple Silicon 安装构建。
