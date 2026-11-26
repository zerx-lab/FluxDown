# uk（Українська）翻译术语表

GitHub Actions 工作流 `.github/workflows/i18n-mt.yml`（qwen-mt）翻译 `assets/i18n/uk.json` 时读取本表；人工修正术语时也以本表为准。用词对齐 Windows / Android 该语言界面与 qBittorrent 译文的通行说法。

## 原则

- **语义以 `en.json` 为准**；`zh.json` 只用于理解语境（en 有歧义时看中文怎么说）。
- 标准乌克兰语，用词对齐 Windows / Android 乌克兰语界面与 qBittorrent 乌克兰语译文；不要用俄语借词，不要出现俄语字母 ы э ъ ё。
- 语气：称呼用 `ви`（小写）；按钮、菜单用动词不定式（`Завантажити`、`Призупинити`、`Видалити`），与 Windows 乌克兰语界面一致；不混用 `ти`。
- 标题与按钮用句首大写（Sentence case）；按钮、菜单项、字段名不加句号，完整句子加句号。
- 数字与单位：小数点用逗号（`1,5 ГБ` 风格；单位 `KB/s`、`MB/s`、`GB` 等保持拉丁原样），数值与单位之间留空格；百分号前留空格：`{percent} %`。
- 名词在 `{count}` 之后按乌克兰语复数规则变化（1 / 2–4 / 5+ 三种形式）。
- 引号用 `« »`；省略号用 `…`；撇号统一用 `’`（U+2019），如 `з’єднання`。
- `{name}` 占位符原样保留，可调整语序，不翻译、不改名、不新增花括号。

## 不翻译（原样保留）

FluxDown, `fluxdown_engine`, `fluxdownd`, `fluxdown-agent`, ZerxLabBot, BitTorrent, **BT**, eD2K, Kad, aria2, yt-dlp, FFmpeg, HLS, DASH, M3U8, RSS, WebDAV, SMB, DHT, PEX, uTP, UPnP, **MSE**, HTTP(S), FTP, SOCKS5, TLS, CDN, LAN, VPN, TUN, Wi-Fi, **NMH** (native messaging host, no desarrollar la sigla), **ComSpec**, `%SystemRoot%\system32\cmd.exe`, `chrome://restart`, `FLUXDOWN_BIND`, `PUID/PGID`, `noexec`, `loopback`, `link-local`, `carrier-grade NAT`, HMAC-SHA256, `X-FluxDown-Signature`, CIDR, IPv4/IPv6, infohash, peer ID, Referer, **User-Agent**, cookie, ntfy, Bark, WeCom, Feishu, DingTalk, Telegram, Discord, Slack, Gotify, Home Assistant, NAS, Docker, macOS, Windows, Linux, Android, iOS, Chrome, Edge, Firefox, Safari, GitHub, Torrent, Magnet, `nodes.dat`, `ipfilter.dat`, `server.met`, JSON, Markdown, API, MCP, CLI, 2xx/3xx/4xx/5xx.

- `searchKeywords*`、`languageChinese`、`webhookTemplatePlaceholder`、`urlPlaceholder`、`rssProviderBuiltin`、`rssCookiesHint`、`donateDate`、`mobileReferrer`、`apiServiceApi`、`extension*Store` 按 `copyKeys` 原样复制。
- `Proxy`、`Host`、`Webhook`、`Endpoint`、`Push`、`Ratio` 在 `keep` 中，保持拉丁原文。

## 词汇对照

| en | uk | 备注 |
|---|---|---|
| Download | Завантаження | 动词按钮 `Завантажити`；状态 `Завантажується` |
| Upload | Відвантаження | 动词 `Відвантажити` |
| Seeding | Роздача | 与「做种中」过滤器一致 |
| Seed | Сід |  |
| Peer | Пір |  |
| Tracker | Трекер |  |
| Completed | Завершено | 状态名 |
| Failed | Помилка | 状态名 |
| Paused | Призупинено | 状态名 |
| Pause | Призупинити |  |
| Resume | Відновити |  |
| Start | Запустити |  |
| Cancel | Скасувати |  |
| Confirm | Підтвердити |  |
| OK | OK |  |
| Delete | Видалити |  |
| Remove | Вилучити |  |
| Task | Завдання |  |
| Task group | Група завдань |  |
| Queue | Черга |  |
| Category | Категорія |  |
| File | Файл |  |
| Folder | Папка |  |
| Path | Шлях |  |
| File name | Назва файлу |  |
| Save location | Місце збереження |  |
| Settings | Налаштування |  |
| Default | За замовчуванням |  |
| Advanced | Додатково |  |
| Speed limit | Обмеження швидкості |  |
| Threads | Потоки |  |
| Connections | З’єднання |  |
| Segment | Сегмент |  |
| Direct | Напряму |  |
| Dedicated proxy | Окремий Proxy | Proxy 保持原样 |
| Plugin | Плагін |  |
| Browser extension | Розширення браузера |  |
| Component | Компонент |  |
| Device | Пристрій |  |
| Account | Обліковий запис |  |
| Sign in | Увійти |  |
| Sign out | Вийти |  |
| Sync | Синхронізація |  |
| Notification | Сповіщення |  |
| Theme | Тема |  |
| Appearance | Вигляд |  |
| Language | Мова |  |
| Tray | Системний трей |  |
| Shortcut | Комбінація клавіш |  |
| Update | Оновлення |  |
| Subscription | Підписка | RSS 订阅 |
| Feed | Стрічка |  |
| Quota | Квота |  |
| Diagnostics | Діагностика |  |
| Log | Журнал |  |
| Blocklist | Список блокування |  |
| Allowlist | Список дозволених |  |

## qwen-mt 机器翻译配置

`scripts/i18n-mt` 只读取本节的 JSON 代码块；改上文术语时同步这里。

```json
{
  "targetLang": "Ukrainian",
  "source": "en",
  "domains": "UI strings of FluxDown, a desktop and mobile download manager (HTTP, BitTorrent, eD2K). Standard Ukrainian, following Windows and Android Ukrainian conventions; no Russianisms, never use the letters ы э ъ ё. Address the user as lowercase 'ви', never 'ти'. Buttons and menus in infinitive (Завантажити, Призупинити, Видалити). Sentence case; no final period on buttons, menus and labels, full sentences end with a period. Quotes « », ellipsis …, apostrophe ’ (U+2019). Decimal comma (1,5 GB), space between number and unit, percent with a space: {percent} %. Use correct Ukrainian plural forms after counts. Keep {placeholder} names unchanged.",
  "siteDomains": "Website and documentation of FluxDown, a free open-source download manager (HTTP, BitTorrent, eD2K) for desktop, mobile, NAS and servers. Standard Ukrainian without Russianisms; clear and natural marketing / technical documentation tone; address the reader as 'ви'. Sentence case for headings. Do not translate code, commands, file paths, config keys, environment variables, URLs, product names or placeholders such as {0} or {name}; keep HTML tags and Markdown markup unchanged.",
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
    "Webhook", "Endpoint"
  ],
  "terms": {
    "Download": "Завантаження",
    "Upload": "Відвантаження",
    "Seeding": "Роздача",
    "Seed": "Сід",
    "Peer": "Пір",
    "Tracker": "Трекер",
    "Completed": "Завершено",
    "Failed": "Помилка",
    "Paused": "Призупинено",
    "Pause": "Призупинити",
    "Resume": "Відновити",
    "Start": "Запустити",
    "Cancel": "Скасувати",
    "Confirm": "Підтвердити",
    "OK": "OK",
    "Delete": "Видалити",
    "Remove": "Вилучити",
    "Task": "Завдання",
    "Task group": "Група завдань",
    "Queue": "Черга",
    "Category": "Категорія",
    "File": "Файл",
    "Folder": "Папка",
    "Path": "Шлях",
    "File name": "Назва файлу",
    "Save location": "Місце збереження",
    "Settings": "Налаштування",
    "Default": "За замовчуванням",
    "Advanced": "Додатково",
    "Speed limit": "Обмеження швидкості",
    "Threads": "Потоки",
    "Connections": "З’єднання",
    "Segment": "Сегмент",
    "Direct": "Напряму",
    "Dedicated proxy": "Окремий проксі",
    "Plugin": "Плагін",
    "Browser extension": "Розширення браузера",
    "Component": "Компонент",
    "Device": "Пристрій",
    "Account": "Обліковий запис",
    "Sign in": "Увійти",
    "Sign out": "Вийти",
    "Sync": "Синхронізація",
    "Notification": "Сповіщення",
    "Theme": "Тема",
    "Appearance": "Вигляд",
    "Language": "Мова",
    "Tray": "Системний трей",
    "Shortcut": "Комбінація клавіш",
    "Update": "Оновлення",
    "Subscription": "Підписка",
    "Feed": "Стрічка",
    "Quota": "Квота",
    "Diagnostics": "Діагностика",
    "Log": "Журнал",
    "Blocklist": "Список блокування",
    "Allowlist": "Список дозволених"
  },
  "copyKeys": [
    "searchKeywords*", "languageChinese", "webhookTemplatePlaceholder", "urlPlaceholder", "rssProviderBuiltin",
    "rssCookiesHint", "donateDate", "mobileReferrer", "apiServiceApi", "extension*Store"
  ]
}
```
