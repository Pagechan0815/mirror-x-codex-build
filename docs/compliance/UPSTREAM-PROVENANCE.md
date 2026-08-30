# Mirror X Codex 来源与发布边界

## 当前事实

- 本仓库是基于 CodexPlusPlus 的独立 fork，公开构建与发布仓库为 `Pagechan0815/mirror-x-codex-build`。
- 当前工作树包含 Mirror X 自有的接入、会话同步、移动中继和白标改造。
- 不应把本产品描述为 OpenAI、CodexPlusPlus 原作者或其他上游的官方产品。
- 旧的目录名、配置键和协议字段仅用于迁移兼容，不代表产品名称。

## 许可证状态

上游 CodexPlusPlus 使用 `AGPL-3.0`。本公开快照同样使用 `AGPL-3.0`，仓库根目录附带完整 `LICENSE` 文本，Cargo metadata 也与之一致。

不得在官网、安装器或销售页面承诺“完全 MIT”“与上游无关”或“官方授权”。

## 发布边界

- Windows 可作为当前主要发布目标，但仍需通过安装、启用、真实 Responses 探测、会话同步、恢复和卸载回归。
- macOS workflow 使用 GitHub-hosted arm64 与 Intel runner 构建并执行 DMG 挂载、架构、签名和启动校验；没有用户真机和 notarization 证据时，必须标注为 macOS 测试版。
- GitHub Actions 的构建成功不等于 GitHub Release 成功；Billing、签名、公证和发布权限属于外部发布条件。
