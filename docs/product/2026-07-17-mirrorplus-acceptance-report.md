# mirror x codex MVP 验收报告

> 日期：2026-07-17
> 版本：1.2.32 MVP
> 结论：Windows 本地安装成品通过；macOS 源码与双架构 CI/DMG 链路通过结构测试，实际 DMG 需在 macOS GitHub Actions runner 生成。

## 1. 已交付能力

- 单页入口：API Key、混合/纯 API、验证、启用、启动、会话重试、恢复。
- 安装前体检：检测 Codex Desktop、`CODEX_HOME` 写入权限和现有 `config.toml`；未通过时前后端共同阻止接管。
- 未检测到 Codex 时提供 OpenAI 官方 Codex 入口，并可安装后重新检测；不绑定单一商店安装路径。
- 原有 `[mcp_servers.*]` 配置增量保留，状态页显示已保留的 MCP 数量。
- 启用时自动释放并注册内置插件市场快照；纯 API 仅开启插件市场专用注入，插件失败不阻断模型 API。
- 标准 Bearer `/v1/models` Key 验证和动态模型发现。
- Responses API 固定接入，默认模型优先 `gpt-5.5`、其次 `gpt-5.4`、最后服务端首个模型。
- 首次接管前不可覆盖 baseline，记录文件存在性和 SHA-256。
- 每次启用/恢复前 operation snapshot。
- 保留未知 TOML 段、MCP、features 等非 mirror+ 配置。
- 混合 API 保留 ChatGPT auth；纯 API 在接管期间写入 API Key auth。
- 配置写入失败自动恢复本次操作前状态。
- 接管后默认执行 provider session sync；失败进入 degraded，可重试。
- 恢复 baseline 后把接管期间的新会话同步回原 provider。
- 旧 `~/.codex-session-delete/settings.json` 自动迁移到 `~/.mirrorplus`。
- Windows 安装不要求管理员权限，桌面只提供一个 manager 入口。

## 2. 测试结果

| 范围 | 结果 |
|---|---|
| mirror access 单元测试 | 8/8 通过 |
| relay/config 集成测试 | 95/95 通过 |
| provider session sync | 18/18 通过 |
| manager 单元测试 | 29/29 通过（串行） |
| Windows/发布结构测试 | 22/22 通过 |
| TypeScript | 通过 |
| Vite production build | 通过 |
| `git diff --check` | 通过，仅有仓库既有 LF/CRLF 提示 |

全 workspace 聚合命令在本机 5 分钟窗口内未完成链接，因此使用各 crate 和高风险集成测试的明确结果验收。并发运行 manager 测试时曾出现一个环境变量测试竞态，单测及 manager 串行全量重跑均通过。

## 3. 视觉验收

- 1180×820：无横向/纵向溢出，核心动作在首屏。
- 390×844：无横向溢出，模式与按钮自动单列，文字没有重叠。
- 正式窗口调整为 880×700，最小 640×600，适配常见 1366×768 设备。
- 新版 release 窗口实测三项体检均完整显示；滚轮可滚动到底部操作区，未出现重叠或横向溢出。
- 新版截图：`output/playwright/mirror-x-codex-desktop.png`、`output/playwright/mirror-x-codex-mobile.png`。

## 4. Windows 安装成品

- 文件：`dist/windows/mirror-x-codex-1.2.32-windows-x64-setup.exe`
- 大小：15,500,789 bytes
- SHA-256：`8E189B42AAF7AC6F083049B9E1256D4BBC0265D33F9C23A75F08D0F718D4963D`

隔离烟测结果：

- 静默安装返回 0。
- 安装后的 launcher/manager SHA-256 与 release staging 完全一致。
- 安装后的 manager 进程成功启动。
- 静默卸载返回 0。
- 卸载后安装目录与 uninstall registry key 均不存在。

## 5. macOS 状态

- CI 矩阵覆盖 `x86_64-apple-darwin` 和 `aarch64-apple-darwin`。
- DMG 脚本生成 `mirror x codex.app`、`mirror x codex 管理器.app` 并检查 Info.plist、PkgInfo、可执行权限与 codesign。
- macOS CI 分别执行可逆接入和插件市场测试，并对 DMG 执行校验、只读挂载、Mach-O 架构、动态库、严格签名和 manager 启动烟测。
- Windows 无 Apple SDK、`hdiutil`、`codesign` 和 notarization 环境，无法在本机真实生成或打开 DMG。
- 发布前必须在 macOS runner 完成 DMG 构建，并配置正式 Developer ID 签名/公证；临时 ad-hoc 签名不应作为面向小白的最终公开包。

## 6. 尚未完成的外部验收

- 没有使用真实 mirror+ 普通 API Key，因此未验证生产 `/v1/models` 返回的具体模型字段和实际 Responses 推理。
- 未修改 mirror+ 服务器，也未把后台或 SSH 凭据写入任何产物。
- 当前默认自动更新源为空；公开发行前需要自有 GitHub repository/release URL 和签名发布流程。

以上三项不影响本地 Windows 安装包结构和客户端事务逻辑，但会阻断“可公开大规模分发”的最终发布结论。
