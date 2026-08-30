# Mirror X Codex v1.2.45 本机升级与公网 Canary 验收

- 执行日期：2026-08-16
- 授权范围：覆盖升级本机 `D:\mirrorplus`；部署独立公网 canary
- 明确未执行：生产 Relay 切换、生产 Relay 重启、Git commit、Git push
- Windows 版本：`1.2.45`
- PWA 构建：初始验收 `v20260815.7`；执行中默认引导更新后为 `v20260816.1`

## 1. 本机覆盖升级

使用完整 NSIS 安装包覆盖升级，而不是只替换单个 EXE。

安装包：

```text
D:\mirror++\CodexPlusPlus\dist\windows\mirror-x-codex-1.2.45-windows-x64-setup.exe
SHA256 A8FCD4E02A29E40D6F829FAED12730B356AE24D809245E8649B3FA24BB48CEC2
```

安装后文件：

| 文件 | 版本 | SHA256 |
| --- | --- | --- |
| `D:\mirrorplus\mirror-x-codex-manager.exe` | 1.2.45 | `64EABB86D750401FF4366CF7A5FE9E236713FCF866CD33AE10743E21F6219003` |
| `D:\mirrorplus\mirror-x-codex.exe` | 1.2.45 | `B9ED71AFF59505A078C46B38AF4FB54DADC35652EF57435A04E51F158FDBE9E9` |
| `D:\mirrorplus\mirror-x-imagegen.exe` | 1.2.45 | `D23850FDE9F2628BFE71B6D289F1F758D6F53F0E17B89E45C49408EF959AE2BD` |

安装注册信息 `DisplayVersion=1.2.45`，安装路径、桌面快捷方式和开始菜单快捷方式均指向 `D:\mirrorplus`。

升级前备份：

```text
D:\mirrorplus\backups\20260816-1215-v1.2.45-preinstall\
```

备份包含旧 Manager、Launcher、Imagegen、卸载器、注册表导出、快捷方式和切换 canary 前的 `settings.json`。

## 2. 公网 Canary 拓扑

生产保持：

```text
公网 /relay -> 127.0.0.1:8765
版本 1.2.39
```

新增 canary：

```text
公网 /relay-canary-v1245/ -> 127.0.0.1:8768
systemd: mirror-x-relay-canary-v1245.service
版本 1.2.45
PWA v20260815.7
```

Linux x64 Relay：

```text
/root/mirror-x-relay-canary/v1.2.45/codex-plus-mobile-relay-linux-x64
SHA256 4CB0DCD1D4AFDE8B832D3986BA61D1712B8B64ECFA55399E34B8427B635C3E55
```

Nginx 只在现有 Relay `server` 中增加一行 canary snippet include；生产 `/relay` 的 `proxy_pass http://127.0.0.1:8765` 未改变。`nginx -t` 通过后仅执行 graceful reload。

服务器回滚备份：

```text
/root/mirror-x-relay-canary/backups/20260816-v1245/relay.conf.before-v1245-canary
```

## 3. 公网真实 Host 端到端验证

本机已安装 Manager 连接公网 canary，验证脚本没有输出或保存 API Key、room、token、二维码 Fragment。

通过项：

- WSS 注册成功
- 加密 App Server 会话建立成功
- `initialize` 成功
- `thread/list` 成功，读取 20 条会话
- Host 宣告 `fileDownloadChunks`
- 17,852,851 bytes 安装包通过 69 个加密分块传回
- 接收文件 SHA256 与本机原文件一致

验证脚本：

```text
D:\mirror++\CodexPlusPlus\output\deploy-audit-v1.2.45\validate-installed-host-canary.py
```

## 4. 安全与隔离验证

公网 canary 已通过：

- Host 离线返回 `HOST_OFFLINE`
- Host 在线时错误 token 返回 `TOKEN_MISMATCH`
- 不同 Key 派生的房间相互隔离
- 第二个手机接管时旧手机收到 `CLIENT_REPLACED`
- 接管后的新客户端仍可双向传输加密消息
- `/status` 不暴露完整 room、token 或加密密钥

验证脚本：

```text
D:\mirror++\CodexPlusPlus\output\deploy-audit-v1.2.45\validate-public-relay-isolation.py
```

## 5. 公网移动视口验证

通过：

- `390×844` 竖屏工作区
- `390×844` 左侧抽屉打开
- 抽屉内部关闭按钮可收回抽屉
- `844×390` 横屏
- 附件按钮为 48×48，文案覆盖图片、视频和文件
- 页面显示运行中、已连接和等待电脑更新状态
- PWA 构建号显示 `v20260815.7`
- 浏览器控制台 `0 error / 0 warning`

证据：

```text
D:\mirror++\CodexPlusPlus\output\playwright\v20260815.7-public-canary-390x844.png
D:\mirror++\CodexPlusPlus\output\playwright\v20260815.7-public-canary-drawer-open-390x844.png
D:\mirror++\CodexPlusPlus\output\playwright\v20260815.7-public-canary-844x390.png
D:\mirror++\CodexPlusPlus\output\playwright\v20260815.7-public-canary-console-errors.txt
```

## 6. 当前真机测试方式

本机 Manager 已切换到：

```text
wss://relay.jingziai.club/relay-canary-v1245
```

手机控制保持开启，canary 二维码已在 Manager 中显示。用户只需用 Android 或 iPhone 相机扫码，在手机浏览器打开页面。

## 7. 回滚

本机回滚：

1. 关闭 Manager。
2. 从 `D:\mirrorplus\backups\20260816-1215-v1.2.45-preinstall\` 恢复三个 EXE、卸载器和注册表。
3. 如只回滚 canary 测试设置，恢复 `settings.before-canary.json`。
4. 重新启动 `D:\mirrorplus\mirror-x-codex-manager.exe`。

服务器 canary 回滚：

1. 用服务器备份恢复 `/etc/nginx/conf.d/relay.conf`。
2. 执行 `nginx -t`。
3. graceful reload Nginx。
4. 停止并禁用 `mirror-x-relay-canary-v1245.service`。

上述回滚不会切换、停止或重启生产 `8765` Relay。

## 8. 验收结论与边界

结论：

- Windows 本机安装版：通过
- 独立公网 canary：通过
- 真实 Manager Host 到公网 canary：通过
- 大文件分块与哈希一致性：通过
- 公网房间隔离和错误凭证：通过
- 生产未切换：已确认

仍需店主真机完成：

- 至少一台 Android
- 至少一台 iPhone
- 4G/5G 与 Wi-Fi 切换
- 锁屏恢复、切后台、软键盘遮挡
- 实际发送一句话和一个附件

在真机完成前，不能对外宣称“所有 Android/iPhone 均已验证”；可以对外说明“Windows 电脑端与公网 canary 已完成自动化和浏览器移动视口验收，正式生产切换待真机确认”。

## 9. 授权后收口复核

复核时间：`2026-08-16 13:24:35 +08:00`

### 9.1 版本、安装与运行一致性

| ID | Objective | Precondition | Steps | Expected Result | Priority | Risk Tag | Evidence | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| NP-013 | 确认安装包未漂移 | 候选安装包存在 | 重新计算 SHA256 | 与首次安装记录一致 | P0 | Compatibility | `A8FCD4E...CEC2` | Pass |
| NP-014 | 确认本机安装完整 | 已覆盖安装 | 核对注册表、快捷方式、三个 EXE | 版本均为 1.2.45，路径均为 `D:\mirrorplus` | P0 | Compatibility | 注册表、PE 版本、文件哈希 | Pass |
| NP-015 | 确认实际运行的是安装版 | Manager 正在运行 | 核对进程可执行文件绝对路径 | 仅 1 个 `D:\mirrorplus\mirror-x-codex-manager.exe` | P0 | Logic | Windows 进程快照 | Pass |
| AP-010 | 确认没有测试进程残留 | 完成 E2E | 扫描 mock、E2E、验证脚本进程 | 临时测试进程为 0 | P0 | Security | Windows 进程快照 | Pass |

### 9.2 回归测试复跑

| Test | Result |
| --- | --- |
| Mobile Relay 单元测试 | `11 passed / 0 failed` |
| Mobile Host 单元测试 | `23 passed / 0 failed` |
| Manager Windows subsystem | `24 passed / 0 failed` |
| 公网 canary 房间隔离 | Pass：`HOST_OFFLINE`、`TOKEN_MISMATCH`、room isolation、`CLIENT_REPLACED`、接管后可用、状态脱敏 |
| 已安装 Manager Host 公网 E2E | Pass：App Server 连接、20 条会话、69 个加密文件分块、17,852,851 bytes、SHA256 一致 |

### 9.3 测试连接和敏感临时文件清理

- canary 当前为 `1 room / 1 Host / 0 Client`。
- 本机当前仅运行真实安装版 Manager Host。
- mock Host、E2E 和验证脚本进程均为 0。
- 已删除 3 个 `public-canary-mock-*` 临时文件，其中一个 stdout 曾包含完整临时二维码 Fragment。
- 没有删除源码、正式截图、测试脚本、安装包或回滚备份。

### 9.4 生产未变化的只读证据

- 公网生产 `/health` 仍返回版本 `1.2.39`。
- 公网 canary `/relay-canary-v1245/health` 返回版本 `1.2.45`。
- 生产当前 3 个 Host 房间仍在线；本轮没有停止、替换或重启生产 Host。
- 本轮没有执行生产切流、生产 Relay 重启、Git commit 或 Git push。

### 9.5 新发现的商业化边界

| Bug ID | Title | Severity | Priority | Reproducibility | Scope | Root Cause Type | Status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| MXM-045-STATUS-01 | 生产 v1.2.39 `/status` 返回未遮罩 room 标识 | S2 | P1 | Always | Global | Backend/Security | Open on production; fixed in 1.2.45 canary |

说明：

- 当前接口没有返回 API Key、配对 token 或加密密钥。
- 但完整 room 标识仍属于不必要的信息暴露，不符合商业化的最小暴露原则。
- 正式发布时必须让生产使用 1.2.45 的状态脱敏逻辑，发布后再从公网验证所有 room 均已遮罩。
- 在店主完成 Android/iPhone 真机测试前，不执行生产切换。

### 9.6 最终 Acceptance Decision

- Windows 本机安装版：`Accept`。
- 独立公网 canary：`Accept for real-device testing`。
- 公网商业发布：`Conditional Accept`。
- 当前阻断项：
  1. Android 真机扫码、发消息、发附件、切网和锁屏恢复。
  2. iPhone 真机扫码、发消息、发附件、切网和锁屏恢复。
  3. 生产切换后复核 `/status` 脱敏；该动作需要单独生产发布授权。

回滚材料复核通过：安装包、9 个本机备份文件、设置备份、卸载器和注册表备份均存在。

## 10. 执行中默认引导更新

2026-08-16 晚间，独立公网 canary 已更新到 PWA `v20260816.1`：

- 会话空闲时仍使用 `turn/start`。
- 会话执行中再次发送时默认使用 `turn/steer`。
- 每次引导携带 `expectedTurnId`，避免误导到已经切换的任务。
- `review` 和手动 `compact` 不可引导时明确提示，不自动伪装成排队成功。
- 公网 390×844 浏览器流程验证为 `turn/steer=1 / turn/start=0`。
- 生产 `/relay` 仍为 `1.2.39`，本次只更新 `/relay-canary-v1245/`。

详细证据见 [QA-v1.2.45-steer-default-20260816.md](QA-v1.2.45-steer-default-20260816.md)。
