# zh（简体中文）翻译术语表

本表**仅用于机器起草官网文档** `website-v2/src/content/docs/zh/**`（由 GitHub Actions 工作流 `.github/workflows/i18n-mt.yml`，qwen-mt，从 en 译出）。`assets/i18n/zh.json` 是手写基线，**不**走机器翻译、也不受本表约束；本表的词汇对照从 zh.json 与现有 zh 文档的实际用法整理而来。

## 原则

- **语义以 en 为准**；与现有 zh 文档并存时，沿用文档里已有的说法。
- 技术文档语气：准确、简洁、陈述句；称呼读者用「你」，不用「您」。
- 中文与英文字母、数字之间加一个半角空格（`FluxDown 支持 HTTP 下载`、`最多 16 个线程`）。
- 代码、命令、路径、配置键、环境变量、URL、API 名称、JSON 字段不翻译；`{name}` 之类占位符原样保留；Markdown 结构（标题层级、列表、表格、frontmatter 键）不改。
- 中文标点用全角（，。：；？！（））；代码块、行内代码和链接地址里保持半角。

## 不翻译（原样保留）

FluxDown、FluxCloud、BitTorrent、BT、eD2K、aria2、yt-dlp、FFmpeg、HLS、DASH、M3U8、RSS、Webhook、WebDAV、SMB、DHT、PeX、UPnP、uTP、Tracker、Peer、HTTP、HTTPS、FTP、SOCKS5、API、MCP、JSON、CLI、NAS、Docker、Windows、macOS、Linux、Android、iOS、Chrome、Edge、Firefox、Safari、GitHub、User-Agent、Referer、cookie、`fluxdownd`、`fluxdown-agent`、`FLUXDOWN_BIND`、`PUID/PGID`。

## 词汇对照

| en | zh | 备注 |
|---|---|---|
| Task | 任务 | |
| Task group | 任务组 | |
| Queue | 队列 | |
| Category | 分类 | |
| Seeding | 做种 | 状态标签为「做种中」 |
| Share ratio | 分享率 | |
| Torrent file | 种子文件 | |
| Magnet link | 磁力链接 | |
| Plugin | 插件 | |
| Resolver | 解析器 | resolver 插件 |
| Settings | 设置 | 「设置项」指单个选项 |
| Browser extension | 浏览器扩展 | |
| Sniff rules | 嗅探规则 | |
| Headless server | 无头服务器 | |
| Proxy | 代理 | |
| Dedicated proxy | 独立代理 | |
| Direct | 直连 | 代理语境 |
| Segment | 分段 | HTTP / HLS 分段 |
| Threads | 线程 | 「线程数」 |
| Speed limit | 限速 | 「下载限速」 |
| Subscription | 订阅 | |
| Feed | 订阅源 | |
| Notification | 通知 | |
| Tray | 托盘 | |
| Shortcut | 快捷键 | |
| Access key | 访问密钥 | |
| Token | 令牌 | |
| Account | 账户 | |
| Sign in / Sign out | 登录 / 退出登录 | |
| Plan | 套餐 | 云服务套餐 |
| Membership | 会员 | |
| Quota | 额度 | |
| Save location | 保存位置 | |
| Default | 默认 | |
| Advanced | 高级 | |
| Log | 日志 | |
| Diagnostics | 诊断 | |
| Theme / Appearance | 主题 / 外观 | |
| Language | 语言 | |
| Component | 组件 | FFmpeg / yt-dlp 等外部组件 |
| Device | 设备 | |
| Sync | 同步 | |
| Update | 更新 | |
| Completed / Paused / Failed | 已完成 / 已暂停 / 失败 | 状态名 |
| Pause / Resume | 暂停 / 继续 | |
| Cancel / Delete / Remove | 取消 / 删除 / 移除 | |
| File / Folder | 文件 / 文件夹 | |

## qwen-mt 机器翻译配置

`scripts/i18n-mt` 只读取本节的 JSON 代码块；改上文术语时同步这里。

```json
{
  "targetLang": "Chinese",
  "source": "en",
  "domains": "Documentation for FluxDown, a desktop and mobile download manager (HTTP, BitTorrent, eD2K). Simplified Chinese, technical documentation tone; address the reader as 你, never 您. Put a space between Chinese and Latin letters or digits. Do not translate code, commands, file paths, config keys, environment variables, URLs, API names or placeholders such as {name}; keep Markdown structure unchanged. Meaning follows the English source.",
  "keep": [
    "FluxDown", "FluxCloud", "BitTorrent", "BT", "eD2K", "aria2", "yt-dlp", "FFmpeg", "HLS", "DASH", "M3U8", "RSS",
    "Webhook", "WebDAV", "SMB", "DHT", "PeX", "UPnP", "uTP", "Tracker", "Peer", "HTTP", "HTTPS", "FTP", "SOCKS5",
    "API", "MCP", "JSON", "CLI", "NAS", "Docker", "Windows", "macOS", "Linux", "Android", "iOS", "Chrome", "Edge",
    "Firefox", "Safari", "GitHub", "User-Agent", "Referer", "cookie", "fluxdownd", "fluxdown-agent", "FLUXDOWN_BIND",
    "PUID/PGID"
  ],
  "terms": {
    "Task": "任务",
    "Task group": "任务组",
    "Queue": "队列",
    "Category": "分类",
    "Seeding": "做种",
    "Share ratio": "分享率",
    "Torrent file": "种子文件",
    "Magnet link": "磁力链接",
    "Plugin": "插件",
    "Resolver": "解析器",
    "Settings": "设置",
    "Browser extension": "浏览器扩展",
    "Sniff rules": "嗅探规则",
    "Headless server": "无头服务器",
    "Proxy": "代理",
    "Dedicated proxy": "独立代理",
    "Direct": "直连",
    "Segment": "分段",
    "Threads": "线程",
    "Speed limit": "限速",
    "Subscription": "订阅",
    "Feed": "订阅源",
    "Notification": "通知",
    "Tray": "托盘",
    "Shortcut": "快捷键",
    "Access key": "访问密钥",
    "Token": "令牌",
    "Account": "账户",
    "Sign in": "登录",
    "Sign out": "退出登录",
    "Plan": "套餐",
    "Membership": "会员",
    "Quota": "额度",
    "Save location": "保存位置",
    "Default": "默认",
    "Advanced": "高级",
    "Log": "日志",
    "Diagnostics": "诊断",
    "Theme": "主题",
    "Appearance": "外观",
    "Language": "语言",
    "Component": "组件",
    "Device": "设备",
    "Sync": "同步",
    "Update": "更新",
    "Completed": "已完成",
    "Paused": "已暂停",
    "Failed": "失败",
    "Pause": "暂停",
    "Resume": "继续",
    "Cancel": "取消",
    "Delete": "删除",
    "Remove": "移除",
    "File": "文件",
    "Folder": "文件夹"
  },
  "copyKeys": []
}
```
