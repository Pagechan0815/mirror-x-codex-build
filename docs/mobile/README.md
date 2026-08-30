# Mirror X Codex 手机远程控制

本文档目录记录手机远程控制从需求、调研、架构到 Windows v1.2.45 实现与验证的完整结果。

## 当前结论

截至 2026-08-17，Windows v1.2.45 已完整安装到本机 `D:\mirrorplus`。独立公网 canary 已更新到 PWA `v20260817.1`，并通过自动化、真实已安装 Manager Host 和公网移动浏览器视口验收；正式 `/relay` 尚未切换。

主路径为：

```text
手机 PWA
  → 加密 WebSocket
Relay（只转发密文）
  → Manager Host
Codex Desktop dispatcher
  → Desktop 持有的同一个 Codex App Server
```

这意味着手机可以读取桌面会话，并实时接收电脑 Codex 正在执行任务的事件和输出。手机发出的消息也进入同一个 Desktop App Server 数据流。

它不是桌面画面的像素级镜像。电脑任务执行期间，手机“发送”默认调用 `turn/steer`，把新要求追加到同一个 active turn；不会默认排队成下一轮，也不会发起第二个并发 `turn/start`。

当 Codex Desktop 或 CDP bridge 不可用时，Manager 可进入 `standalone` 兼容模式；此时可以继续访问本机 Codex 数据和执行能力，但不具备桌面 active turn 的实时同步语义。

## 文档清单

| 文档 | 用途 | 当前性 |
|---|---|---|
| [PRD](../product/2026-08-11-mobile-prd.md) | 用户、场景、范围和验收目标 | 需求基线 |
| [Research](Research.md) | Codex App Server、PWA、中继与竞品调研 | 调研基线 |
| [Architecture](Architecture.md) | 架构、安全、状态机及 v1.2.42 主路径勘误 | 当前架构基线 |
| [ObjectModel](ObjectModel.md) | 跨端对象和协议模型 | 设计基线，部分命名早于实现 |
| [SCAFFOLD](SCAFFOLD.md) | 早期脚手架与实施指南 | 历史参考，不代表当前完成度 |
| [QA v1.2.41](QA-v1.2.41.md) | 上一版本稳定性记录 | 历史记录 |
| [QA v1.2.42 Desktop Sync](QA-v1.2.42-desktop-sync.md) | Desktop Sync 首次实现证据 | 历史记录 |
| [QA v1.2.43 用户逻辑审计](QA-v1.2.43-user-logic-audit.md) | 用户旅程、漏洞修复、测试、安装与边界 | 历史记录 |
| [QA v1.2.45 候选版](QA-v1.2.45-mobile-release-candidate.md) | 文件预览、附件、移动交互和完整测试矩阵 | 当前功能验收 |
| [QA v1.2.45 公网 Canary](QA-v1.2.45-public-canary-20260816.md) | 本机安装、公网 canary、真实 Host E2E、回滚和发布边界 | 当前发布依据 |
| [QA v1.2.45 默认引导](QA-v1.2.45-steer-default-20260816.md) | 执行中 `turn/steer`、竞态、兼容和公网验证 | 当前发送行为依据 |
| [QA v1.2.45 任务时间线](QA-v1.2.45-timeline-sync-20260817.md) | 同步性能、过程/结论分层、流式节流与手机视口验证 | 当前呈现行为依据 |
| [FINAL_DELIVERY](FINAL_DELIVERY.md) | v1.2.43 阶段性交付摘要 | 历史记录 |

## 当前能力

- 手机查看项目和历史会话，不要求先在手机端打开过该会话。
- 手机实时看到 Desktop 当前任务的开始、增量输出和完成状态。
- 手机在任务执行期间提交新指令，默认引导当前 turn，并使用 `expectedTurnId` 防止误导到已经切换的任务。
- 手机停止请求携带 `threadId + turnId`，只针对当前任务。
- Relay 重启、Host 晚恢复、临时离线和限流期间保留当前页面与会话内容。
- 恢复后自动继续同一 session，并显示当前连接模式。
- Markdown、表格、代码块、链接和 `[!image]` 占位格式兼容。
- 移动端抽屉支持打开和收回，覆盖竖屏与横屏视口。
- 不同 Key 派生不同房间；错误 Key、房间隔离和客户端替换策略已有自动化覆盖。

## 当前限制

- Windows 优先；本轮没有构建或验证 macOS。
- 服务器已建立独立公网路径 `/relay-canary-v1245/`，运行 v1.2.45；公开生产 `/relay` 仍指向 `8765 / v1.2.39`，在线用户未被切换。
- 生产 v1.2.39 的 `/status` 仍返回未遮罩 room 标识；v1.2.45 canary 已修复。正式切换后必须复核状态脱敏。
- Android、iPhone 的扫码、发消息、附件、切网、锁屏和软键盘仍需店主真机完成，因此暂不能宣称所有手机均已验证。
- 手机浏览器会持久保存配对凭证；丢失手机时目前需在该手机断开或更换 API Key，独立撤销机制尚未实现。
- 不是远程桌面，不同步鼠标、窗口和像素画面。
- 单个配对房间保持单活动手机/标签页策略。
- 当前任务处于 `review` 或手动 `compact` 阶段时，Codex 不允许同 turn 引导；手机会明确提示等待阶段结束，不会伪装成已发送。
- 电脑必须开机，Manager 和 Codex Desktop 必须处于可用状态，才能获得 Desktop 实时同步。

## 本机测试入口

安装版：

```text
D:\mirrorplus\mirror-x-codex-manager.exe
```

当前本机已连接独立公网 canary，Manager 中可直接扫描二维码测试。

公网 canary 手机页面已更新到 `v20260817.1`。执行中再次发送默认引导当前任务；过程、思考摘要、命令和子 Agent 统一显示在执行过程面板，完成后自动折叠并单独显示最终结论。已经打开旧页面的手机需要刷新一次。

详细步骤和证据见 [QA-v1.2.45-public-canary-20260816.md](QA-v1.2.45-public-canary-20260816.md)、[QA-v1.2.45-steer-default-20260816.md](QA-v1.2.45-steer-default-20260816.md) 和 [QA-v1.2.45-timeline-sync-20260817.md](QA-v1.2.45-timeline-sync-20260817.md)。
