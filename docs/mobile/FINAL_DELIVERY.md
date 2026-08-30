# Mirror X Codex 手机远程控制 — 当前交付报告

交付日期：2026-08-14  
版本：Windows v1.2.43  
状态：本机实现、构建和验证完成；未发布公网 Relay，未 commit/push

## 交付结论

v1.2.43 在 Desktop Sync 主路径上补齐了用户侧状态机：初始化回退与重试、最近会话自动打开、同文本重复发送、双击防重、按 thread 排队、精确停止、重连恢复和 Host 快速重启隔离。

因此，手机现在可以实时看到电脑 Codex 正在执行的任务，并在同一会话数据流中继续操作。它仍不是远程桌面画面镜像。

## 已实现

- `desktopSync` 主路径：Phone → Relay → Manager Host → Desktop dispatcher → Desktop-owned App Server。
- `standalone` 降级：Desktop/CDP 不可用时由 Manager 启动兼容 App Server。
- 当前 Desktop active turn、`turnId` 和实时 notification 同步到手机。
- 手机晚连接时，从历史恢复当前 active 状态。
- active turn 执行期间，手机消息排队；当前 turn 完成后自动发送。
- 停止请求使用 `threadId + turnId` 精确定位。
- Relay 整进程重启后，Host 恢复连接并继续同一 session RPC。
- Relay 与 Host 错开恢复期间，手机保留工作台、会话和输入状态。
- `HOST_OFFLINE` / `RATE_LIMITED` 指数退避和 Toast 节流。
- 项目、历史会话、Markdown、表格、代码块、链接、`[!image]` 占位渲染。
- 手机抽屉可打开和收回，竖屏和横屏布局均已覆盖。
- 房间隔离、错误 token、加密通道和 `CLIENT_REPLACED` 策略。
- Relay `/status` 与本地诊断日志不再暴露完整 room ID。

## 验证状态

| 范围 | 结果 |
|---|---|
| Mobile Host 测试 | 17/17 |
| Relay 测试 | 11/11 |
| Manager Windows subsystem | 24/24 |
| Desktop dispatcher 真实只读 live test | 通过 |
| Host + 本地 Relay + Desktop Sync live test | 通过 |
| Relay 整进程重启与同 session 恢复 | 通过 |
| Manager TypeScript check / Vite build | 通过 |
| 加密、隔离、错误 token、客户端替换 | 通过 |
| 消息排队、停止、自动发送 | 通过 |
| Markdown 与特殊占位格式 | 通过 |
| 390×844、412×915、844×390 | 通过 |

完整证据见 [QA-v1.2.43-user-logic-audit.md](QA-v1.2.43-user-logic-audit.md)。

## 本机交付物

```text
D:\mirrorplus\mirror-x-codex-manager.exe
```

- FileVersion：`1.2.43`
- ProductVersion：`1.2.43`
- SHA256：`C613C1766B5AE992A710EC008F7E4C3DE6F1C211DC916CB011A2467A9B26F39A`

回滚备份：

```text
D:\mirrorplus\backups\20260814-232937-mobile-logic-v1.2.43\
```

## 可真实演示的流程

1. Windows 电脑启动 Codex Desktop。
2. 启动 `D:\mirrorplus\mirror-x-codex-manager.exe`。
3. 在 Manager 开启手机控制，并让 Host 连接 Relay。
4. 手机扫码或打开配对页面，输入对应 Key。
5. 手机选择项目和会话。
6. 在电脑 Codex 发起任务，手机实时显示工作状态和增量输出。
7. 任务执行中从手机发送下一条指令，页面显示排队；当前任务完成后自动发送。
8. 临时重启测试 Relay，手机保留页面；连接恢复后继续同一 session。

本轮没有发布公网 Relay。上述流程用于本机/测试 Relay 验证，不能据此宣称公网页面已经更新。

服务器已经建立仅监听 `127.0.0.1:8767` 的 v1.2.43 独立 canary，并通过完整
Relay E2E 和 390×844 手机视口验证。它没有接入 Nginx；公开域名仍使用
`8765 / v1.2.39`，因此不影响当前客户。

## 可以对外承诺

在 Windows、电脑开机、Codex Desktop 与 Manager 正常运行、Relay 已部署匹配版本的前提下：

- 手机可查看项目和历史会话。
- 手机可实时查看 Codex Desktop 当前任务的协议级状态和输出。
- 手机可继续发送指令；繁忙时安全排队。
- 短时网络中断后自动恢复，并尽量保留当前页面和会话。
- 中继只转发端到端加密内容，不应读取用户会话正文。
- 不同 Key 对应隔离房间。

## 不能对外承诺

- 不能宣称是远程桌面、像素级画面镜像或鼠标键盘接管。
- 不能保证电脑关机、休眠、Manager 退出或 Codex Desktop bridge 不可达时仍可实时工作。
- 不能允许手机和 Desktop 对同一 active turn 同时并发启动第二个任务。
- 不能承诺一个房间多手机/多标签同时写入。
- 不能承诺任意网络环境永不断线；只能承诺已实现恢复和状态保留策略。
- 不能承诺 macOS 已稳定，本轮未构建和真机验证 macOS。
- 不能宣称公网 Relay 已升级，本轮明确未部署生产。
- 不能承诺丢失手机可在不换 Key 的情况下独立撤销；独立 pairing secret 尚未实现。

## 发布前剩余事项

1. 使用独立 canary Relay 验证公开域名、WSS、Nginx 路径和真实手机网络。
2. 做至少一台 Android 和一台 iPhone 真机验收。
3. 验证 canary 后，按连接排空、可回滚切换方式发布 Relay。
4. 再构建正式安装包并核对版本、签名策略、SHA256 和下载链路。
5. macOS 作为独立里程碑处理，不能沿用 Windows 结论。

## Windows 安装包

```text
D:\mirror++\CodexPlusPlus\dist\windows\mirror-x-codex-1.2.43-windows-x64-setup.exe
D:\mirror++\CodexPlusPlus\dist\windows\mirror-x-codex-1.2.43-windows-x64.zip
```

- Setup SHA256：`B5B0692454C7EB60452AD7E3DD878E2B9AF08EEFE2A79CA536B4756FCF7A502F`
- ZIP SHA256：`0103E898563EE889AB7F9FB68EFE057C6CC202353B0E9F34950E046C75AA4459`
- 已执行真实覆盖升级，退出码 `0`
- 注册表、快捷方式和三个 EXE 版本均验证通过
- 覆盖升级备份：`D:\mirrorplus\backups\20260814-remote-full-installer-v1.2.43\`
