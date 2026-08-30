# Mirror X Codex

Mirror X Codex 是镜子AI提供的 Codex Desktop 接入工具。用户在本机填写镜子AI Key、验证每个 Key 有权限使用的模型，即可在官方 Codex 中按模型自动走对应分组。

它不是聊天客户端，不替代官方 Codex；它负责安全、可恢复地完成模型接入。

## 用户下载

从 [最新发布页](https://github.com/Pagechan0815/mirror-x-codex-build/releases/latest) 下载与系统匹配的安装包：

| 系统 | 文件 |
| --- | --- |
| Windows 10/11 x64 | `mirror-x-codex-*-windows-x64-setup.exe` |
| Intel Mac | `mirror-x-codex-*-macos-x64.dmg` |
| Apple Silicon Mac（M1/M2/M3/M4） | `mirror-x-codex-*-macos-arm64.dmg` |

完整的零基础教程见：[Mirror X Codex 用户安装与使用指南](docs/Mirror-X-Codex-用户安装与使用指南.md)。

## 产品行为

- 安装前自动检查官方 Codex 是否可用；未安装时提供官方安装入口。
- 支持 CodexPro（GPT / Grok）与企业专线分组 Key；每个模型只会使用所属分组的 Key。
- 支持混合 API：保留官方登录，并让已选模型通过镜子AI请求。
- 支持纯 API：全部模型请求通过镜子AI。
- 不主动删除 MCP、插件市场或未知 Codex 配置。
- 首次接入时创建本机基线；可点击“恢复使用前状态”回到接入前。
- 支持 Codex 的上下文压缩请求转发，包括 `/v1/responses/compact`。
- 支持 macOS Intel 与 Apple Silicon 双架构安装包；额度页显示本地 token/context 用量，服务器余额以中转站后台为准。

## 使用边界

不要通过聊天、截图、工单或公开仓库提交 API Key。用户只应在自己的 Mirror X Codex 本机界面中填写 Key。

如果模型不可用、额度耗尽或中转线路返回错误，请在 Mirror X Codex 中重新验证对应分组的 Key，或切换到该 Key 已授权的模型。

## 开发与发布

项目使用 Rust、Tauri 和 React 构建。发布前必须完成 Windows x64、macOS Intel、macOS Apple Silicon 三端构建与安装包验证；发布附件由 GitHub Actions 生成。
