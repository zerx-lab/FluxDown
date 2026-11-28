# ja（日本語）翻译术语表

GitHub Actions 工作流 `.github/workflows/i18n-mt.yml`（qwen-mt）翻译 `assets/i18n/ja.json` 与官网 / 文档时读取本表；人工修正术语时也以本表为准。用词对齐 Windows / Android 与 qBittorrent 该语言译文的通行说法。

## 原则

- **语义以 `en.json` 为准**；`zh.json` 只用于理解语境（en 有歧义时看中文怎么说）。
- 标准日语（日本）；术语对齐 Microsoft Windows / Android 日语界面与 qBittorrent 日语译文。长音符号不省略（`フォルダー`、`ブラウザー`、`ユーザー`）。
- 语气：说明与提示句用「です・ます調」；按钮、菜单、标签用名词或动词终止形（`保存`、`削除`、`ダウンロード`），不用「〜してください」命令式堆叠。
- 标点用全角：`。`、`、`；句末加 `。`，按钮、菜单项、字段名不加句号；引号用 `「 」`，省略号用 `…`。
- 日文与半角字母、数字之间不加空格（写 `FluxDownを`，不写 `FluxDown を`）；数值与单位之间留一个半角空格（`1.5 GB`）。
- 数字用半角、小数点用 `.`；百分号紧跟数值：`{percent}%`。
- 无大小写问题，不要为「句首大写」加任何处理；英文原文里的 Title Case 标题直接翻成自然的日语名词短语。
- `{name}` 占位符原样保留（同名、同数量），不翻译、不改名，不引入多余花括号。

## 不翻译（原样保留）

FluxDown、fluxdown_engine、fluxdownd、fluxdown-agent、ZerxLabBot、BitTorrent、BT、eD2K、Kad、aria2、yt-dlp、FFmpeg、HLS、DASH、M3U8、RSS、WebDAV、SMB、DHT、PEX、uTP、UPnP、MSE、HTTP、HTTPS、FTP、SOCKS5、TLS、CDN、LAN、VPN、TUN、Wi-Fi、NMH、ComSpec、%SystemRoot%\system32\cmd.exe、chrome://restart、FLUXDOWN_BIND、PUID/PGID、noexec、loopback、link-local、carrier-grade NAT、HMAC-SHA256、X-FluxDown-Signature、CIDR、IPv4、IPv6、infohash、peer ID、Referer、User-Agent、cookie、ntfy、Bark、WeCom、Feishu、DingTalk、Telegram、Discord、Slack、Gotify、Home Assistant、NAS、Docker、macOS、Windows、Linux、Android、iOS、Chrome、Edge、Firefox、Safari、GitHub、Torrent、Magnet、nodes.dat、ipfilter.dat、server.met、JSON、Markdown、API、MCP、CLI、2xx、3xx、4xx、5xx、Webhook。

- `searchKeywords*`、`languageChinese`、`webhookTemplatePlaceholder`、`urlPlaceholder`、`rssProviderBuiltin`、`rssCookiesHint`、`donateDate`、`mobileReferrer`、`apiServiceApi`、各扩展商店名（`extension*Store`）是软件值或搜索索引，原样复制（`copyKeys`）。

## 词汇对照

| en | ja | 备注 |
|---|---|---|
| Download | ダウンロード |  |
| Downloading | ダウンロード中 |  |
| Upload | アップロード |  |
| Seeding | シード中 | 与「做种中」侧栏过滤器一致 |
| Seed | シード |  |
| Peer | ピア |  |
| Leech | リーチャー |  |
| Tracker | トラッカー |  |
| Task | タスク |  |
| Task group | タスクグループ |  |
| Queue | キュー |  |
| Category | カテゴリ |  |
| File | ファイル |  |
| Folder | フォルダー |  |
| Path | パス |  |
| File name | ファイル名 |  |
| Save location | 保存先 |  |
| Settings | 設定 |  |
| Default | 既定 | Microsoft 用语 `既定` |
| Advanced | 詳細設定 |  |
| Speed limit | 速度制限 |  |
| Threads | スレッド |  |
| Connections | 接続 |  |
| Segment | セグメント |  |
| Plugin | プラグイン |  |
| Browser extension | ブラウザー拡張機能 |  |
| Component | コンポーネント |  |
| Device | デバイス |  |
| Account | アカウント |  |
| Sign in | サインイン |  |
| Sign out | サインアウト |  |
| Sync | 同期 |  |
| Notification | 通知 |  |
| Theme | テーマ |  |
| Appearance | 外観 |  |
| Language | 言語 |  |
| Tray | システムトレイ |  |
| Shortcut | ショートカット |  |
| Update | 更新 |  |
| Subscription | 購読 |  |
| Feed | フィード |  |
| Quota | クォータ |  |
| Diagnostics | 診断 |  |
| Log | ログ |  |
| Pause | 一時停止 |  |
| Resume | 再開 |  |
| Start | 開始 |  |
| Cancel | キャンセル |  |
| Confirm | 確認 |  |
| OK | OK |  |
| Delete | 削除 | 与 Remove 同为 `削除`，靠语境区分 |
| Remove | 削除 | 从列表移除可译 `リストから削除` |
| Completed | 完了 |  |
| Failed | 失敗 |  |
| Paused | 一時停止中 |  |
| Direct | 直接 |  |
| Dedicated proxy | 専用プロキシ |  |
| Blocklist | ブロックリスト |  |
| Allowlist | 許可リスト |  |
| Access key | アクセスキー |  |
| Pairing | ペアリング |  |
| Ratio | 共有率 |  |
| Proxy | プロキシ | 日语写 `プロキシ` |
| Host | ホスト |  |
| Push | プッシュ |  |
| Endpoint | エンドポイント |  |

## qwen-mt 机器翻译配置

`scripts/i18n-mt` 只读取本节的 JSON 代码块；改上文术语时同步这里。

```json
{
  "targetLang": "Japanese",
  "source": "en",
  "domains": "UI strings of FluxDown, a desktop and mobile download manager (HTTP, BitTorrent, eD2K). Natural Japanese for Japan, terminology aligned with Windows and Android Japanese UI. Polite です・ます style for messages and hints; buttons, menus and labels in plain noun or dictionary form (保存, 削除, ダウンロード), no final period on labels, full sentences end with 。. Full-width punctuation 、。, quotes 「」, ellipsis …. Keep the long vowel mark (フォルダー, ブラウザー). No space between Japanese text and Latin letters or digits; one space between number and unit (1.5 GB); percent attached: {percent}%. Keep {placeholder} names unchanged.",
  "siteDomains": "Website and documentation of FluxDown, a free open-source download manager (HTTP, BitTorrent, eD2K) for desktop, mobile, NAS and servers. Natural Japanese for Japan in clear marketing / technical documentation tone; polite です・ます style, no overly casual or overly honorific wording. Full-width punctuation 、。, no space between Japanese text and Latin letters or digits. Do not translate code, commands, file paths, config keys, environment variables, URLs, product names or placeholders such as {0} or {name}; keep HTML tags and Markdown markup unchanged.",
  "keep": [
    "FluxDown", "fluxdown_engine", "fluxdownd", "fluxdown-agent", "ZerxLabBot", "BitTorrent", "BT", "eD2K", "Kad", "aria2", "yt-dlp", "FFmpeg",
    "HLS", "DASH", "M3U8", "RSS", "WebDAV", "SMB", "DHT", "PEX", "uTP", "UPnP", "MSE", "HTTP",
    "HTTPS", "FTP", "SOCKS5", "TLS", "CDN", "LAN", "VPN", "TUN", "Wi-Fi", "NMH", "ComSpec", "%SystemRoot%\\system32\\cmd.exe",
    "chrome://restart", "FLUXDOWN_BIND", "PUID/PGID", "noexec", "loopback", "link-local", "carrier-grade NAT", "HMAC-SHA256", "X-FluxDown-Signature", "CIDR", "IPv4", "IPv6",
    "infohash", "peer ID", "Referer", "User-Agent", "cookie", "ntfy", "Bark", "WeCom", "Feishu", "DingTalk", "Telegram", "Discord",
    "Slack", "Gotify", "Home Assistant", "NAS", "Docker", "macOS", "Windows", "Linux", "Android", "iOS", "Chrome", "Edge",
    "Firefox", "Safari", "GitHub", "Torrent", "Magnet", "nodes.dat", "ipfilter.dat", "server.met", "JSON", "Markdown", "API", "MCP",
    "CLI", "2xx", "3xx", "4xx", "5xx", "Webhook"
  ],
  "terms": {
    "Download": "ダウンロード",
    "Downloading": "ダウンロード中",
    "Upload": "アップロード",
    "Seeding": "シード中",
    "Seed": "シード",
    "Peer": "ピア",
    "Leech": "リーチャー",
    "Tracker": "トラッカー",
    "Task": "タスク",
    "Task group": "タスクグループ",
    "Queue": "キュー",
    "Category": "カテゴリ",
    "File": "ファイル",
    "Folder": "フォルダー",
    "Path": "パス",
    "File name": "ファイル名",
    "Save location": "保存先",
    "Settings": "設定",
    "Default": "既定",
    "Advanced": "詳細設定",
    "Speed limit": "速度制限",
    "Threads": "スレッド",
    "Connections": "接続",
    "Segment": "セグメント",
    "Plugin": "プラグイン",
    "Browser extension": "ブラウザー拡張機能",
    "Component": "コンポーネント",
    "Device": "デバイス",
    "Account": "アカウント",
    "Sign in": "サインイン",
    "Sign out": "サインアウト",
    "Sync": "同期",
    "Notification": "通知",
    "Theme": "テーマ",
    "Appearance": "外観",
    "Language": "言語",
    "Tray": "システムトレイ",
    "Shortcut": "ショートカット",
    "Update": "更新",
    "Subscription": "購読",
    "Feed": "フィード",
    "Quota": "クォータ",
    "Diagnostics": "診断",
    "Log": "ログ",
    "Pause": "一時停止",
    "Resume": "再開",
    "Start": "開始",
    "Cancel": "キャンセル",
    "Confirm": "確認",
    "OK": "OK",
    "Delete": "削除",
    "Remove": "削除",
    "Completed": "完了",
    "Failed": "失敗",
    "Paused": "一時停止中",
    "Direct": "直接",
    "Dedicated proxy": "専用プロキシ",
    "Blocklist": "ブロックリスト",
    "Allowlist": "許可リスト",
    "Access key": "アクセスキー",
    "Pairing": "ペアリング",
    "Ratio": "共有率",
    "Proxy": "プロキシ",
    "Host": "ホスト",
    "Push": "プッシュ",
    "Endpoint": "エンドポイント"
  },
  "copyKeys": [
    "searchKeywords*", "languageChinese", "webhookTemplatePlaceholder", "urlPlaceholder", "rssProviderBuiltin",
    "rssCookiesHint", "donateDate", "mobileReferrer", "apiServiceApi", "extension*Store"
  ]
}
```
