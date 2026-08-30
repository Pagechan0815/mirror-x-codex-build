# Mirror X Codex v1.2.45 手机远程候选版 QA 验收报告

- 测试日期：2026-08-15
- PWA 构建：`v20260815.7`
- Windows 应用版本：`1.2.45`
- 测试环境：Windows 本机、Chromium/Playwright、隔离 Relay `127.0.0.1:8816`
- 生产影响：无。未修改公网、未重启生产 Relay、未安装覆盖 `D:\mirrorplus`
- 候选程序：`D:\mirror++\CodexPlusPlus\target\release\mirror-x-codex-manager.exe`
- SHA256：`64EABB86D750401FF4366CF7A5FE9E236713FCF866CD33AE10743E21F6219003`

## 1. Product Goal Understanding

- Target users：购买镜子AI Key、主要使用 Windows、希望在外出时用手机继续操作自己电脑 Codex 的普通用户。
- Core scenario：电脑保持开机并运行 Mirror X Codex，手机扫码进入加密工作台，查看项目、历史会话、进行中的任务、发送新任务和附件。
- Primary user success outcome：不用 VPN、不理解 API 配置，也能从手机安全进入自己的电脑任务并持续工作。
- Business goal：把“中转模型接入”升级为可演示、可售卖、可恢复的手机远程 Codex 能力。
- Non-goals：不是像素级远程桌面；不是手机直接运行 Codex；视频当前作为文件交给 Codex，不承诺自动理解视频画面；当前不承诺一组 Key 多电脑或多手机并行控制。
- Quality risks：
  - API Key 当前同时派生远程配对凭据，Key 泄露会扩大为远程控制风险。
  - 手机任务默认 `dangerFullAccess + never`，这是已确认的产品规则，但必须向用户说明高权限边界。
  - 真实 Android、iPhone 和公网弱网仍需店主真机验收。
  - 25MB 文件会在手机内存中重组，低内存设备可能出现性能压力。
- Assumption：首发只以 Windows 为正式支持平台；Mac 暂不作为本轮发布门槛。

## 2. User Journey Map

| Journey Step | User Goal | System Response | Risk | Observable Signal | Priority Path |
| --- | --- | --- | --- | --- | --- |
| 打开电脑端 | 开启手机远程 | Host 连接 Relay，展示二维码 | Codex 未安装或 Host 启动失败 | Manager 状态和诊断日志 | Core |
| 手机扫码 | 进入自己的电脑 | URL Fragment 解出派生凭据，建立加密房间 | Key 错误、电脑离线 | 精确错误页，可重新配对 | Core |
| 初始化工作台 | 看到项目和会话 | 连接 Desktop App Server，加载历史与当前任务 | 长时间“正在读取” | 已连接、同步时间、加载骨架 | Core |
| 查看进行中任务 | 知道电脑正在做什么 | 展示执行中、运行时间、最近活动和停止按钮 | 手机与桌面状态漂移 | 顶栏与会话状态一致 | Core |
| 浏览项目文件 | 查看图片、Markdown、PDF、视频 | Host 分块读取，手机用 Blob 预览 | 大文件撑断 Relay、文件竞态覆盖 | 预览成功且连接不中断 | High |
| 发送任务 | 输入文本并附加文件 | 分块上传，调用 `turn/start` | 重复发送、断线、附件失败 | 上传进度、执行中、最终回复 | Core |
| 向上阅读 | 不被新输出抢到底部 | 保留阅读位置，显示“↓ 有新回复” | 完成刷新强制滚底 | 按钮出现，点击后才到底部 | High |
| 网络恢复 | 内容不丢、任务不断 | 保留会话 ID、自动重连和恢复 | 反复初始化、重复提交 | 已保存提示、恢复后同会话 | Core |
| 断开 | 立即退出远程 | 清除配对和会话信息，无确认弹窗 | 误点 | 一步断开，返回配对页 | Medium |

## 3. Normal Path Tests

| ID | Module | Journey Step | Test Type | Objective | Precondition | Steps | Expected Result | Priority | Risk Tag | Evidence | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| NP-001 | Relay | 手机扫码 | Normal | 验证 Host/Client 注册和双向加密 | 隔离 Relay | 运行 `mobile_e2e_check.py` | 双向密文可解，Relay 看不到明文 | P0 | Security | E2E 全部 PASS | Pass |
| NP-002 | Desktop Host | 初始化 | Normal | 连接真实 Codex Desktop App Server | 本机 Codex 可用 | 运行 ignored live test | initialize、thread/list 返回 | P0 | Logic | `mobile_relay_host_live` 1/1 | Pass |
| NP-003 | Desktop Sync | 会话恢复 | Normal | 同一手机会话恢复 | 已初始化 | 重发 `appServerConnect` | `resumed=true`，不重复初始化 | P0 | Data | live test 日志 | Pass |
| NP-004 | Relay Recovery | 断线恢复 | Normal | Relay 重启后任务仍可访问 | Host 保持运行 | 停止并重启 Relay | 同一 App Server 恢复并响应 RPC | P0 | Compatibility | live test 日志 | Pass |
| NP-005 | History | 历史会话 | Normal | 分页加载历史和 turns | Mock Host 在线 | 进入工作台 | 自动打开最近/活动会话 | P1 | UX | `v20260815.7-mobile-390x844-main.png` | Pass |
| NP-006 | Activity | 长任务状态 | Normal | 展示执行中和最近活动 | 活动线程 | 打开工作台 | 顶栏、会话、停止按钮一致 | P1 | UX | 主视口截图 | Pass |
| NP-007 | Attachment | 文件上传 | Normal | 上传 MP4 和 Markdown | 空闲会话 | 选择两个附件并发送 | MP4=`video/mp4`，README=`text/markdown` | P0 | Data | Mock events | Pass |
| NP-008 | Permission | 任务权限 | Normal | 验证默认完全访问 | 空闲会话 | 发送任务 | `approvalPolicy=never`、`dangerFullAccess` | P0 | Security | Mock `turn/start` 事件 | Pass |
| NP-009 | Markdown | 回复渲染 | Normal | 表格、任务项、引用、代码等可读 | Mock 回复 | 发送任务 | 结构化渲染、代码可复制 | P1 | UX | `v20260815.7-mobile-attachment-complete.png` | Pass |
| NP-010 | File Viewer | Markdown 文件 | Normal | 默认渲染并可看源码 | 项目有 Markdown | 打开 `PREVIEW.md` | 标题、列表、代码块正常 | P1 | UX | `v20260815.7-mobile-markdown-preview.png` | Pass |
| NP-011 | File Viewer | 大图预览 | Normal | 验证超过 2MB 的文件 | 2,300,070 bytes PNG | 打开文件 | 9 分块传输、预览成功、连接保持 | P0 | Performance | 大图截图和 Mock events | Pass |
| NP-012 | Windows Build | 候选构建 | Normal | 生成可运行 release EXE | Node/Rust/Tauri 可用 | `npm run build` | 生成 1.2.45 release EXE | P0 | Compatibility | release artifact + SHA256 | Pass |

## 4. Abnormal Path Tests

| ID | Module | Objective | Precondition | Steps | Expected Result | Priority | Risk Tag | Evidence | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| AP-001 | Relay | 电脑离线时拒绝手机 | 无 Host | Client 连接 | 返回 `HOST_OFFLINE` | P0 | UX | E2E | Pass |
| AP-002 | Relay | 错误 Token 不串房间 | Host 在线 | 使用错误 Token | 返回 `TOKEN_MISMATCH` | P0 | Security | E2E | Pass |
| AP-003 | Crypto | 外部 Key 不能解密 | 不同派生 Key | 解密密文 | 解密失败 | P0 | Security | E2E + crypto vectors | Pass |
| AP-004 | Client | 旧 Host 能力不足 | 无 `fileDownloadChunks` | 打开文件 | 明确提示更新电脑端 | P1 | Compatibility | `mobile_reconnect_check.mjs` | Pass |
| AP-005 | Download | 文件不存在 | 有效会话 | 请求不存在路径 | 返回文件读取失败，不中断房间 | P1 | UX | Rust error path | Pass |
| AP-006 | Download | 分块乱序 | 已收到 start | 先发 index=1 | 终止并提示“文件分块顺序错误” | P0 | Data | Node regression | Pass |
| AP-007 | Upload | 附件传输失败 | 传输中断 | Relay/Host 断开 | 附件标记失败，可重新选择 | P1 | Data | 客户端状态机 | Pass |
| AP-008 | Thread | 活动任务禁止并发写入 | 桌面任务执行中 | 尝试手机发送 | 输入锁定或走 fork/恢复逻辑 | P0 | Data | 活动线程截图 +既有回归 | Pass |
| AP-009 | Bootstrap | 初始化失败可恢复 | Host 暂不可用 | 进入页面 | 展示明确错误、保留手动重试 | P0 | UX | 既有 Mock 失败路径 | Pass |

## 5. Boundary Condition Tests

| ID | Boundary | Expected Result | Evidence | Status |
| --- | --- | --- | --- | --- |
| BC-001 | 空文件上传 | 不添加并提示空文件 | PWA 逻辑 | Pass |
| BC-002 | 单附件 25MB+1 | 拒绝添加 | PWA/Host 常量与测试 | Pass |
| BC-003 | 附件数量 5+1 | 第 6 个拒绝 | PWA 逻辑 | Pass |
| BC-004 | 附件总量 50MB+1 | 停止继续添加 | PWA 逻辑 | Pass |
| BC-005 | 下载超过客户端上限 1 byte | 发送任何分块前拒绝 | Rust 23/23 | Pass |
| BC-006 | 下载 2.3MB | 9 个小于 2MB 的 Relay 帧 | Mock event + Rust 测试 | Pass |
| BC-007 | 空文本文件 | 允许读取，不制造 Base64 错误 | 新分块协议 | Pass |
| BC-008 | 320×568 | 无横向溢出，输入区可见 | 320 截图，scrollWidth=320 | Pass |
| BC-009 | 430×932 | 48×48 附件按钮，输入区完整 | 430 截图和 DOM 几何 | Pass |
| BC-010 | 844×390 横屏 | 无强制竖屏，无横向溢出 | 横屏截图，scrollWidth=844 | Pass |

## 6. Destructive/Malicious Tests

仅在隔离环境执行，未对生产服务发起破坏性请求。

| ID | Scenario | Expected Protection | Evidence | Status |
| --- | --- | --- | --- | --- |
| DP-001 | 非 32 位小写派生凭据 | 注册拒绝 | Relay 11/11 | Pass |
| DP-002 | 跨 room 访问 | 不可看到或解密其他 room | E2E | Pass |
| DP-003 | 同 room 第二手机接管 | 旧手机收到 `CLIENT_REPLACED` 并停止重连 | E2E | Pass |
| DP-004 | 超过 2MB 单帧 | Relay 拒绝；正常文件协议必须分块规避 | Relay 单测 + Host 单测 | Pass |
| DP-005 | 上传分块乱序 | Host 拒绝，不写出错误完成文件 | Rust 23/23 | Pass |
| DP-006 | 路径/文件名注入 | 上传文件名剥离路径和 Windows 保留字符 | Rust 23/23 | Pass |
| DP-007 | API Key 泄露后的远程控制 | 当前架构无法独立撤销设备权限 | 架构审查 | Not Run / Open Risk |

## 7. Novice User Tests

| ID | Objective | Steps | Expected Result | Evidence | Status |
| --- | --- | --- | --- | --- | --- |
| NU-001 | 扫码即用 | 电脑开启后扫码 | 不手输 room/token | PWA 配对流程 | Pass |
| NU-002 | 看懂是否在线 | 打开工作台 | 顶栏显示已连接/执行中 | 手机截图 | Pass |
| NU-003 | 找到历史和文件 | 点三横菜单 | 三个入口清晰，选择后抽屉收回 | Playwright 真实点击 | Pass |
| NU-004 | 添加附件 | 点回形针 | 显示图片、视频、文件选择 | 附件截图/事件 | Pass |
| NU-005 | 理解视频边界 | 选择 MP4 | 显示“视频”“将按文件上传” | 浏览器快照 | Pass |
| NU-006 | 错误恢复 | 电脑离线或版本旧 | 使用普通中文提示下一步 | E2E/Node | Pass |

首次关键任务在本地测试中可在一次尝试内完成。真实小白用户的完成时间仍需店主真机观察，目标为 2 分钟内完成“扫码、打开会话、发送一句话”。

## 8. Mobile Experience Tests

| ID | Device/State | Expected Result | Evidence | Status |
| --- | --- | --- | --- | --- |
| MB-001 | 320×568 竖屏 | 无溢出，核心控件可见 | `v20260815.7-mobile-320x568.png` | Pass |
| MB-002 | 390×844 竖屏 | 主会话可读，输入区固定 | `v20260815.7-mobile-390x844-main.png` | Pass |
| MB-003 | 430×932 竖屏 | 触控目标和输入宽度正常 | `v20260815.7-mobile-430x932.png` | Pass |
| MB-004 | 844×390 横屏 | 不锁竖屏、内容可操作 | `v20260815.7-mobile-844x390-landscape.png` | Pass |
| MB-005 | 390×480 输入聚焦 | 输入框底部 469px，未被裁掉 | `v20260815.7-mobile-390x480-input-focus.png` | Pass |
| MB-006 | 抽屉菜单 | 顶栏、关闭按钮、选项后自动收回 | Playwright 点击记录 | Pass |
| MB-007 | 向上阅读 | 完成后 `scrollTop=0` 保持不变 | `v20260815.7-mobile-scroll-retained.png` | Pass |
| MB-008 | 新回复跳转 | 显示“↓ 有新回复”，点击后 distance=0 | DOM 量测 | Pass |
| MB-009 | Markdown 长内容 | 表格横向可读，代码可复制 | 完成截图 | Pass |
| MB-010 | 大文件预览 | 连接不中断、文件查看器可关闭 | 大图截图 | Pass |
| MB-011 | 真实 Android/iPhone | 软键盘、浏览器恢复、弱网 | 店主真机 | Blocked |

## 9. Bug Severity Grading

| Bug ID | Title | Severity | Priority | Reproducibility | Scope | Root Cause Type | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| MXM-045-01 | 大文件预览撞 Relay 2MB 单帧上限 | S2 | P0 | Always | Segment | Architecture | Fixed |
| MXM-045-02 | 旧 `fs/readFile` Base64 导致内存膨胀 | S2 | P1 | Always | Segment | Frontend/Protocol | Fixed |
| MXM-045-03 | 快速切换文件时慢请求覆盖新预览 | S2 | P1 | Intermittent | Single user | Frontend | Fixed |
| MXM-045-04 | Relay 测试仍断言旧读取接口 | S3 | P1 | Always | CI | Test | Fixed |
| MXM-RISK-01 | API Key 同时作为远程配对根凭据 | S2 Risk | P1 | Conditional | Global | Architecture | Open |
| MXM-RISK-02 | 25MB 文件在手机端重组时有内存峰值 | S2 Risk | P2 | Device dependent | Segment | Performance | Open |
| MXM-RISK-03 | 通知仅页面标题和 Toast，无系统 Push | S3 | P2 | Always | Segment | Product scope | Accepted |

## 10. Regression Rules

Smoke：

- `node --check app.js/relay.js`
- `mobile_pwa_format_check.mjs`
- `mobile_reconnect_check.mjs`
- Python Mock Host 编译
- Mobile Relay 11/11
- Mobile Host 23/23

Critical business flow：

- 手机扫码 -> Desktop Sync -> 历史会话 -> 发送任务 -> 完成回复。
- MP4/Markdown 上传 -> 本机路径 -> `turn/start`。
- 2MB 以上图片预览 -> 多分块 -> Blob -> 不中断连接。

High-risk history cases：

- Relay 重启后同一 App Server 恢复。
- 初始化失败不无限闪断。
- 活动桌面会话不被手机并发写坏。
- 用户上滑后完成刷新不抢到底部。
- 左侧抽屉可以打开和收回。

Cross-platform sanity：

- Windows Manager 24/24。
- 320、390、430 竖屏和 844×390 横屏。
- Mac 不纳入本轮发布门槛。

任何修改以下模块都必须重跑全部 Smoke 与对应 Critical 流程：

- `mobile_relay_host.rs`
- `pwa/relay.js`
- `pwa/app.js`
- Relay 单帧限制或 Nginx WebSocket 配置
- Desktop Sync/CDP 生命周期

## 11. Acceptance Criteria

- Open S0：0。
- Open S1：0。
- Core path：本机隔离环境 100% 通过。
- Must-run regression：全部通过。
- 证据：测试日志、Mock events、9 张当前构建截图、release EXE 和 SHA256 已保留。
- 数据完整性：上传分块、下载分块、断线恢复和重复会话均有自动化覆盖。
- 本机候选版信心分：`94/100`。
- 公网商业发布信心分：`80/100`，低于正式 Accept 门槛，原因是真机和公网 canary 尚未完成。

## 12. Final Acceptance Report

### Test Execution Summary

- 自动化核心组：
  - Mobile Relay：11/11。
  - Mobile Host：23/23。
  - Manager Windows subsystem：24/24。
  - Desktop Host live：1/1。
  - Mobile E2E：全部通过。
  - TypeScript、Vite、Tauri release build：通过。
- 浏览器手工自动化：大文件、Markdown、附件、权限、滚动、抽屉和多尺寸通过。
- 真实手机：未执行，由店主验证。

### Acceptance Decision

- Windows 本机候选版：`Accept`。
- 公网商业发布：`Conditional Accept`。
- 不允许宣称：
  - 已在所有 Android/iPhone 浏览器验证。
  - 手机是桌面画面镜像。
  - 视频内容可被模型直接观看理解。
  - 一组 Key 支持多电脑、多手机同时在线。
  - API Key 泄露后远程控制仍安全。

### Immediate fix

1. 已完成分块下载、Blob 预览、旧 Host 能力提示和文件预览竞态保护。
2. 店主使用真实 Android 和 iPhone 完成真机清单。
3. 在独立公网 canary 端口发布 `v20260815.7`，验证后才允许切生产。

### Short-term improvement

1. 为大文件增加可见下载进度和取消按钮。
2. 把页面标题/Toast 通知升级为可选系统通知或 PWA Push。
3. 增加设备列表、最近登录和一键撤销配对。
4. 为弱网补分块重试或断点续传。

### Structural optimization

1. 将“模型 API Key”和“设备配对凭据”彻底分离，支持独立过期、撤销和设备级授权。
2. 将单活动手机策略升级为账号、设备和房间三层对象模型。
3. 对完全访问增加一次性产品说明和可审计操作记录，但不改变已确认的默认 `never + dangerFullAccess`。
