# id（Bahasa Indonesia）翻译术语表

GitHub Actions 工作流 `.github/workflows/i18n-mt.yml`（qwen-mt）翻译 `assets/i18n/id.json` 时读取本表；人工修正术语时也以本表为准。用词对齐 Windows / Android 该语言界面与 qBittorrent 译文的通行说法。

## 原则

- **语义以 `en.json` 为准**；`zh.json` 只用于理解语境（en 有歧义时看中文怎么说）。
- 标准印尼语（Bahasa Indonesia baku），用词对齐 Windows / Android 印尼语界面与 qBittorrent 印尼语译文。
- 语气：称呼用 `Anda`（首字母大写）；按钮、菜单用动词原形（`Unduh`、`Jeda`、`Hapus`）；不用 `kamu` / `lo` 等口语人称。
- 标题与按钮用句首大写（Sentence case）；按钮、菜单项、字段名不加句号，完整句子加句号。
- 数字与单位：小数点用逗号（`1,5 GB`），数值与单位之间留空格；百分号紧贴数字：`{percent}%`。
- 印尼语名词无复数变化；`{count}` 后直接接名词，不用重叠式（`{count} berkas`）。
- 引号用 `“ ”`；省略号用 `…`。
- `{name}` 占位符原样保留，可调整语序，不翻译、不改名、不新增花括号。

## 不翻译（原样保留）

FluxDown, `fluxdown_engine`, `fluxdownd`, `fluxdown-agent`, ZerxLabBot, BitTorrent, **BT**, eD2K, Kad, aria2, yt-dlp, FFmpeg, HLS, DASH, M3U8, RSS, WebDAV, SMB, DHT, PEX, uTP, UPnP, **MSE**, HTTP(S), FTP, SOCKS5, TLS, CDN, LAN, VPN, TUN, Wi-Fi, **NMH** (native messaging host, no desarrollar la sigla), **ComSpec**, `%SystemRoot%\system32\cmd.exe`, `chrome://restart`, `FLUXDOWN_BIND`, `PUID/PGID`, `noexec`, `loopback`, `link-local`, `carrier-grade NAT`, HMAC-SHA256, `X-FluxDown-Signature`, CIDR, IPv4/IPv6, infohash, peer ID, Referer, **User-Agent**, cookie, ntfy, Bark, WeCom, Feishu, DingTalk, Telegram, Discord, Slack, Gotify, Home Assistant, NAS, Docker, macOS, Windows, Linux, Android, iOS, Chrome, Edge, Firefox, Safari, GitHub, Torrent, Magnet, `nodes.dat`, `ipfilter.dat`, `server.met`, JSON, Markdown, API, MCP, CLI, 2xx/3xx/4xx/5xx.

- `searchKeywords*`、`languageChinese`、`webhookTemplatePlaceholder`、`urlPlaceholder`、`rssProviderBuiltin`、`rssCookiesHint`、`donateDate`、`mobileReferrer`、`apiServiceApi`、`extension*Store` 按 `copyKeys` 原样复制。
- `Proxy`、`Host`、`Webhook`、`Endpoint`、`Push`、`Ratio` 在 `keep` 中，保持拉丁原文。

## 词汇对照

| en | id | 备注 |
|---|---|---|
| Download | Unduh | 名词 `Unduhan`；状态 `Mengunduh` |
| Upload | Unggah | 名词 `Unggahan` |
| Seeding | Seeding | 与「做种中」过滤器一致 |
| Seed | Seed |  |
| Peer | Peer |  |
| Tracker | Tracker |  |
| Completed | Selesai | 状态名 |
| Failed | Gagal | 状态名 |
| Paused | Dijeda | 状态名 |
| Pause | Jeda |  |
| Resume | Lanjutkan |  |
| Start | Mulai |  |
| Cancel | Batal |  |
| Confirm | Konfirmasi |  |
| OK | OK |  |
| Delete | Hapus |  |
| Remove | Hapus |  |
| Task | Tugas |  |
| Task group | Grup tugas |  |
| Queue | Antrean |  |
| Category | Kategori |  |
| File | Berkas |  |
| Folder | Folder |  |
| Path | Jalur |  |
| File name | Nama berkas |  |
| Save location | Lokasi penyimpanan |  |
| Settings | Pengaturan |  |
| Default | Bawaan |  |
| Advanced | Lanjutan |  |
| Speed limit | Batas kecepatan |  |
| Threads | Utas |  |
| Connections | Koneksi |  |
| Segment | Segmen |  |
| Direct | Langsung |  |
| Dedicated proxy | Proxy khusus | Proxy 保持原样 |
| Plugin | Plugin |  |
| Browser extension | Ekstensi peramban |  |
| Component | Komponen |  |
| Device | Perangkat |  |
| Account | Akun |  |
| Sign in | Masuk |  |
| Sign out | Keluar |  |
| Sync | Sinkronisasi |  |
| Notification | Notifikasi |  |
| Theme | Tema |  |
| Appearance | Tampilan |  |
| Language | Bahasa |  |
| Tray | Baki sistem |  |
| Shortcut | Pintasan |  |
| Update | Pembaruan |  |
| Subscription | Langganan | RSS 订阅 |
| Feed | Umpan |  |
| Quota | Kuota |  |
| Diagnostics | Diagnostik |  |
| Log | Log |  |
| Blocklist | Daftar blokir |  |
| Allowlist | Daftar izin |  |

## qwen-mt 机器翻译配置

`scripts/i18n-mt` 只读取本节的 JSON 代码块；改上文术语时同步这里。

```json
{
  "targetLang": "Indonesian",
  "source": "en",
  "domains": "UI strings of FluxDown, a desktop and mobile download manager (HTTP, BitTorrent, eD2K). Standard Indonesian (Bahasa Indonesia baku), following Windows and Android Indonesian conventions. Address the user as 'Anda' (capitalized), never casual 'kamu'. Buttons and menus as base-form verbs (Unduh, Jeda, Hapus). Sentence case; no final period on buttons, menus and labels, full sentences end with a period. Quotes “ ”, ellipsis …. Decimal comma (1,5 GB), space between number and unit, percent attached: {percent}%. No reduplicated plurals after counts. Keep {placeholder} names unchanged.",
  "siteDomains": "Website and documentation of FluxDown, a free open-source download manager (HTTP, BitTorrent, eD2K) for desktop, mobile, NAS and servers. Standard Indonesian (Bahasa Indonesia baku); clear and natural marketing / technical documentation tone; address the reader as 'Anda'. Sentence case for headings. Do not translate code, commands, file paths, config keys, environment variables, URLs, product names or placeholders such as {0} or {name}; keep HTML tags and Markdown markup unchanged.",
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
    "Download": "Unduh",
    "Upload": "Unggah",
    "Seeding": "Seeding",
    "Seed": "Seed",
    "Peer": "Peer",
    "Tracker": "Tracker",
    "Completed": "Selesai",
    "Failed": "Gagal",
    "Paused": "Dijeda",
    "Pause": "Jeda",
    "Resume": "Lanjutkan",
    "Start": "Mulai",
    "Cancel": "Batal",
    "Confirm": "Konfirmasi",
    "OK": "OK",
    "Delete": "Hapus",
    "Remove": "Hapus",
    "Task": "Tugas",
    "Task group": "Grup tugas",
    "Queue": "Antrean",
    "Category": "Kategori",
    "File": "Berkas",
    "Folder": "Folder",
    "Path": "Jalur",
    "File name": "Nama berkas",
    "Save location": "Lokasi penyimpanan",
    "Settings": "Pengaturan",
    "Default": "Bawaan",
    "Advanced": "Lanjutan",
    "Speed limit": "Batas kecepatan",
    "Threads": "Utas",
    "Connections": "Koneksi",
    "Segment": "Segmen",
    "Direct": "Langsung",
    "Dedicated proxy": "Proxy khusus",
    "Plugin": "Plugin",
    "Browser extension": "Ekstensi peramban",
    "Component": "Komponen",
    "Device": "Perangkat",
    "Account": "Akun",
    "Sign in": "Masuk",
    "Sign out": "Keluar",
    "Sync": "Sinkronisasi",
    "Notification": "Notifikasi",
    "Theme": "Tema",
    "Appearance": "Tampilan",
    "Language": "Bahasa",
    "Tray": "Baki sistem",
    "Shortcut": "Pintasan",
    "Update": "Pembaruan",
    "Subscription": "Langganan",
    "Feed": "Umpan",
    "Quota": "Kuota",
    "Diagnostics": "Diagnostik",
    "Log": "Log",
    "Blocklist": "Daftar blokir",
    "Allowlist": "Daftar izin"
  },
  "copyKeys": [
    "searchKeywords*", "languageChinese", "webhookTemplatePlaceholder", "urlPlaceholder", "rssProviderBuiltin",
    "rssCookiesHint", "donateDate", "mobileReferrer", "apiServiceApi", "extension*Store"
  ]
}
```
