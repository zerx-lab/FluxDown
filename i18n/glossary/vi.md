# vi（Tiếng Việt）翻译术语表

GitHub Actions 工作流 `.github/workflows/i18n-mt.yml`（qwen-mt）翻译 `assets/i18n/vi.json` 时读取本表；人工修正术语时也以本表为准。用词对齐 Windows / Android 该语言界面与 qBittorrent 译文的通行说法。

## 原则

- **语义以 `en.json` 为准**；`zh.json` 只用于理解语境（en 有歧义时看中文怎么说）。
- 标准越南语，完整保留声调与变音符号（ă â đ ê ô ơ ư 及五个声调）。
- 语气：称呼用 `bạn`；按钮、菜单用动词短语（`Tải xuống`、`Tạm dừng`、`Xóa`）；不混用 `quý khách` 等其他人称。
- 标题与按钮用句首大写（Sentence case）；按钮、菜单项、字段名不加句号，完整句子加句号。
- 数字与单位：小数点用逗号（`1,5 GB`），数值与单位之间留空格；百分号紧贴数字：`{percent}%`。
- 越南语名词无复数变化，`{count}` 后直接接名词（`{count} tệp`）。
- 引号用 `“ ”`；省略号用 `…`。
- `{name}` 占位符原样保留，可调整语序，不翻译、不改名、不新增花括号。

## 不翻译（原样保留）

FluxDown, `fluxdown_engine`, `fluxdownd`, `fluxdown-agent`, ZerxLabBot, BitTorrent, **BT**, eD2K, Kad, aria2, yt-dlp, FFmpeg, HLS, DASH, M3U8, RSS, WebDAV, SMB, DHT, PEX, uTP, UPnP, **MSE**, HTTP(S), FTP, SOCKS5, TLS, CDN, LAN, VPN, TUN, Wi-Fi, **NMH** (native messaging host, no desarrollar la sigla), **ComSpec**, `%SystemRoot%\system32\cmd.exe`, `chrome://restart`, `FLUXDOWN_BIND`, `PUID/PGID`, `noexec`, `loopback`, `link-local`, `carrier-grade NAT`, HMAC-SHA256, `X-FluxDown-Signature`, CIDR, IPv4/IPv6, infohash, peer ID, Referer, **User-Agent**, cookie, ntfy, Bark, WeCom, Feishu, DingTalk, Telegram, Discord, Slack, Gotify, Home Assistant, NAS, Docker, macOS, Windows, Linux, Android, iOS, Chrome, Edge, Firefox, Safari, GitHub, Torrent, Magnet, `nodes.dat`, `ipfilter.dat`, `server.met`, JSON, Markdown, API, MCP, CLI, 2xx/3xx/4xx/5xx.

- `searchKeywords*`、`languageChinese`、`webhookTemplatePlaceholder`、`urlPlaceholder`、`rssProviderBuiltin`、`rssCookiesHint`、`donateDate`、`mobileReferrer`、`apiServiceApi`、`extension*Store` 按 `copyKeys` 原样复制。
- `Proxy`、`Host`、`Webhook`、`Endpoint`、`Push`、`Ratio` 在 `keep` 中，保持拉丁原文。

## 词汇对照

| en | vi | 备注 |
|---|---|---|
| Download | Tải xuống | 状态 `Đang tải xuống` |
| Upload | Tải lên |  |
| Seeding | Đang seed | 与「做种中」过滤器一致 |
| Seed | Seed |  |
| Peer | Peer |  |
| Tracker | Tracker |  |
| Completed | Đã hoàn tất | 状态名 |
| Failed | Thất bại | 状态名 |
| Paused | Đã tạm dừng | 状态名 |
| Pause | Tạm dừng |  |
| Resume | Tiếp tục |  |
| Start | Bắt đầu |  |
| Cancel | Hủy |  |
| Confirm | Xác nhận |  |
| OK | OK |  |
| Delete | Xóa |  |
| Remove | Gỡ bỏ |  |
| Task | Tác vụ |  |
| Task group | Nhóm tác vụ |  |
| Queue | Hàng đợi |  |
| Category | Danh mục |  |
| File | Tệp |  |
| Folder | Thư mục |  |
| Path | Đường dẫn |  |
| File name | Tên tệp |  |
| Save location | Vị trí lưu |  |
| Settings | Cài đặt |  |
| Default | Mặc định |  |
| Advanced | Nâng cao |  |
| Speed limit | Giới hạn tốc độ |  |
| Threads | Luồng |  |
| Connections | Kết nối |  |
| Segment | Phân đoạn |  |
| Direct | Trực tiếp |  |
| Dedicated proxy | Proxy riêng | Proxy 保持原样 |
| Plugin | Plugin |  |
| Browser extension | Tiện ích mở rộng trình duyệt |  |
| Component | Thành phần |  |
| Device | Thiết bị |  |
| Account | Tài khoản |  |
| Sign in | Đăng nhập |  |
| Sign out | Đăng xuất |  |
| Sync | Đồng bộ hóa |  |
| Notification | Thông báo |  |
| Theme | Chủ đề |  |
| Appearance | Giao diện |  |
| Language | Ngôn ngữ |  |
| Tray | Khay hệ thống |  |
| Shortcut | Phím tắt |  |
| Update | Cập nhật |  |
| Subscription | Đăng ký | RSS 订阅 |
| Feed | Nguồn cấp |  |
| Quota | Hạn mức |  |
| Diagnostics | Chẩn đoán |  |
| Log | Nhật ký |  |
| Blocklist | Danh sách chặn |  |
| Allowlist | Danh sách cho phép |  |

## qwen-mt 机器翻译配置

`scripts/i18n-mt` 只读取本节的 JSON 代码块；改上文术语时同步这里。

```json
{
  "targetLang": "Vietnamese",
  "source": "en",
  "domains": "UI strings of FluxDown, a desktop and mobile download manager (HTTP, BitTorrent, eD2K). Standard Vietnamese with full diacritics and tone marks. Address the user as 'bạn', never mix other pronouns. Buttons and menus as short verb phrases (Tải xuống, Tạm dừng, Xóa). Sentence case; no final period on buttons, menus and labels, full sentences end with a period. Quotes “ ”, ellipsis …. Decimal comma (1,5 GB), space between number and unit, percent attached: {percent}%. Keep {placeholder} names unchanged.",
  "siteDomains": "Website and documentation of FluxDown, a free open-source download manager (HTTP, BitTorrent, eD2K) for desktop, mobile, NAS and servers. Standard Vietnamese with full diacritics; clear and natural marketing / technical documentation tone; address the reader as 'bạn'. Sentence case for headings. Do not translate code, commands, file paths, config keys, environment variables, URLs, product names or placeholders such as {0} or {name}; keep HTML tags and Markdown markup unchanged.",
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
    "Download": "Tải xuống",
    "Upload": "Tải lên",
    "Seeding": "Đang seed",
    "Seed": "Seed",
    "Peer": "Peer",
    "Tracker": "Tracker",
    "Completed": "Đã hoàn tất",
    "Failed": "Thất bại",
    "Paused": "Đã tạm dừng",
    "Pause": "Tạm dừng",
    "Resume": "Tiếp tục",
    "Start": "Bắt đầu",
    "Cancel": "Hủy",
    "Confirm": "Xác nhận",
    "OK": "OK",
    "Delete": "Xóa",
    "Remove": "Gỡ bỏ",
    "Task": "Tác vụ",
    "Task group": "Nhóm tác vụ",
    "Queue": "Hàng đợi",
    "Category": "Danh mục",
    "File": "Tệp",
    "Folder": "Thư mục",
    "Path": "Đường dẫn",
    "File name": "Tên tệp",
    "Save location": "Vị trí lưu",
    "Settings": "Cài đặt",
    "Default": "Mặc định",
    "Advanced": "Nâng cao",
    "Speed limit": "Giới hạn tốc độ",
    "Threads": "Luồng",
    "Connections": "Kết nối",
    "Segment": "Phân đoạn",
    "Direct": "Trực tiếp",
    "Dedicated proxy": "Proxy riêng",
    "Plugin": "Plugin",
    "Browser extension": "Tiện ích mở rộng trình duyệt",
    "Component": "Thành phần",
    "Device": "Thiết bị",
    "Account": "Tài khoản",
    "Sign in": "Đăng nhập",
    "Sign out": "Đăng xuất",
    "Sync": "Đồng bộ hóa",
    "Notification": "Thông báo",
    "Theme": "Chủ đề",
    "Appearance": "Giao diện",
    "Language": "Ngôn ngữ",
    "Tray": "Khay hệ thống",
    "Shortcut": "Phím tắt",
    "Update": "Cập nhật",
    "Subscription": "Đăng ký",
    "Feed": "Nguồn cấp",
    "Quota": "Hạn mức",
    "Diagnostics": "Chẩn đoán",
    "Log": "Nhật ký",
    "Blocklist": "Danh sách chặn",
    "Allowlist": "Danh sách cho phép"
  },
  "copyKeys": [
    "searchKeywords*", "languageChinese", "webhookTemplatePlaceholder", "urlPlaceholder", "rssProviderBuiltin",
    "rssCookiesHint", "donateDate", "mobileReferrer", "apiServiceApi", "extension*Store"
  ]
}
```
