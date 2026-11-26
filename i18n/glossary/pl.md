# pl（Polski）翻译术语表

GitHub Actions 工作流 `.github/workflows/i18n-mt.yml`（qwen-mt）翻译 `assets/i18n/pl.json` 时读取本表；人工修正术语时也以本表为准。用词对齐 Windows / Android 该语言界面与 qBittorrent 译文的通行说法。

## 原则

- **语义以 `en.json` 为准**；`zh.json` 只用于理解语境（en 有歧义时看中文怎么说）。
- 标准波兰语，正确使用 ą ć ę ł ń ó ś ź ż；不要丢掉变音符号。
- 语气：按钮、菜单用第二人称单数命令式（`Pobierz`、`Wstrzymaj`、`Usuń`），与 Windows 波兰语界面一致；说明与提示句用无人称或隐含的「你」（`Możesz…`、`Sprawdź…`），不用 `Pan/Pani`，同一段文字不混用。
- 标题与按钮用句首大写（Sentence case）；按钮、菜单项、字段名不加句号，完整句子加句号。
- 数字与单位：小数点用逗号（`1,5 GB`），数值与单位之间留空格；百分号紧贴数字：`{percent}%`。
- 名词在 `{count}` 之后按波兰语复数规则变化（1 / 2–4 / 5+ 三种形式），不要硬套单数。
- 引号用 `„ ”`；省略号用 `…`。
- `{name}` 占位符原样保留，可调整语序，不翻译、不改名、不新增花括号。

## 不翻译（原样保留）

FluxDown, `fluxdown_engine`, `fluxdownd`, `fluxdown-agent`, ZerxLabBot, BitTorrent, **BT**, eD2K, Kad, aria2, yt-dlp, FFmpeg, HLS, DASH, M3U8, RSS, WebDAV, SMB, DHT, PEX, uTP, UPnP, **MSE**, HTTP(S), FTP, SOCKS5, TLS, CDN, LAN, VPN, TUN, Wi-Fi, **NMH** (native messaging host, no desarrollar la sigla), **ComSpec**, `%SystemRoot%\system32\cmd.exe`, `chrome://restart`, `FLUXDOWN_BIND`, `PUID/PGID`, `noexec`, `loopback`, `link-local`, `carrier-grade NAT`, HMAC-SHA256, `X-FluxDown-Signature`, CIDR, IPv4/IPv6, infohash, peer ID, Referer, **User-Agent**, cookie, ntfy, Bark, WeCom, Feishu, DingTalk, Telegram, Discord, Slack, Gotify, Home Assistant, NAS, Docker, macOS, Windows, Linux, Android, iOS, Chrome, Edge, Firefox, Safari, GitHub, Torrent, Magnet, `nodes.dat`, `ipfilter.dat`, `server.met`, JSON, Markdown, API, MCP, CLI, 2xx/3xx/4xx/5xx.

- `searchKeywords*`、`languageChinese`、`webhookTemplatePlaceholder`、`urlPlaceholder`、`rssProviderBuiltin`、`rssCookiesHint`、`donateDate`、`mobileReferrer`、`apiServiceApi`、`extension*Store` 按 `copyKeys` 原样复制。
- `Proxy`、`Host`、`Webhook`、`Endpoint`、`Push`、`Ratio` 在 `keep` 中，保持拉丁原文。

## 词汇对照

| en | pl | 备注 |
|---|---|---|
| Download | Pobieranie | 动词按钮 `Pobierz`；状态 `Pobieranie` |
| Upload | Wysyłanie | 动词 `Wyślij` |
| Seeding | Seedowanie | 与「做种中」过滤器一致 |
| Seed | Seed |  |
| Peer | Peer |  |
| Tracker | Tracker |  |
| Completed | Ukończono | 状态名 |
| Failed | Niepowodzenie | 状态名 |
| Paused | Wstrzymano | 状态名 |
| Pause | Wstrzymaj |  |
| Resume | Wznów |  |
| Start | Uruchom |  |
| Cancel | Anuluj |  |
| Confirm | Potwierdź |  |
| OK | OK |  |
| Delete | Usuń |  |
| Remove | Usuń |  |
| Task | Zadanie |  |
| Task group | Grupa zadań |  |
| Queue | Kolejka |  |
| Category | Kategoria |  |
| File | Plik |  |
| Folder | Folder |  |
| Path | Ścieżka |  |
| File name | Nazwa pliku |  |
| Save location | Lokalizacja zapisu |  |
| Settings | Ustawienia |  |
| Default | Domyślne |  |
| Advanced | Zaawansowane |  |
| Speed limit | Limit prędkości |  |
| Threads | Wątki |  |
| Connections | Połączenia |  |
| Segment | Segment |  |
| Direct | Bezpośrednio |  |
| Dedicated proxy | Dedykowany Proxy | Proxy 保持原样 |
| Plugin | Wtyczka |  |
| Browser extension | Rozszerzenie przeglądarki |  |
| Component | Składnik |  |
| Device | Urządzenie |  |
| Account | Konto |  |
| Sign in | Zaloguj się |  |
| Sign out | Wyloguj się |  |
| Sync | Synchronizacja |  |
| Notification | Powiadomienie |  |
| Theme | Motyw |  |
| Appearance | Wygląd |  |
| Language | Język |  |
| Tray | Zasobnik systemowy |  |
| Shortcut | Skrót |  |
| Update | Aktualizacja |  |
| Subscription | Subskrypcja | RSS 订阅 |
| Feed | Kanał |  |
| Quota | Przydział |  |
| Diagnostics | Diagnostyka |  |
| Log | Dziennik |  |
| Blocklist | Lista blokad |  |
| Allowlist | Lista dozwolonych |  |

## qwen-mt 机器翻译配置

`scripts/i18n-mt` 只读取本节的 JSON 代码块；改上文术语时同步这里。

```json
{
  "targetLang": "Polish",
  "source": "en",
  "domains": "UI strings of FluxDown, a desktop and mobile download manager (HTTP, BitTorrent, eD2K). Standard Polish with correct diacritics (ą ć ę ł ń ó ś ź ż). Buttons and menus in informal imperative, second person singular (Pobierz, Wstrzymaj, Usuń) as in Windows Polish; hints impersonal or addressed informally (Możesz…), never formal 'Pan/Pani'; do not mix registers. Sentence case; no final period on buttons, menus and labels, full sentences end with a period. Quotes „ ”, ellipsis …. Decimal comma (1,5 GB), space between number and unit, percent attached: {percent}%. Use correct Polish plural forms after counts. Keep {placeholder} names unchanged.",
  "siteDomains": "Website and documentation of FluxDown, a free open-source download manager (HTTP, BitTorrent, eD2K) for desktop, mobile, NAS and servers. Standard Polish with correct diacritics; clear and natural marketing / technical documentation tone; address the reader informally in second person singular, never 'Pan/Pani'. Sentence case for headings. Do not translate code, commands, file paths, config keys, environment variables, URLs, product names or placeholders such as {0} or {name}; keep HTML tags and Markdown markup unchanged.",
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
    "Download": "Pobieranie",
    "Upload": "Wysyłanie",
    "Seeding": "Seedowanie",
    "Seed": "Seed",
    "Peer": "Peer",
    "Tracker": "Tracker",
    "Completed": "Ukończono",
    "Failed": "Niepowodzenie",
    "Paused": "Wstrzymano",
    "Pause": "Wstrzymaj",
    "Resume": "Wznów",
    "Start": "Uruchom",
    "Cancel": "Anuluj",
    "Confirm": "Potwierdź",
    "OK": "OK",
    "Delete": "Usuń",
    "Remove": "Usuń",
    "Task": "Zadanie",
    "Task group": "Grupa zadań",
    "Queue": "Kolejka",
    "Category": "Kategoria",
    "File": "Plik",
    "Folder": "Folder",
    "Path": "Ścieżka",
    "File name": "Nazwa pliku",
    "Save location": "Lokalizacja zapisu",
    "Settings": "Ustawienia",
    "Default": "Domyślne",
    "Advanced": "Zaawansowane",
    "Speed limit": "Limit prędkości",
    "Threads": "Wątki",
    "Connections": "Połączenia",
    "Segment": "Segment",
    "Direct": "Bezpośrednio",
    "Dedicated proxy": "Dedykowany Proxy",
    "Plugin": "Wtyczka",
    "Browser extension": "Rozszerzenie przeglądarki",
    "Component": "Składnik",
    "Device": "Urządzenie",
    "Account": "Konto",
    "Sign in": "Zaloguj się",
    "Sign out": "Wyloguj się",
    "Sync": "Synchronizacja",
    "Notification": "Powiadomienie",
    "Theme": "Motyw",
    "Appearance": "Wygląd",
    "Language": "Język",
    "Tray": "Zasobnik systemowy",
    "Shortcut": "Skrót",
    "Update": "Aktualizacja",
    "Subscription": "Subskrypcja",
    "Feed": "Kanał",
    "Quota": "Przydział",
    "Diagnostics": "Diagnostyka",
    "Log": "Dziennik",
    "Blocklist": "Lista blokad",
    "Allowlist": "Lista dozwolonych"
  },
  "copyKeys": [
    "searchKeywords*", "languageChinese", "webhookTemplatePlaceholder", "urlPlaceholder", "rssProviderBuiltin",
    "rssCookiesHint", "donateDate", "mobileReferrer", "apiServiceApi", "extension*Store"
  ]
}
```
