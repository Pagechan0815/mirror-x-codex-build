# Mirror X Codex 安装与使用教程

适用版本：`v1.2.57`

当前公开支持：Windows 10/11 64 位

说明：`v1.2.57` 包含纯 API provider 修复、历史会话保护、工具与插件独立管理和 Image Skill 注册。macOS Intel 与 Apple Silicon 的源码和发布链路已具备，但 Mirror X 安装包尚未完成本仓库 macOS runner 验收，暂不向普通用户开放。

适用对象：第一次使用 Codex、只会复制粘贴 Key 的普通用户

Mirror X Codex 不是另一个聊天软件。它是官方 Codex Desktop 的接入工具：

```text
安装官方 Codex
       ↓
安装 Mirror X Codex
       ↓
填写并验证镜子AI Key
       ↓
勾选 Key 有权使用的模型
       ↓
应用接入并从 Mirror X 打开 Codex
```

Mirror X Codex 支持：

- GPT、Grok 等文本模型接入。
- 不同分组使用不同 Key。
- 混合 API 和纯 API 两种模式。
- 保留已有 MCP 和插件配置。
- 独立的镜子AI生图 Image Key。
- 通过 `$jingzi-imagegen` 调用 `gpt-image-2`。
- 恢复首次接入前保存的 Codex 配置基线。

> API Key 只填写在本机 Mirror X Codex 界面。不要把 Key 发到群聊、截图、工单、教程或 Codex 对话里。

## 一、安装前准备

准备以下内容：

1. 官方 Codex Desktop。
2. 至少一个镜子AI文本模型 Key。
3. 如需生图，再准备一个具有 `gpt-image-2` 权限的 Image Key。

### 没有安装官方 Codex

先打开 Mirror X Codex，在“开始前检查”中查看 Codex 状态。

如果显示“未安装”：

1. 点击“安装 Codex”。
2. 按跳转页面安装官方 Codex。
3. 完成后返回 Mirror X Codex。
4. 点击“重新检测”。

Codex 不一定只能通过 Microsoft Store 安装，以官方当前提供的安装入口为准。

## 二、下载正确的安装包

`v1.2.57` 对应文件如下：

| 电脑类型 | 安装包 |
| --- | --- |
| Windows 10/11 64 位 | `mirror-x-codex-1.2.57-windows-x64-setup.exe` |
| Mac Intel 芯片 | 暂未开放下载 |
| Mac Apple 芯片（M1/M2/M3/M4） | 暂未开放下载 |

项目发布页：

<https://github.com/Pagechan0815/mirror-x-codex-build/releases/latest>

如果发布页提示无权限，说明当前仓库没有向该账号公开。此时应向镜子AI管理员索取对应安装包，不要从陌生网盘或第三方页面下载。

### Mac 版本说明

CodexPlusPlus `v1.2.56` 的 Mac arm64 与 x64 安装包已在真实 GitHub macOS runner 完成构建、DMG 结构检查和管理器启动测试，因此上游 Mac 发布链路可以作为有效参考。

Mirror X 已参考这套逻辑补齐二进制校验、`PkgInfo`、图标、管理器窗口属性、`mirrorplus://` 唤起、DMG 重试和双架构验收脚本。但 Mirror X 自己的两个 macOS job 目前因 GitHub Actions 账户额度问题没有获得 runner，不能把源码检查写成真机验收。

在 Mirror X 的 macOS arm64 与 x64 job 真正运行并全部通过之前：

1. 不向普通用户分发旧版 Mirror X Mac 包。
2. 不使用 Windows 或静态检查结果宣称 Mac 已验证。
3. 不要求普通用户通过终端命令绕过系统限制。

后续开放时，`x64` 只给 Intel，`arm64` 给 Apple Silicon，不得混用架构包。

1. 点击屏幕左上角苹果菜单。
2. 选择“关于本机”。
3. 显示“芯片 Apple M...”时下载 `arm64`。
4. 显示“处理器 Intel...”时下载 `x64`。

## 三、安装 Mirror X Codex

### Windows

1. 完全退出旧版 Mirror X Codex。
2. 双击 `mirror-x-codex-1.2.57-windows-x64-setup.exe`。
3. 按安装向导完成安装。
4. 从桌面或开始菜单打开 `mirror x codex`。

一般不需要管理员权限。

如果 Windows SmartScreen 提示来源未知：

1. 确认文件来自镜子AI管理员或项目发布页。
2. 点击“更多信息”。
3. 点击“仍要运行”。

### macOS

Mirror X Mac 安装包暂未开放。不要安装历史旧包，也不要从陌生网盘下载或在终端执行绕过 Gatekeeper 的命令。正式开放后，本节会提供不需要终端的安装步骤。

## 四、第一次接入

### 第一步：退出 Codex

应用接入前必须完全退出 Codex。

- Windows：确认任务栏和任务管理器中没有 Codex 主程序。
- macOS：使用 `Command + Q` 退出，不要只关闭窗口。

### 第二步：运行安装前检查

打开 Mirror X Codex，确认“开始前检查”中的主要项目就绪：

- Codex 已安装。
- Codex 配置目录可读写。
- Mirror X 安装完整。
- 本机可以连接镜子AI接口。

检查未通过时，不要直接应用接入。

### 第三步：填写文本模型 Key

Mirror X Codex 默认提供两个文本模型分组。

#### CodexPro Key

用于 GPT、Grok 以及 CodexPro 分组中的其他模型。

操作：

1. 粘贴 CodexPro Key。
2. 点击“验证”。
3. 等待模型列表返回。
4. 只勾选该 Key 实际返回的模型。

#### 企业GPT专线（极稳）Key

企业专线与 CodexPro 使用方式相同，但走独立且更稳定的 GPT 通道。

操作：

1. 从镜子AI后台复制 `企业GPT专线（极稳）` 分组生成的 Key。
2. 粘贴到企业专线卡片。
3. 点击“验证”。
4. 只勾选该 Key 实际返回的模型。

不要把企业专线 Key 填入 CodexPro 卡片，也不要把同一个模型同时分配给两个 Key。

### 第四步：选择默认模型

每个已填写的 Key 都必须：

1. 验证成功。
2. 至少勾选一个模型。
3. 从全部已勾选模型中选择一个默认模型。

模型不在验证结果中，通常表示该 Key 没有此模型权限。不要手写模型名称绕过验证。

### 第五步：可选启用镜子AI生图

需要生图时：

1. 打开“镜子AI生图”开关。
2. 粘贴具有 `gpt-image-2` 权限的 Image Key。
3. 点击“验证”。
4. 看到“Image Key 有效，已检测到 gpt-image-2”。

Image Key 与 CodexPro Key、企业专线 Key 分开保存和使用。

### 第六步：选择接入模式

| 模式 | 适用用户 | 实际行为 |
| --- | --- | --- |
| 混合 API | 已登录官方 ChatGPT/Codex | 保留官方登录状态，镜子AI模型使用对应 Key |
| 纯 API | 不登录官方 ChatGPT/Codex | 文本模型全部通过镜子AI Key 使用 |

不确定时：

- 已经登录官方 Codex：选“混合 API”。
- 明确不登录、只使用镜子AI：选“纯 API”。

### 第七步：应用并打开 Codex

1. 点击“应用接入”。
2. 等待提示“镜子AI接入已生效”。
3. 完全退出仍在运行的 Codex。
4. 点击 Mirror X Codex 中的“打开 Codex”。

使用多个 Key 分组时，建议始终从 Mirror X Codex 的“打开 Codex”启动，以确保本地模型路由正在运行。

## 五、纯 API 模式下的插件、MCP 和 Skill

纯 API 模式不等于删除插件。

当前 `v1.2.57` 会：

- 保留 `config.toml` 中已有的 MCP 配置。
- 保留已有本地 Skill 和插件目录。
- 注册 Mirror X 安装包内置的插件市场快照。
- 尽量在 Codex 中继续显示插件市场入口。
- 在开启生图时安装 `$jingzi-imagegen`。

但“插件显示出来”和“插件能够使用”不是同一件事：

| 插件类型 | 纯 API 未登录状态 |
| --- | --- |
| 本地 Skill | 通常可以使用 |
| 本地 MCP server | 本机命令和依赖正常时可以使用 |
| 使用独立 API Key 的工具 | 配置对应 Key 后可以使用 |
| GitHub、Adobe、Slack 等远程插件 | 需要各自的登录或 OAuth |
| 依赖官方 ChatGPT 工作区权限的功能 | 未登录时可能不可用 |

因此纯 API 模式可以保留插件框架和入口，但不能绕过插件自身的账号授权。

## 六、在 Codex 中使用文本模型

完成接入并从 Mirror X Codex 打开 Codex 后：

1. 新建一个任务。
2. 打开模型选择器。
3. 选择刚才勾选的模型。
4. 发送一条简单消息测试。

建议先测试：

```text
请回复当前模型名称，并用一句话说明你已正常工作。
```

如果更换 Key 或调整模型：

```text
重新验证 Key
→ 重新勾选模型
→ 应用接入
→ 完全退出 Codex
→ 从 Mirror X Codex 打开
```

## 七、使用镜子AI生图

`gpt-image-2` 是生图工具，不会出现在顶部文本模型选择器。

正确流程是：

```text
当前 GPT / Claude 模型理解需求
           ↓
调用 $jingzi-imagegen
           ↓
镜子AI gpt-image-2 生成图片
           ↓
保存到当前项目
```

### 推荐提示词

当前版本建议明确写出 `$jingzi-imagegen`，这样才能确定走镜子AI，而不是 Adobe 或官方 `image_gen`。

```text
使用 $jingzi-imagegen 生成一张坐在窗边的橘猫图片，
保存到 output/imagegen/orange-cat.png。
```

海报示例：

```text
使用 $jingzi-imagegen 生成一张竖版科技活动海报，
主题是“AI 创业实战”，深色背景，中文主标题清晰，
保存到 output/imagegen/ai-event-poster.png。
```

### 当前生图能力

- 固定使用 `gpt-image-2`。
- 支持文生图。
- 支持一次生成 1 到 10 个变体。
- 支持接口返回临时 URL 或 `b64_json`。
- 图片保存到当前项目目录。
- 不依赖 Adobe 登录。
- 不依赖官方 `OPENAI_API_KEY`。
- 不需要用户安装 Python。

当前暂不支持：

- 图片编辑。
- 蒙版编辑。
- 参考图上传。
- 原生透明背景。

每次请求可能产生费用。批量生图前要明确数量。

### 为什么只说“帮我生图”仍可能打开 Adobe

Codex 可以自行选择匹配的 Skill 或插件。当前版本仅安装 `$jingzi-imagegen`，还没有实现强制生图意图路由。

因此：

- 明确写 `$jingzi-imagegen`：确定使用镜子AI。
- 只说“生成图片”：可能使用镜子AI，也可能选择 Adobe 或官方生图工具。
- 明确说“使用 Adobe/官方生图”：会使用对应官方工具。

## 八、升级旧版本

`v1.2.57` 包含生图接口同时返回空 `b64_json` 和有效临时 URL 时的兼容修复。新版本只解码非空 `b64_json`，否则继续下载 URL；任何来源得到 0 字节数据都会报错且不会写入空文件。

生图功能从 `v1.2.36` 开始提供。

升级步骤：

1. 完全退出 Codex。
2. 完全退出旧版 Mirror X Codex。
3. 运行对应系统的 `v1.2.57` 安装包覆盖安装。
4. 打开 Mirror X Codex。
5. 确认版本为 `1.2.57`。
6. 需要生图时重新打开生图开关并验证 Image Key。
7. 从 Mirror X Codex 点击“打开 Codex”。

Windows 安装目录不是默认目录时，覆盖安装要确认仍指向原目录，避免电脑中出现两个版本。

## 九、恢复使用前状态

操作：

1. 完全退出 Codex。
2. 打开 Mirror X Codex。
3. 点击“恢复使用前状态”。
4. 等待恢复完成。
5. 按原来的方式打开 Codex。

当前版本会使用首次接入时创建的本机基线恢复：

- `config.toml`
- `auth.json`
- Mirror X 管理设置
- `$jingzi-imagegen` Skill
- 镜子AI Image Key 配置

如果接入前已经存在同名生图 Skill 或配置，Mirror X 会尝试恢复原文件。

### 恢复功能的真实边界

当前 `v1.2.57` 的恢复目标是“回到首次接入时保存的基线”，不能宣传为任何情况下都绝对无损。

需要注意：

- 接入后用户自行修改的同一份 `config.toml`，可能被首次基线覆盖。
- 不要在 Codex 正在运行时执行恢复。
- 不要手动删除 Mirror X 的数据目录，否则可能丢失恢复基线。
- 恢复前有重要自定义配置时，建议另外备份 `~/.codex`。
- 当前版本存在会话归属同步逻辑；恢复后若旧会话模型显示异常，应新建任务验证，不要继续反复修改旧会话。

推荐普通用户在首次接入后，不要手动编辑 `config.toml` 和 `auth.json`。

## 十、常见问题

### 验证 Key 后没有模型

- 检查 Key 是否完整，前后不要有空格或换行。
- 确认 Key 已开通对应模型分组。
- GPT/Grok 使用 CodexPro Key。
- 企业专线使用 `企业GPT专线（极稳）` 分组生成的 Key。
- 点击对应卡片的“验证”，不要手写模型名。

### Codex 中看不到刚勾选的模型

1. 每个已填写 Key 至少勾选一个模型。
2. 设置默认模型。
3. 点击“应用接入”。
4. 完全退出 Codex。
5. 从 Mirror X Codex 点击“打开 Codex”。

### 模型能看到但请求失败

常见原因：

- 模型分配到了错误的 Key。
- Key 已失效。
- Key 没有该模型权限。
- 中转线路当前不可用。
- 上游账户余额不足。

回到 Mirror X Codex，重新验证对应 Key，只保留验证结果中实际返回的模型。

### 出现 `stream disconnected before completion`

该错误不一定是本机问题。

常见原因：

- 上游长连接中断。
- 模型生成时间过长。
- 会话上下文过大。
- 中转线路临时不可用。
- Chat Completions 与 Responses 协议转换不完整。

处理顺序：

1. 完全退出 Codex。
2. 从 Mirror X Codex 重新打开。
3. 新建一个短任务测试相同模型。
4. 切换到同一 Key 授权的其他模型测试。
5. 超长旧会话不要继续反复压缩，改用新任务并粘贴交接摘要。

`v1.2.36` 增加了 `/v1/responses/compact` 转发支持，但不能保证解决所有上游断流问题。

### Claude 工具调用或压缩时偶发断流

如果 Claude 普通短对话正常，但在 MCP、插件、工具调用或长会话压缩时出现：

```text
stream disconnected before completion
```

Mirror X 已修复一个兼容性问题：Codex 发送没有正文的工具元数据时，旧逻辑可能把它转换成空白消息，严格的 Claude 上游会拒绝这条消息。

当前状态：

- 修复提交：`1c4e468`
- 修复范围：只影响 Responses 转 Chat 的消息转换，不修改会话数据库、Key、MCP 或插件配置。
- 当前公开安装包：`v1.2.57`，已经包含该修复。
- 更新时完全退出 Codex 和 Mirror X，再直接覆盖安装即可，不需要先卸载旧版。

更新后如果旧任务仍保留异常状态，先新建任务测试，不要手动修改 Codex 会话数据库。

### Claude 切换回 GPT 后报错

先新建任务测试 GPT。

如果新任务正常，说明旧任务中可能残留 Claude 的工具调用、摘要或响应状态。不要修改会话数据库，保留旧任务作为历史记录，在新任务继续工作。

### 纯 API 下插件市场能看到但不能使用

插件市场入口由 Mirror X 保留，不代表插件已经完成授权。

- Adobe 需要 Adobe 账号授权。
- GitHub 需要 GitHub 连接。
- 其他远程插件需要各自 OAuth。
- 本地 Skill 和本地 MCP 不需要官方 ChatGPT 登录，但本机依赖必须正常。

### Codex 提示 Adobe 需要重新登录

说明本次任务没有调用 `$jingzi-imagegen`。

检查：

1. Mirror X Codex 中已启用“镜子AI生图”。
2. Image Key 验证时检测到 `gpt-image-2`。
3. 已点击“应用接入”。
4. 已完全退出并从 Mirror X Codex 重新打开 Codex。
5. 提示词明确写了“使用 `$jingzi-imagegen`”。

### 生图提示 Image Key 未配置

1. 返回 Mirror X Codex。
2. 打开生图开关。
3. 填写 Image Key。
4. 点击“验证”。
5. 点击“应用接入”。

不要把 Key 放进 Codex 提示词。

### 生图提示执行器不存在

说明安装包不完整或旧版本覆盖失败。

Windows 应存在：

```text
mirror-x-imagegen.exe
```

macOS 应用中应包含：

```text
Contents/MacOS/mirror-x-imagegen
```

Windows 用户重新安装完整的 `v1.2.57` 安装包。Mac 用户不要安装旧包，等待 Guide 明确开放对应芯片版本。

### Mac 提示应用已损坏

当前 Mirror X Mac 包没有向普通用户开放。删除来源不明或历史旧包，不要按陌生教程执行终端命令；联系镜子AI管理员并等待 Guide 发布已验证版本。

### 想更换 Key

1. 在对应分组替换 Key。
2. 重新验证。
3. 重新勾选模型。
4. 点击“应用接入”。
5. 完全退出并重新打开 Codex。

## 十一、获取帮助时请提供

不要发送任何 API Key。

请提供：

- 操作系统，例如 Windows 11 或 macOS。
- Mac 芯片类型，例如 Intel、M1、M2、M3、M4。
- Mirror X Codex 版本。
- 使用混合 API 还是纯 API。
- 使用的模型名称。
- 问题发生时间。
- 完整错误文字。
- 不包含 Key 的界面截图。
- 问题发生在验证 Key、应用接入、打开 Codex、模型请求还是生图阶段。

## 十二、给普通用户的最短操作版

```text
1. 安装官方 Codex
2. Windows 安装 Mirror X Codex v1.2.57；Mac 等待 Guide 开放已验证版本
3. 完全退出 Codex
4. 填写并验证 CodexPro / 企业专线 Key
5. 勾选模型并设置默认模型
6. 需要生图时打开生图开关并验证 Image Key
7. 选择混合 API 或纯 API
8. 点击“应用接入”
9. 点击“打开 Codex”
10. 生图时明确写“使用 $jingzi-imagegen”
```
