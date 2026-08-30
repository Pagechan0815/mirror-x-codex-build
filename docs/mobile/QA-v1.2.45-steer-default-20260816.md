# Mirror X Codex v1.2.45 执行中默认引导 QA

- 日期：2026-08-16
- Windows 应用：`1.2.45`
- PWA 构建：`v20260816.1`
- 改动目标：手机在 Codex 执行过程中再次发送时，默认引导当前任务，不再默认排队到下一轮。
- 生产影响：无。测试阶段未切换或重启生产 Relay。

## 1. 协议依据

官方 Codex App Server 和本机已安装 Codex Schema 均包含：

```text
turn/steer
params:
  threadId
  expectedTurnId
  input
  clientUserMessageId
response:
  turnId
```

本机 Schema 明确要求 `expectedTurnId`，请求只在它与当前 active turn 一致时成功。

`review` 和手动 `compact` 属于不可引导 turn，客户端必须明确报错。

## 2. 当前发送规则

| 当前状态 | 手机发送行为 | 结果 |
| --- | --- | --- |
| 会话空闲 | `turn/start` | 创建新任务 |
| 会话执行中且有 active turn ID | `turn/steer` | 新要求追加到当前任务 |
| 发送时任务刚好结束 | 重新读取 thread 后 `turn/start` | 作为下一轮发送，并明确提示 |
| active turn 已切换 | 刷新 ID 后只重试一次 `turn/steer` | 防止误导到旧任务 |
| `review` / 手动 `compact` | 不自动排队 | 恢复输入并提示稍后再发 |
| Codex 不支持 `turn/steer` | 不伪装成功 | 提示更新 Codex或先停止任务 |
| 网络中断、响应不确定 | `confirming` | 恢复后核对消息 ID，避免重复执行 |

## 3. 自动化结果

| ID | Objective | Precondition | Steps | Expected Result | Priority | Risk Tag | Evidence | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| NP-STEER-001 | 构造安全引导参数 | active turn 已知 | 生成 steer params | 包含 `expectedTurnId` 和消息 ID | P0 | Logic | Node 单测 | Pass |
| NP-STEER-002 | 执行中默认引导 | 390×844、active mock thread | 输入并发送 | 发出 `turn/steer`，不发 `turn/start` | P0 | Logic | Browser event log | Pass |
| NP-STEER-003 | 连续补充两次 | active turn 保持运行 | 连续发送两条补充 | 2 次 steer、2 个唯一消息 ID | P0 | Data | Browser event log | Pass |
| NP-STEER-004 | 空闲会话保持新任务语义 | 390×844、idle mock thread | 输入并发送 | 发出 `turn/start`，不发 `turn/steer` | P0 | Logic | Public canary event log | Pass |
| AP-STEER-001 | 不可引导阶段 | compact/review error | 解析 App Server error data | 显示中文边界提示 | P0 | UX | Node 单测 | Pass |
| AP-STEER-002 | 旧 Codex 不支持 | JSON-RPC method not found | 解析 `-32601` | 提示更新 Codex或停止任务 | P1 | Compatibility | Node 单测 | Pass |
| AP-STEER-003 | RPC 错误信息保留 | App Server 返回 code/data | Relay RPC 反序列化 | Error 保留 `code + data` | P0 | Logic | reconnect transport test | Pass |
| BC-STEER-001 | 缺少 turn ID | active 状态尚未同步 | 构造 steer | 阻止误发并提示稍后重试 | P0 | Data | Node 单测 | Pass |

回归：

- Mobile Relay：`11 passed / 0 failed`
- Mobile Host：`23 passed / 0 failed`
- Windows Manager：`24 passed / 0 failed`
- PWA format：Pass
- reconnect transport：Pass
- Python mock Host compile：Pass

## 4. 浏览器真实流程证据

测试环境：

```text
390×844
Desktop Sync
active turn: demo-active-turn
PWA: v20260816.1
```

事件结果：

```text
turn/steer: 2
turn/start: 0
expectedTurnId present: true
unique clientUserMessageId: 2
```

截图：

```text
D:\mirror++\CodexPlusPlus\output\playwright\v20260816.1-steer-active-turn-390x844.png
```

## 5. Acceptance Decision

- 本地源码与浏览器候选版：`Accept`
- Windows Host/Manager 兼容：`Accept`
- 独立公网 canary：`Accept`
- 生产：未发布

公网 canary 发布结果：

```text
path: /relay-canary-v1245/
Relay: 1.2.45
PWA: v20260816.1
production /relay: 1.2.39（未改变）
```

公网真实浏览器验证：

```text
viewport: 390×844
active turn:
turn/steer: 1
turn/start: 0
expectedTurnId present: true
clientUserMessageId present: true

idle thread:
turn/start: 1
turn/steer: 0
approvalPolicy: never
sandboxPolicy: dangerFullAccess
```

公网截图：

```text
D:\mirror++\CodexPlusPlus\output\playwright\v20260816.1-public-canary-steer-390x844.png
```

服务器回滚：

```text
/root/mirror-x-relay-canary/v1.2.45/codex-plus-mobile-relay-linux-x64.before-v20260816.1
/var/www/mirror-x-mobile-canary-v1245.before-v20260816.1/
```

不能承诺：

- `review` 和手动 `compact` 阶段可以被引导。
- 网络完全断开时仍能立即引导。
- 旧 Codex App Server 一定支持 `turn/steer`。

可以承诺：

- 支持 `turn/steer` 的 Codex 中，执行过程中再次发送默认影响当前任务。
- 不会默认把补充要求静默排队成下一轮。
- 通过 `expectedTurnId` 防止任务切换时误发。
