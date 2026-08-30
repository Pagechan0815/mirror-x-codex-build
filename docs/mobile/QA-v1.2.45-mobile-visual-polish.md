# Mirror X Codex v1.2.45 手机端视觉与交互验收

日期：2026-08-15

## 结论

本轮 PWA build 已升级为 `v20260815.5`。已完成商业化视觉优化、多格式文件预览、执行记录折叠、运行态导航、同步状态反馈与附件入口重设计，并通过隔离环境和公网 Chromium 回归验证。

`v20260815.5` 已于 2026-08-15 发布到 `relay.jingziai.club`。本轮只替换 Nginx 独立托管的静态 PWA 文件，没有重启生产 Relay、没有切换端口、没有中断现有 WebSocket 连接，也没有 commit 或 push。`D:\mirrorplus` 已安装 Manager 仍为 `1.2.45`。

## 已完成

- 深色绿色品牌视觉、渐变背景、半透明顶栏与输入栏。
- 强化连接状态、选中态、消息层级、附件卡片和工作中状态。
- 手机、平板、横屏和桌面布局分级，不再把大屏设备错误放大为手机布局。
- 输入框适配 iOS/Android 软键盘，键盘弹出时压缩冗余顶部空间。
- 明确展示“＋附件”，支持图片、视频、文档与压缩包。
- 320px 窄屏下保留 44px 触控区域，并自动收起附件文字。
- 移动端抽屉支持按钮关闭和 Android 系统返回键关闭。
- 内部控制消息自动净化，Markdown、代码块、表格、引用和 `[!image]` 正常展示。
- 会话中的本机图片路径自动加载为缩略图，点击后进入大图预览。
- Markdown 文件默认渲染，可在“查看源码 / 查看预览”之间切换。
- 图片、Markdown、文本、代码、JSON、PDF 与常见视频格式进入统一文件查看器。
- 执行记录默认折叠为一行，点击后查看完整命令和输出。
- 顶部导航显示任务执行中状态；手机菜单按钮在有运行任务时显示呼吸提示点。
- 抽屉顶部显示执行中任务数量，会话和项目条目显示“执行中 / 正在运行”。
- 会话标题旁显示“正在同步 / 实时接收中 / 刚刚同步 / 同步可能延迟”，支持点击手动刷新。
- 历史会话、项目和会话正文首次读取时使用骨架屏，不再只显示静态“正在读取”。
- 附件入口改为独立的回形针加号按钮，触控区域统一为 `48×48`，已选择附件时显示数量角标。

## 自动化验证

| 项目 | 结果 |
|---|---|
| `cargo test -p codex-plus-mobile-relay -- --nocapture` | 11/11 通过 |
| `node scripts/mobile_pwa_format_check.mjs` | 通过 |
| `node scripts/mobile_reconnect_check.mjs` | 通过 |
| WebKit 430×932 | 无横向溢出，输入栏可见 |
| WebKit 430×500 键盘模拟 | 顶栏收起，输入栏贴合可视区底部 |
| WebKit 320×568 抽屉 | 打开、关闭均通过 |
| Android Chromium 系统返回键 | 抽屉关闭且页面不退出 |
| Android Chromium 844×390 横屏 | 无横向溢出，输入栏可见 |
| Android Chromium 附件流程 | 选择、上传、发送、回复均通过 |
| Android Chromium 本机图片 | 会话内缩略图与大图查看均通过 |
| Android Chromium Markdown | 渲染与源码切换均通过 |
| Android Chromium 执行记录 | 默认一行、展开和收起均通过 |
| 320×568 附件按钮 | 48px 点击区域可见，未横向溢出 |
| WebKit / Chromium 控制台 | 0 error / 0 warning |
| 公网 PWA build | `v20260815.5` |
| 公网 320×568 / 390×844 / 430×932 / 844×390 | 无横向溢出，输入栏可见，附件按钮不小于 48px |
| 公网执行中状态 | 顶栏、抽屉、历史会话与项目角标均可见 |
| 公网附件流程 | 选择、上传、发送、Markdown 回复均通过 |
| 公网控制台 | 0 error / 0 warning |
| 生产 Relay | 保持运行，未重启，健康检查为 `ok` |
| 发布回滚备份 | `/var/backups/mirror-x-mobile/v20260815.5/` |

## 截图证据

- `output/playwright/v20260815.3-mobile-390x844-final.png`
- `output/playwright/v20260815.3-mobile-390x480-keyboard-final.png`
- `output/playwright/v20260815.3-desktop-1440x900-final.png`
- `output/playwright/v20260815.3-ios-webkit-430x932-final.png`
- `output/playwright/v20260815.3-ios-webkit-430x500-keyboard-final.png`
- `output/playwright/v20260815.3-ios-webkit-320x568-drawer-open.png`
- `output/playwright/v20260815.3-ios-webkit-320x568-drawer-closed.png`
- `output/playwright/v20260815.3-android-chrome-844x390-landscape.png`
- `output/playwright/v20260815.3-android-attachment-send-final.png`
- `output/playwright/v20260815.4-mobile-image-markdown-plus.png`
- `output/playwright/v20260815.4-markdown-source-toggle.png`
- `output/playwright/v20260815.4-tool-expanded.png`
- `output/playwright/v20260815.4-mobile-320-plus-visible.png`
- `output/playwright/v20260815.5-mobile-390x844-runtime-sync-attachment.png`
- `output/playwright/v20260815.5-mobile-390x844-drawer-runtime.png`
- `output/playwright/v20260815.5-mobile-390x844-attachment-selected.png`
- `output/playwright/v20260815.5-public-mobile-390x844-runtime-sync.png`
- `output/playwright/v20260815.5-public-mobile-390x844-drawer-runtime.png`
- `output/playwright/v20260815.5-public-mobile-390x844-attachment-selected.png`

## 尚未完成的边界

- 尚未在真实 iPhone、Android 手机上做物理键盘、系统相册、相机和文件管理器验收。
- 本轮已覆盖公网 Chromium 和隔离测试房间，但仍不替代多品牌真实 Android 与真实 iPhone 的系统级文件选择器验收。
- 手机端与桌面端共享 Codex App Server、会话与项目数据，但不是桌面画面镜像；“同步”表示协议事件和落盘历史同步。
- 本轮没有生成或安装新的 Windows 安装包。
- 生产 Relay 仍为现有版本，本轮未处理 `/status` 暴露过多房间标识信息的问题；应另开安全修复，先做兼容验证和灰度发布。
- 服务器 SSH 暴露面存在大量失败登录记录；应另行完成密钥登录、限制来源和禁用密码直登，不在本轮 UI 发布中直接修改。
