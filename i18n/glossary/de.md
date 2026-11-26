# de（Deutsch）翻译术语表

GitHub Actions 工作流 `.github/workflows/i18n-mt.yml`（qwen-mt）翻译 `assets/i18n/de.json` 与官网 / 文档时读取本表；人工修正术语时也以本表为准。用词对齐 Windows / Android 与 qBittorrent 该语言译文的通行说法。

## 原则

- **语义以 `en.json` 为准**；`zh.json` 只用于理解语境（en 有歧义时看中文怎么说）。
- 标准德语（德国）；术语对齐 Windows / Android 德语界面与 qBittorrent 德语译文。
- 称呼：**du（非正式）**，与大多数开源应用一致（`du`、`dein`、`Starte…`、`Prüfe…`）；不要出现 `Sie`，同一文本内不混用。
- 按钮、菜单、动作用动词不定式（`Herunterladen`、`Anhalten`、`Löschen`）；说明句用完整句子并以句号结尾，按钮、菜单项、字段名不加句号。
- 德语名词一律大写；标题不做 Title Case，只有句首与名词大写。
- 复合词连写（`Dateiname`、`Speicherort`、`Geschwindigkeitslimit`），不要拆成英语式两词。
- 引号用 `„ “`；省略号用 `…`；小数点用逗号（`1,5 GB`），数值与单位之间留空格；百分号前留空格：`{percent} %`。
- `{name}` 占位符原样保留，可调整语序；不要把占位符放进复合词中间。
- 术语表（terms）里德语名词的英文键一律小写（`task`、`file`），因为脚本在句中出现小写英文词时会把译文首字母降为小写，而德语名词必须保持大写；匹配本身不区分大小写，标签（`Task`）同样命中。动词、形容词类术语（`Pause`、`Default`、`Completed`）保留大写键。
- `{name}` 占位符原样保留（同名、同数量），不翻译、不改名，不引入多余花括号。

## 不翻译（原样保留）

FluxDown、fluxdown_engine、fluxdownd、fluxdown-agent、ZerxLabBot、BitTorrent、BT、eD2K、Kad、aria2、yt-dlp、FFmpeg、HLS、DASH、M3U8、RSS、WebDAV、SMB、DHT、PEX、uTP、UPnP、MSE、HTTP、HTTPS、FTP、SOCKS5、TLS、CDN、LAN、VPN、TUN、Wi-Fi、NMH、ComSpec、%SystemRoot%\system32\cmd.exe、chrome://restart、FLUXDOWN_BIND、PUID/PGID、noexec、loopback、link-local、carrier-grade NAT、HMAC-SHA256、X-FluxDown-Signature、CIDR、IPv4、IPv6、infohash、peer ID、Referer、User-Agent、cookie、ntfy、Bark、WeCom、Feishu、DingTalk、Telegram、Discord、Slack、Gotify、Home Assistant、NAS、Docker、macOS、Windows、Linux、Android、iOS、Chrome、Edge、Firefox、Safari、GitHub、Torrent、Magnet、nodes.dat、ipfilter.dat、server.met、JSON、Markdown、API、MCP、CLI、2xx、3xx、4xx、5xx、Proxy、Host、Webhook、Endpoint、Push。

- `searchKeywords*`、`languageChinese`、`webhookTemplatePlaceholder`、`urlPlaceholder`、`rssProviderBuiltin`、`rssCookiesHint`、`donateDate`、`mobileReferrer`、`apiServiceApi`、各扩展商店名（`extension*Store`）是软件值或搜索索引，原样复制（`copyKeys`）。

## 词汇对照

| en | de | 备注 |
|---|---|---|
| Download | Download | 名词；状态 `Lädt herunter` |
| Downloading | Lädt herunter |  |
| Upload | Upload |  |
| Seeding | Seeding | 沿用 BT 通用说法 `Seeding` |
| Seed | Seed |  |
| Peer | Peer |  |
| Leech | Leecher |  |
| Tracker | Tracker |  |
| Task | Aufgabe |  |
| Task group | Aufgabengruppe |  |
| Queue | Warteschlange |  |
| Category | Kategorie |  |
| File | Datei |  |
| Folder | Ordner |  |
| Path | Pfad |  |
| File name | Dateiname |  |
| Save location | Speicherort |  |
| Settings | Einstellungen |  |
| Default | Standard |  |
| Advanced | Erweitert |  |
| Speed limit | Geschwindigkeitslimit |  |
| Threads | Threads |  |
| Connections | Verbindungen |  |
| Segment | Segment |  |
| Plugin | Plug-in | Duden 写法 `Plug-in` |
| Browser extension | Browsererweiterung |  |
| Component | Komponente |  |
| Device | Gerät |  |
| Account | Konto |  |
| Sign in | Anmelden |  |
| Sign out | Abmelden |  |
| Sync | Synchronisierung |  |
| Notification | Benachrichtigung |  |
| Theme | Design |  |
| Appearance | Darstellung |  |
| Language | Sprache |  |
| Tray | Infobereich | Windows 用语 `Infobereich` |
| Shortcut | Tastenkombination |  |
| Update | Update |  |
| Subscription | Abonnement |  |
| Feed | Feed |  |
| Quota | Kontingent |  |
| Diagnostics | Diagnose |  |
| Log | Protokoll |  |
| Pause | Anhalten | 按钮 `Anhalten`；状态 `Angehalten` |
| Resume | Fortsetzen |  |
| Start | Starten |  |
| Cancel | Abbrechen |  |
| Confirm | Bestätigen |  |
| OK | OK |  |
| Delete | Löschen |  |
| Remove | Entfernen |  |
| Completed | Abgeschlossen |  |
| Failed | Fehlgeschlagen |  |
| Paused | Angehalten |  |
| Direct | Direkt |  |
| Dedicated proxy | Dedizierter Proxy |  |
| Blocklist | Sperrliste |  |
| Allowlist | Zulassungsliste |  |
| Access key | Zugangsschlüssel |  |
| Pairing | Kopplung |  |
| Ratio | Verhältnis |  |

## qwen-mt 机器翻译配置

`scripts/i18n-mt` 只读取本节的 JSON 代码块；改上文术语时同步这里。

```json
{
  "targetLang": "German",
  "source": "en",
  "domains": "UI strings of FluxDown, a desktop and mobile download manager (HTTP, BitTorrent, eD2K). Standard German (Germany), terminology aligned with Windows and Android German UI. Informal du-form (du, dein) in messages and hints, never Sie; buttons and menus in infinitive (Herunterladen, Anhalten, Löschen). Always capitalize German nouns; sentence case, never English-style Title Case. Join compounds (Dateiname, Speicherort). No final period on buttons, menus and labels, full sentences end with a period. Quotes „ “, ellipsis …. Decimal comma (1,5 GB), space between number and unit, percent with space: {percent} %. Keep {placeholder} names unchanged.",
  "siteDomains": "Website and documentation of FluxDown, a free open-source download manager (HTTP, BitTorrent, eD2K) for desktop, mobile, NAS and servers. Standard German (Germany), clear and natural marketing / technical documentation tone; address the reader with informal du, never Sie. Capitalize nouns, sentence case for headings, join compounds. Do not translate code, commands, file paths, config keys, environment variables, URLs, product names or placeholders such as {0} or {name}; keep HTML tags and Markdown markup unchanged.",
  "keep": [
    "FluxDown", "fluxdown_engine", "fluxdownd", "fluxdown-agent", "ZerxLabBot", "BitTorrent", "BT", "eD2K", "Kad", "aria2", "yt-dlp", "FFmpeg",
    "HLS", "DASH", "M3U8", "RSS", "WebDAV", "SMB", "DHT", "PEX", "uTP", "UPnP", "MSE", "HTTP",
    "HTTPS", "FTP", "SOCKS5", "TLS", "CDN", "LAN", "VPN", "TUN", "Wi-Fi", "NMH", "ComSpec", "%SystemRoot%\\system32\\cmd.exe",
    "chrome://restart", "FLUXDOWN_BIND", "PUID/PGID", "noexec", "loopback", "link-local", "carrier-grade NAT", "HMAC-SHA256", "X-FluxDown-Signature", "CIDR", "IPv4", "IPv6",
    "infohash", "peer ID", "Referer", "User-Agent", "cookie", "ntfy", "Bark", "WeCom", "Feishu", "DingTalk", "Telegram", "Discord",
    "Slack", "Gotify", "Home Assistant", "NAS", "Docker", "macOS", "Windows", "Linux", "Android", "iOS", "Chrome", "Edge",
    "Firefox", "Safari", "GitHub", "Torrent", "Magnet", "nodes.dat", "ipfilter.dat", "server.met", "JSON", "Markdown", "API", "MCP",
    "CLI", "2xx", "3xx", "4xx", "5xx", "Proxy", "Host", "Webhook", "Endpoint", "Push"
  ],
  "terms": {
    "download": "Download",
    "Downloading": "Lädt herunter",
    "upload": "Upload",
    "seeding": "Seeding",
    "seed": "Seed",
    "peer": "Peer",
    "leech": "Leecher",
    "tracker": "Tracker",
    "task": "Aufgabe",
    "task group": "Aufgabengruppe",
    "queue": "Warteschlange",
    "category": "Kategorie",
    "file": "Datei",
    "folder": "Ordner",
    "path": "Pfad",
    "file name": "Dateiname",
    "save location": "Speicherort",
    "settings": "Einstellungen",
    "Default": "Standard",
    "Advanced": "Erweitert",
    "speed limit": "Geschwindigkeitslimit",
    "threads": "Threads",
    "connections": "Verbindungen",
    "segment": "Segment",
    "plugin": "Plug-in",
    "browser extension": "Browsererweiterung",
    "component": "Komponente",
    "device": "Gerät",
    "account": "Konto",
    "Sign in": "Anmelden",
    "Sign out": "Abmelden",
    "sync": "Synchronisierung",
    "notification": "Benachrichtigung",
    "theme": "Design",
    "appearance": "Darstellung",
    "language": "Sprache",
    "tray": "Infobereich",
    "shortcut": "Tastenkombination",
    "update": "Update",
    "subscription": "Abonnement",
    "feed": "Feed",
    "quota": "Kontingent",
    "diagnostics": "Diagnose",
    "log": "Protokoll",
    "Pause": "Anhalten",
    "Resume": "Fortsetzen",
    "Start": "Starten",
    "Cancel": "Abbrechen",
    "Confirm": "Bestätigen",
    "OK": "OK",
    "Delete": "Löschen",
    "Remove": "Entfernen",
    "Completed": "Abgeschlossen",
    "Failed": "Fehlgeschlagen",
    "Paused": "Angehalten",
    "Direct": "Direkt",
    "Dedicated proxy": "Dedizierter Proxy",
    "blocklist": "Sperrliste",
    "allowlist": "Zulassungsliste",
    "access key": "Zugangsschlüssel",
    "pairing": "Kopplung",
    "ratio": "Verhältnis"
  },
  "copyKeys": [
    "searchKeywords*", "languageChinese", "webhookTemplatePlaceholder", "urlPlaceholder", "rssProviderBuiltin",
    "rssCookiesHint", "donateDate", "mobileReferrer", "apiServiceApi", "extension*Store"
  ]
}
```
