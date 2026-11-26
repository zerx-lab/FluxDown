# nl（Nederlands）翻译术语表

GitHub Actions 工作流 `.github/workflows/i18n-mt.yml`（qwen-mt）翻译 `assets/i18n/nl.json` 时读取本表；人工修正术语时也以本表为准。用词对齐 Windows / Android 该语言界面与 qBittorrent 译文的通行说法。

## 原则

- **语义以 `en.json` 为准**；`zh.json` 只用于理解语境（en 有歧义时看中文怎么说）。
- 标准荷兰语（Nederland），用词对齐 Windows / Android 荷兰语界面与 qBittorrent 荷兰语译文。
- 语气：称呼用 `je` / `jouw`（非正式），不用 `u`，同一段文字不混用；按钮、菜单用动词不定式（`Downloaden`、`Pauzeren`、`Verwijderen`）。
- 标题与按钮用句首大写（Sentence case）；按钮、菜单项、字段名不加句号，完整句子加句号。
- 数字与单位：小数点用逗号（`1,5 GB`），数值与单位之间留空格；百分号紧贴数字：`{percent}%`。
- 复合词连写（`Bestandsnaam`、`Taakgroep`），不要拆成两个词；`{count}` 后名词按复数形式（`{count} bestanden`）。
- 引号用 `“ ”`；省略号用 `…`。
- `{name}` 占位符原样保留，可调整语序，不翻译、不改名、不新增花括号。

## 不翻译（原样保留）

FluxDown, `fluxdown_engine`, `fluxdownd`, `fluxdown-agent`, ZerxLabBot, BitTorrent, **BT**, eD2K, Kad, aria2, yt-dlp, FFmpeg, HLS, DASH, M3U8, RSS, WebDAV, SMB, DHT, PEX, uTP, UPnP, **MSE**, HTTP(S), FTP, SOCKS5, TLS, CDN, LAN, VPN, TUN, Wi-Fi, **NMH** (native messaging host, no desarrollar la sigla), **ComSpec**, `%SystemRoot%\system32\cmd.exe`, `chrome://restart`, `FLUXDOWN_BIND`, `PUID/PGID`, `noexec`, `loopback`, `link-local`, `carrier-grade NAT`, HMAC-SHA256, `X-FluxDown-Signature`, CIDR, IPv4/IPv6, infohash, peer ID, Referer, **User-Agent**, cookie, ntfy, Bark, WeCom, Feishu, DingTalk, Telegram, Discord, Slack, Gotify, Home Assistant, NAS, Docker, macOS, Windows, Linux, Android, iOS, Chrome, Edge, Firefox, Safari, GitHub, Torrent, Magnet, `nodes.dat`, `ipfilter.dat`, `server.met`, JSON, Markdown, API, MCP, CLI, 2xx/3xx/4xx/5xx.

- `searchKeywords*`、`languageChinese`、`webhookTemplatePlaceholder`、`urlPlaceholder`、`rssProviderBuiltin`、`rssCookiesHint`、`donateDate`、`mobileReferrer`、`apiServiceApi`、`extension*Store` 按 `copyKeys` 原样复制。
- `Proxy`、`Host`、`Webhook`、`Endpoint`、`Push`、`Ratio` 在 `keep` 中，保持拉丁原文。

## 词汇对照

| en | nl | 备注 |
|---|---|---|
| Download | Download | 动词按钮 `Downloaden`；状态 `Downloaden` |
| Upload | Upload | 动词 `Uploaden` |
| Seeding | Seeden | 与「做种中」过滤器一致 |
| Seed | Seed |  |
| Peer | Peer |  |
| Tracker | Tracker |  |
| Completed | Voltooid | 状态名 |
| Failed | Mislukt | 状态名 |
| Paused | Gepauzeerd | 状态名 |
| Pause | Pauzeren |  |
| Resume | Hervatten |  |
| Start | Starten |  |
| Cancel | Annuleren |  |
| Confirm | Bevestigen |  |
| OK | OK |  |
| Delete | Verwijderen |  |
| Remove | Verwijderen |  |
| Task | Taak |  |
| Task group | Taakgroep |  |
| Queue | Wachtrij |  |
| Category | Categorie |  |
| File | Bestand |  |
| Folder | Map |  |
| Path | Pad |  |
| File name | Bestandsnaam |  |
| Save location | Opslaglocatie |  |
| Settings | Instellingen |  |
| Default | Standaard |  |
| Advanced | Geavanceerd |  |
| Speed limit | Snelheidslimiet |  |
| Threads | Threads |  |
| Connections | Verbindingen |  |
| Segment | Segment |  |
| Direct | Direct |  |
| Dedicated proxy | Aparte Proxy | Proxy 保持原样 |
| Plugin | Plug-in |  |
| Browser extension | Browserextensie |  |
| Component | Onderdeel |  |
| Device | Apparaat |  |
| Account | Account |  |
| Sign in | Aanmelden |  |
| Sign out | Afmelden |  |
| Sync | Synchronisatie |  |
| Notification | Melding |  |
| Theme | Thema |  |
| Appearance | Uiterlijk |  |
| Language | Taal |  |
| Tray | Systeemvak |  |
| Shortcut | Sneltoets |  |
| Update | Update |  |
| Subscription | Abonnement | RSS 订阅 |
| Feed | Feed |  |
| Quota | Quotum |  |
| Diagnostics | Diagnostiek |  |
| Log | Logboek |  |
| Blocklist | Blokkeerlijst |  |
| Allowlist | Lijst met toegestane items |  |

## qwen-mt 机器翻译配置

`scripts/i18n-mt` 只读取本节的 JSON 代码块；改上文术语时同步这里。

```json
{
  "targetLang": "Dutch",
  "source": "en",
  "domains": "UI strings of FluxDown, a desktop and mobile download manager (HTTP, BitTorrent, eD2K). Standard Dutch (Netherlands), following Windows and Android Dutch conventions. Informal 'je/jouw', never formal 'u'; do not mix. Buttons and menus in infinitive (Downloaden, Pauzeren, Verwijderen). Sentence case; no final period on buttons, menus and labels, full sentences end with a period. Write compound nouns as one word (Bestandsnaam, Taakgroep). Quotes “ ”, ellipsis …. Decimal comma (1,5 GB), space between number and unit, percent attached: {percent}%. Keep {placeholder} names unchanged.",
  "siteDomains": "Website and documentation of FluxDown, a free open-source download manager (HTTP, BitTorrent, eD2K) for desktop, mobile, NAS and servers. Standard Dutch (Netherlands); clear and natural marketing / technical documentation tone; address the reader informally as 'je', never 'u'. Sentence case for headings. Write compound nouns as one word. Do not translate code, commands, file paths, config keys, environment variables, URLs, product names or placeholders such as {0} or {name}; keep HTML tags and Markdown markup unchanged.",
  "keep": [
    "FluxDown", "fluxdown_engine", "fluxdownd", "fluxdown-agent", "ZerxLabBot", "BitTorrent", "BT", "eD2K", "Kad",
    "aria2", "yt-dlp", "FFmpeg", "HLS", "DASH", "M3U8", "RSS", "WebDAV", "SMB", "DHT", "PEX", "uTP", "UPnP", "MSE",
    "HTTP", "HTTPS", "FTP", "SOCKS5", "TLS", "CDN", "LAN", "VPN", "TUN", "Wi-Fi", "NMH", "ComSpec",
    "%SystemRoot%\\system32\\cmd.exe", "chrome://restart", "FLUXDOWN_BIND", "PUID/PGID", "noexec", "loopback",
    "link-local", "carrier-grade NAT", "HMAC-SHA256", "X-FluxDown-Signature", "CIDR", "IPv4", "IPv6", "infohash",
    "peer ID", "Referer", "User-Agent", "cookie", "ntfy", "Bark", "WeCom", "Feishu", "DingTalk", "Telegram",
    "Discord", "Slack", "Gotify", "Home Assistant", "NAS", "Docker", "macOS", "Windows", "Linux", "Android", "iOS",
    "Chrome", "Edge", "Firefox", "Safari", "GitHub", "Torrent", "Magnet", "nodes.dat", "ipfilter.dat", "server.met",
    "JSON", "Markdown", "API", "MCP", "CLI", "2xx", "3xx", "4xx", "5xx",
    "Proxy", "Host", "Webhook", "Endpoint", "Push", "Ratio"
  ],
  "terms": {
    "Download": "Download",
    "Upload": "Upload",
    "Seeding": "Seeden",
    "Seed": "Seed",
    "Peer": "Peer",
    "Tracker": "Tracker",
    "Completed": "Voltooid",
    "Failed": "Mislukt",
    "Paused": "Gepauzeerd",
    "Pause": "Pauzeren",
    "Resume": "Hervatten",
    "Start": "Starten",
    "Cancel": "Annuleren",
    "Confirm": "Bevestigen",
    "OK": "OK",
    "Delete": "Verwijderen",
    "Remove": "Verwijderen",
    "Task": "Taak",
    "Task group": "Taakgroep",
    "Queue": "Wachtrij",
    "Category": "Categorie",
    "File": "Bestand",
    "Folder": "Map",
    "Path": "Pad",
    "File name": "Bestandsnaam",
    "Save location": "Opslaglocatie",
    "Settings": "Instellingen",
    "Default": "Standaard",
    "Advanced": "Geavanceerd",
    "Speed limit": "Snelheidslimiet",
    "Threads": "Threads",
    "Connections": "Verbindingen",
    "Segment": "Segment",
    "Direct": "Direct",
    "Dedicated proxy": "Aparte Proxy",
    "Plugin": "Plug-in",
    "Browser extension": "Browserextensie",
    "Component": "Onderdeel",
    "Device": "Apparaat",
    "Account": "Account",
    "Sign in": "Aanmelden",
    "Sign out": "Afmelden",
    "Sync": "Synchronisatie",
    "Notification": "Melding",
    "Theme": "Thema",
    "Appearance": "Uiterlijk",
    "Language": "Taal",
    "Tray": "Systeemvak",
    "Shortcut": "Sneltoets",
    "Update": "Update",
    "Subscription": "Abonnement",
    "Feed": "Feed",
    "Quota": "Quotum",
    "Diagnostics": "Diagnostiek",
    "Log": "Logboek",
    "Blocklist": "Blokkeerlijst",
    "Allowlist": "Lijst met toegestane items"
  },
  "copyKeys": [
    "searchKeywords*", "languageChinese", "webhookTemplatePlaceholder", "urlPlaceholder", "rssProviderBuiltin",
    "rssCookiesHint", "donateDate", "mobileReferrer", "apiServiceApi", "extension*Store"
  ]
}
```
