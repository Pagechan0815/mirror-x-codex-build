# Mirror X Codex v1.2.49 发布说明

发布日期：2026-08-26

## 本次结论

`v1.2.49` 是在本地 `v1.2.48 CURRENT` 上选择性吸收 CodexPlusPlus 最新 `main@f514597` 高价值修复后的版本。没有整仓覆盖，没有恢复复杂高级设置，也没有修改真实用户 `.codex`、登录态或历史会话。

## 用户可感知优化

1. Pure API 不再固定假设 Provider 名为 `custom`。具名 Provider、新建会话、恢复会话和模型选择使用当前 profile 的真实配置，避免 `Model provider custom not found`。
2. 内置插件市场正式改为 `mirror-x-curated`，并兼容迁移旧 `openai-curated-remote`，避免 Codex 忽略保留名前缀。
3. Codex 更新导致 App asset 找不到时，失败扫描有冷却、次数上限和并发去重，避免低配置设备空闲 CPU 持续升高。
4. 快速重启遇到固定 helper 端口短暂未释放时，最多等待 6 秒恢复；不会无限等待，也不会杀未知进程。
5. 错选 Codex 路径不会永久落库；启动前仍会二次校验并回退自动探测。
6. 维护页读取大日志时只读取尾部最多 2MB，避免整文件进入内存。
7. 新增“整理会话列表”：严格预览、确认、快照校验、备份后原子应用；Codex 运行中拒绝整理，未知/损坏行保留。
8. 历史 `custom_tool_call` 的 `fc_`/`item_` ID 会按 Codex 新协议归一化为 `ctc_`，`call_id` 保持原样。
9. 修复 Windows 反复运行测试时临时目录复用造成的偶发假失败。

## 保持不变的产品边界

- SimpleApp 仍是普通用户默认入口，高级设置继续隐藏。
- 启动链继续单实例、优先激活已有窗口、禁止强杀 Codex/launcher。
- Image 仍是独立 Skill + Key 注册，不写入 Codex `model_provider`。
- 保留镜子AI Pure/Mixed 接入、企业专线选择、历史会话保护和手机 Relay。

## 验证结果

- `cargo test --workspace`：1038 passed，0 failed，2 ignored live tests。
- `npm test`：35 passed，0 failed。
- `cargo fmt --all -- --check`、`cargo check --workspace`、`npm run check`、`npm run vite:build`：通过。
- Windows 静态 release build：通过，三枚 EXE 文件版本均为 `1.2.49`。
- NSIS 安装包：构建通过；解包后三枚 EXE 与 release 源文件 SHA-256 完全一致。
- 手机 PWA 格式、加密 fallback、重连检查：通过。

## 发布门禁

公开大规模分发前，仍须在无本项目源码和开发缓存的干净 Windows x64 设备，用测试 Key 完成安装、Pure API、Mixed API、新会话、历史会话、模型选择、插件、Image Skill、双击启动、覆盖升级和卸载恢复。两个 live tests 依赖真实 Codex Desktop/Relay，不能用本机单元测试替代。

