# Codex Guide 与工具索引

本页是给普通用户和中转站维护者的入口。用户不需要打开终端或修改 Codex 配置文件。

## 用户 Guide

- [安装与使用指南](Mirror-X-Codex-用户安装与使用指南.md)：安装、Key 验证、混合 API/纯 API、模型选择、历史会话和故障处理。
- [英文 README](../README_EN.md)：给非中文用户的下载和功能说明。

## 内置能力

| 能力 | 入口 | 说明 |
| --- | --- | --- |
| 文本模型 | Mirror X Codex 管理器 | 按 Key 实际验证结果显示模型；CodexPro 与企业专线分开路由 |
| 额度/用量 | Codex 会话详情 | 显示本地会话 token usage、上下文使用量和上下文上限；不会伪造服务器余额 |
| 生图 | `$jingzi-imagegen` | 独立 Skill，使用单独 Image Key，不写入 Codex provider |
| MCP / Skills / Plugins | “工具与插件”页 | 与供应商切换解耦；远程插件仍需各自 OAuth/授权 |
| 手机 Relay | `apps/codex-plus-mobile-relay` | 配对后提供移动控制；生产部署使用 `Dockerfile` 和 `deploy/` |
| 微信助手 PoC | `tools/codex-wechat` | 最小 iLink -> Codex app-server 桥接，默认白名单和前缀保护 |

## 额度说明

“额度”分两类：

1. **本地用量**：从 Codex rollout/session 数据读取每轮 input/output/cache/total tokens，并按模型显示 context limit。大文件只读尾部，原始会话不会被改写。
2. **中转站余额/套餐额度**：由中转站服务端计费系统决定，客户端不能可靠推断，也不会把 token 用量冒充余额。余额不足、Key 失效或限流时，重新验证对应分组 Key 或联系中转站管理员。

## 发布与验证

- Windows x64 当前对普通用户开放。macOS Intel 与 Apple Silicon 的构建和验收脚本已经具备，但 Mirror X 自己的 macOS runner 尚未完成一次真实成功验收，暂不开放下载。
- 上游 CodexPlusPlus `v1.2.56` 的 arm64/x64 DMG 已在真实 GitHub macOS runner 完成构建、bundle 检查和管理器启动测试，可证明参考链路有效；这不能替代 Mirror X 自己的验收。
- Mirror X Mac 正式公开前必须看到两个 macOS job 获得真实 runner 并全部通过，同时完成 Developer ID 签名、公证和干净设备烟测。
- 发布前运行 `cargo test --workspace`、`npm run check`、`npm run vite:build`，并核对安装包内三枚二进制的 SHA-256。
