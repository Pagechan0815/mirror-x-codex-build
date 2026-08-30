## Mirror X Codex 手机端控制功能 PRD

**版本**: v0.1  
**日期**: 2026-08-11  
**状态**: 待评审  
**优先级**: Windows 优先，macOS 暂缓

---

## 1. 背景与目标

Mirror X Codex 已让客户在 Windows 电脑上以中转 API key 驱动官方 Codex Desktop，无需翻墙。  
当前痛点：用户离开电脑后无法继续操作，查不到项目列表，也无法发指令或审批工具调用。

目标：提供一个**手机端入口**，让中转站任何买了 key 的用户，在外出时通过手机浏览器完整操控自己电脑上运行的 Codex，功能对齐桌面端，不需要手机翻墙，不需要额外账号。

---

## 2. 用户与场景

### 2.1 典型用户

| 属性 | 描述 |
|------|------|
| 技术水平 | 零基础，中专生，会扫码、会填 key |
| 设备 | 安卓/iOS 标准浏览器，电脑运行 Windows + Mirror X Codex |
| 网络 | 手机 4G/5G（国内，不开 VPN），电脑可开 VPN |
| 使用场景 | 外出时想看 Codex 跑到哪了，发一条指令，批准一个文件操作 |

### 2.2 前置条件

- 用户电脑必须**开机且运行 Mirror X Codex**（Manager 常驻系统托盘）
- 用户拥有镜子 AI 中转站有效 API key（`sk-xxx` 格式）
- 中转服务器 `193.112.101.159` 上已部署中继服务（管理员一次性操作）

---

## 3. 功能范围（MVP）

### 3.1 必须有（P0）

| 功能 | 说明 |
|------|------|
| 配对入口 | Manager 托盘/界面一键生成"手机连接"二维码或链接 |
| 项目列表 | 手机端展示所有会话，按 cwd 分组为项目，含最后预览和时间 |
| 完整会话历史 | 打开任意会话，加载完整 turns，展示用户/Codex/工具调用消息 |
| 发送消息 | 在已有会话里发文字指令，看流式输出 |
| 新建会话 | 选择已有 cwd 或手填路径，创建新 thread |
| 工具调用审批 | Codex 请求执行命令/改文件时，手机端可批准/拒绝 |
| 中断当前回合 | 一键中止正在运行的 turn |
| 连接状态 | 清晰显示"电脑在线 / 电脑离线 / 连接中" |
| 离线提示 | 电脑关机时，手机端明确显示"请先开启电脑 Mirror X Codex" |

### 3.2 次优先（P1，后续迭代）

- 文件 diff 展示（工具调用结果的代码差异视图）
- 上传图片到会话
- 会话搜索过滤
- 推送通知（turn 完成时提醒手机）

### 3.3 明确不做

- 手机直接调用中转 API（手机只控制桌面 Codex，不绕过桌面）
- 独立 App 上架应用商店
- macOS 手机配对（暂缓）

---

## 4. 技术方案

### 4.1 架构总览

```
手机浏览器 (PWA)
    │ WebSocket (wss)
    ▼
中继服务 (193.112.101.159:8765)     ← 国内服务器，手机直连，无需 VPN
    │ WebSocket (ws, outbound)
    ▼
Mirror X Codex 桌面端 (host 进程，Windows 常驻)
    │ stdio JSON-RPC
    ▼
codex app-server (本机，官方 CLI 驱动)
```

三方通信均经 **AES-256-GCM 端到端加密**，中继服务只做不透明字节转发，看不到内容。

### 4.2 配对机制（"key 即连接"）

1. 用户在 Mirror X Codex Manager 已填的中转 key，无需再输任何东西。
2. Manager 计算：`room_id = SHA-256(key)[0:16]`（hex），`enc_key = SHA-256(key)`。
3. Manager 内嵌 host 进程，持续向中继服务发起连接：`wss://relay/host?room=<room_id>`，并用 AES-GCM 加密所有内容。
4. 手机 PWA：用户首次输入 api key → PWA 计算相同 room_id/enc_key → 连接 `/client?room=<room_id>` → 配对成功 → 之后所有通信 AES-GCM 加解密。
5. key 存 localStorage，下次打开免输入。

**安全分析**：
- 网络层：wss TLS，中间人无法读取
- 中继层：中继只见密文，无法解密
- 应用层：AES-GCM，密钥由 api key 派生，key 不泄露则内容无法解密
- 结论：手机控制权与 api key 访问权绑定，风险级别相同

### 4.3 关键 RPC 接口（已实测 codex 0.147.0-alpha.6.6）

| 方法 | 用途 |
|------|------|
| `initialize` / `initialized` | 握手，获取 codexHome |
| `thread/list` | 会话列表，每条带 `cwd`、`preview`、`status` |
| `thread/turns/list` | 完整会话历史 |
| `thread/resume` | 恢复会话 |
| `turn/start` | 发送新消息 |
| `turn/started` / `turn/completed` | 流式状态通知 |
| `thread/status/changed` | 状态变更推送 |

实测结论：`codex app-server`（无子命令，stdio JSON-RPC）在 Windows 上正常工作；`daemon` 子命令在 Windows 上明确不支持（已验证报错"only supported on Unix platforms"）。桌面 host 直接 `spawn` 进程保持存活。

### 4.4 组件分工

| 组件 | 位置 | 工作量 |
|------|------|--------|
| 中继服务 | `apps/codex-plus-mobile-relay/src/main.rs`（修复安全缺陷后部署到服务器） | 已有完整实现，修安全 + 加限速 |
| 桌面 host | `crates/codex-plus-core/src/launcher.rs`，恢复 `bd8a5ef` 手机 host 逻辑 | 重写鉴权部分，移除 MobileControl 相关测试断言 |
| Manager UI | `apps/codex-plus-manager/src/`，增加"手机控制"面板 + 二维码 | 新增面板 |
| 手机 PWA | 重写 `fn mobile_relay_page()` 内嵌 HTML，或独立 HTML 部署到服务器 | 全新写，响应式 |

### 4.5 必须修复的安全缺陷（阻塞上线）

原型中 `token=${room}`，任何人知道 room_id 就能接入。新方案：
- 同一 room 只接受一个 host，host 未在线时拒绝 client 接入
- client 握手后发第一条消息，host 侧 AES-GCM 解密失败则立即断开
- 中继服务限速：每 IP 每分钟最多 10 次握手尝试

---

## 5. 用户流程

### 5.1 初次配对（一次性，约 1 分钟）

```
① Manager 点击"手机控制" → 显示二维码（内嵌 PWA 地址 + room 参数）
② 手机扫码 → 浏览器打开 PWA 页面
③ PWA 提示输入 api key（输一次，存 localStorage）
④ 连接成功 → 直接进入项目列表
⑤ 首次打开提示"添加到桌面"（可选）
```

### 5.2 日常使用

```
手机打开 PWA → 读取存储的 key → 自动连接
  ├─ 电脑在线 → 直接显示项目列表
  └─ 电脑离线 → "请先打开电脑上的 Mirror X Codex"（轮询重连）
```

---

## 6. 非功能要求

| 项目 | 要求 |
|------|------|
| 延迟 | 手机发消息到 Codex 收到 < 500ms（扣除 Codex 本身处理时间） |
| 并发 | 每个 room 同时最多 1 个 client，新连接踢出旧连接 |
| 可用性 | 中继服务宕机不影响桌面端 Codex 正常使用（桌面独立运行） |
| 数据安全 | 中继服务不持久化任何消息 |
| 不破坏现有 | 手机控制开关默认关闭；用户关闭时 host 进程不启动，Codex 行为与当前完全一致 |

---

## 7. 发布计划

### 阶段一 — 基础设施（v1.2.39）

- 修复中继服务安全缺陷 + 限速，Docker 化部署到 `193.112.101.159:8765`
- 桌面 host 进程恢复进 Manager（开关默认关闭）
- 手机 PWA：项目列表 + 基础历史只读展示

### 阶段二 — 完整交互（v1.2.40）

- 发送消息 + 流式输出渲染
- 工具调用审批 + 中断 turn
- 新建会话
- Manager 二维码 UI

### 阶段三 — 体验打磨（v1.2.41+）

- 推送通知（PWA Web Push）
- 文件 diff 展示
- 多设备管理（当前只允许 1 个手机连接）

---

## 8. 风险

| 风险 | 缓解 |
|------|------|
| `codex app-server` 接口随版本变化 | 版本检测 + 降级提示 |
| 中继服务被扫描滥用 | 握手限速 + 密文校验失败即断开 |
| api key 截图泄露 | 文档提示不要分享二维码截图；二维码只编码 room_id 不编码明文 key |
| Windows 防火墙阻断中继出站 | 安装包安装时一次性添加出站规则 |

---

## 9. 待决策

| 问题 | 当前默认 | 说明 |
|------|----------|------|
| 中继是否配 HTTPS 域名 | 直接 IP+端口 | iOS Safari 对非 wss 的 WebSocket 有限制，建议配域名 + Let's Encrypt |
| PWA 托管位置 | 中继服务器同时提供静态资源 | 后续可挂 CDN |
| 二维码是否嵌入明文 key | 否（只嵌 room_id + 服务器地址） | 用户在 PWA 首次输入 key，安全更好，但步骤多一步 |
