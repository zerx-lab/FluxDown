# 功能演示插件（fluxdown@feature-demo）

覆盖插件平台全部能力的手工验收插件。纯 JS，无需构建，直接以 dev 目录安装。

## 安装

设置 → 扩展 → 插件 → 「安装开发目录」，选择本目录 `packages/plugin-sdk/examples/feature-demo`。


`archive-rules.json` 是用于验收的有效签名规则包。它使用公开的 Ed25519 测试向量密钥；任何人都能取得对应私钥，因此**禁止用于正式插件或生产规则库**。


## 验收清单

- [ ] 安装后弹出授权对话框：风险徽标为「完全信任」（含 `desktop`、`pageScripts`），列出 6 个权限（任务 / 浏览器规则 / 桌面界面 / 后台常驻 / 网页脚本 / 归档分类）与网络主机 `httpbin.org`，并说明原生文件 / 进程 / 网络操作不逐次记录。
- [ ] 未勾选「我已了解」时确定按钮不可用；取消后插件行显示「需要授权」徽标 + 「授权」按钮，启用开关不可用。
- [ ] 授权后插件变为已加载、已启用。

### 活动栏（可移植视图「演示面板」，GPUI 与 Web 都应显示）
- [ ] 活动栏出现「演示面板」与「桌面脚本面板」两个 ✨ 图标；可拖动位置，重启后位置保留。
- [ ] 右上角 `tick N` 每 2 秒递增（设置里「面板计时间隔」可改）。
- **控件** tab
  - [ ] 计数按钮、四种 variant、禁用、加载中按钮显示正确；四个通知按钮弹出对应级别通知。
  - [ ] 输入框快速打字不丢字、不回跳；回车后「最近 submit」更新并弹通知；密码框掩码、数字框、多行文本、开关、复选框、下拉均能回显到下方 kv。
- **展示** tab
  - [ ] 三级标题、7 种色调徽标、各尺寸 / 等宽 / 截断文本、分隔线。
  - [ ] 进度条随 tick 变化、不确定进度、加载指示、4 种 callout、markdown（含代码块）、code、链接（点击打开浏览器）、图片、空状态、section 内滚动区。
- **任务** tab
  - [ ] 表格列出真实任务：大小（字节格式化）、百分比、进度条、状态徽标、创建时间（本地时区）。
  - [ ] 行操作 暂停 / 继续 / 删除 生效（删除会连文件一起删）。
  - [ ] 「新建示例任务」创建 1 MB 下载；「新建 resolver 改写任务」创建的任务 URL 被改写为 `proof.ovh.net/files/1Mb.dat`、文件名 `feature-demo-resolved.dat`。
  - [ ] 「连续新建 21 次」：前 20 次成功（已暂停），第 21 次提示 `tasks.create rate limited`。验收后在下载页批量删除。
- **事件** tab
  - [ ] 下载开始 / 完成 / 失败 / 取消 / 元信息事件逐条出现；完成时弹「任务完成」通知（设置「任务完成时弹通知」关闭后不再弹）。
  - [ ] 「最近命令」记录从命令面板 / 右键执行的命令。
- **宿主能力** tab
  - [ ] `flux.fetch` 返回 httpbin JSON；storage 计数每次 +1；fs 写读并列出文件；settings.update 在「你好 / Hello」间切换，面板标题同步变化。
  - [ ] 「setTimeout」3 秒后弹通知。
  - [ ] 「render 抛错」：视图顶部出现错误条，下一次 tick 后自动消失。

### 命令与任务右键
- [ ] ⌘K 命令面板可搜到「功能演示 Feature Demo: 打个招呼」与「暂停/继续所选任务」；执行后弹通知，带上当前选中任务数。
- [ ] 下载页选中任务右键：在多选分组与删除分组之间出现「暂停/继续所选任务」「查看任务信息」，执行生效。

### 任务详情 tab
- [ ] 任务详情（下载页详情面板与独立详情窗口）出现「演示详情」tab，显示该任务的进度与信息，进度实时更新；暂停 / 继续按钮生效。

### 设置视图
- [ ] 插件行设置按钮打开的对话框有「插件设置」「基础设置」两个 tab；「插件设置」里保存问候语、切换模式后，「基础设置」表单显示新值，反之亦然。

### 签名分类规则（归档插件）
- [ ] 插件详情显示规则版本、规则数、签名公钥；手动刷新成功且因未配置 `updateUrl` 不发起网络请求。
- [ ] 将分类规则配置到任务分类路由后，`.mkv` 任务按签名规则归入 `Series` 子目录；手动指定保存目录仍优先。
- [ ] 通过插件 `settings` 视图调用 `flux.archiveRules.status()` / `update()`，状态与详情一致；配置项里不展示私钥。

### 桌面脚本（仅 GPUI；Web 不显示这些仅桌面视图的入口）
- **桌面脚本面板**（活动栏，嵌入式）
  - [ ] 显示 identity、语言、`colors.primary`；切换主题后颜色值与 revision 变化。
  - [ ] `invoke stats` / `invoke echo` 返回 main 的结果；`call daemon.task.list` 返回任务数；非白名单 `system.shutdown` 报错。
  - [ ] fetch httpbin 成功，fetch example.com 被拒（未授权主机）；`fluxdown.toast` 弹出通知。
- **桌面脚本窗口**（扩展 → 插件行「打开界面」按钮）
  - [ ] 窗口 720×520 打开；gpui-shell toast 与对话框可用；`fluxdown.toast` 可用。
  - [ ] 「写主目录文件」在 `~/fluxdown-feature-demo.txt` 写入，「读回文件」显示内容。

### 浏览器扩展规则层
- [ ] 扩展选项页 → 规则：同步状态出现 plugin 层；规则表出现 `plugin.fluxdown.feature-demo.demo-ext`（色调 accent）。
- [ ] `demo-headers-rejected`（改写请求头）被扩展拒收，原因「plugin 规则不得改写请求头」。
- [ ] 禁用插件后再同步，plugin 层规则清空。

### 页面脚本
- [ ] 授权对话框的权限列表含「在网页中运行脚本」，「页面脚本」分区列出「页面脚本演示」与 `https://httpbin.org/*`；风险徽标为「完全信任」。
- [ ] 浏览器扩展选项页开启插件页面脚本并确认后，打开 `https://httpbin.org/` 控制台出现 `[feature-demo] page-hello { greeting: "hello from feature-demo", caller: "page", ... }`。

### 审计
- [ ] 插件 tab 顶部「审计日志」与插件详情「审计记录」可见：`consent.granted`、`install`、`fetch`（target 不含 query）、`tasks.create` / `tasks.control`、`settings.update`、`fs.write`、`desktop.load` / `desktop.invoke` / `desktop.call` / `desktop.unload`。
- [ ] 「加载更多」、「复制 JSON」、「清空」（二次确认）可用。

### 生命周期
- [ ] 禁用插件：活动栏两个图标消失，已打开的演示面板显示「已结束」；重新启用后「重新打开」恢复。
- [ ] 修改本目录 `main.js` 后在插件行点「重新加载」，面板会话关闭并可重新打开、拿到新代码。

## 不覆盖的能力

`ffmpeg` / `ytdlp` / `auth` 权限与订阅 provider 依赖受管组件或站点账号，本插件不声明，以免授权与风险判定被它们干扰；它们由现有插件覆盖：多文件清单、`dom` 规则与 `memoryLimitMb` 见同目录的 `youtube-down`，`ytdlp` 等见索引仓中的 yt-dlp 插件。
