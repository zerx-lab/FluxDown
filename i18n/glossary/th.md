# th（ไทย）翻译术语表

GitHub Actions 工作流 `.github/workflows/i18n-mt.yml`（qwen-mt）翻译 `assets/i18n/th.json` 时读取本表；人工修正术语时也以本表为准。用词对齐 Windows / Android 该语言界面与 qBittorrent 译文的通行说法。

## 原则

- **语义以 `en.json` 为准**；`zh.json` 只用于理解语境（en 有歧义时看中文怎么说）。
- 标准泰语，用词对齐 Windows / Android 泰语界面；外来技术词用通行的音译（`ดาวน์โหลด`、`อัปโหลด`、`ปลั๊กอิน`）。
- 语气：礼貌中性，不加 `ครับ` / `ค่ะ` / `นะ` 等语气词，不用第二人称代词（`คุณ`）开头，除非原文明确指向用户；按钮、菜单用动词或名词短语。
- 泰语词间不加空格，短语 / 句子之间才用空格；句末不加句号，按钮、菜单项、字段名同理。
- 数字用阿拉伯数字，小数点用句点（`1.5 GB`），数值与单位之间留空格；百分号紧贴数字：`{percent}%`。
- 泰语名词无复数变化；`{count}` 后接量词或名词（`{count} ไฟล์`）。
- 引号用 `“ ”`；省略号用 `…`；不使用佛历，日期保持原有格式。
- `{name}` 占位符原样保留，可调整语序，不翻译、不改名、不新增花括号。

## 不翻译（原样保留）

FluxDown, `fluxdown_engine`, `fluxdownd`, `fluxdown-agent`, ZerxLabBot, BitTorrent, **BT**, eD2K, Kad, aria2, yt-dlp, FFmpeg, HLS, DASH, M3U8, RSS, WebDAV, SMB, DHT, PEX, uTP, UPnP, **MSE**, HTTP(S), FTP, SOCKS5, TLS, CDN, LAN, VPN, TUN, Wi-Fi, **NMH** (native messaging host, no desarrollar la sigla), **ComSpec**, `%SystemRoot%\system32\cmd.exe`, `chrome://restart`, `FLUXDOWN_BIND`, `PUID/PGID`, `noexec`, `loopback`, `link-local`, `carrier-grade NAT`, HMAC-SHA256, `X-FluxDown-Signature`, CIDR, IPv4/IPv6, infohash, peer ID, Referer, **User-Agent**, cookie, ntfy, Bark, WeCom, Feishu, DingTalk, Telegram, Discord, Slack, Gotify, Home Assistant, NAS, Docker, macOS, Windows, Linux, Android, iOS, Chrome, Edge, Firefox, Safari, GitHub, Torrent, Magnet, `nodes.dat`, `ipfilter.dat`, `server.met`, JSON, Markdown, API, MCP, CLI, 2xx/3xx/4xx/5xx.

- `searchKeywords*`、`languageChinese`、`webhookTemplatePlaceholder`、`urlPlaceholder`、`rssProviderBuiltin`、`rssCookiesHint`、`donateDate`、`mobileReferrer`、`apiServiceApi`、`extension*Store` 按 `copyKeys` 原样复制。
- `Proxy`、`Host`、`Webhook`、`Endpoint`、`Push`、`Ratio` 在 `keep` 中，保持拉丁原文。

## 词汇对照

| en | th | 备注 |
|---|---|---|
| Download | ดาวน์โหลด | 状态 `กำลังดาวน์โหลด` |
| Upload | อัปโหลด |  |
| Seeding | กำลังซีด | 与「做种中」过滤器一致 |
| Seed | ซีด |  |
| Peer | เพียร์ |  |
| Tracker | แทรกเกอร์ |  |
| Completed | เสร็จสมบูรณ์ | 状态名 |
| Failed | ล้มเหลว | 状态名 |
| Paused | หยุดชั่วคราว | 状态名 |
| Pause | หยุดชั่วคราว |  |
| Resume | ดำเนินการต่อ |  |
| Start | เริ่ม |  |
| Cancel | ยกเลิก |  |
| Confirm | ยืนยัน |  |
| OK | ตกลง |  |
| Delete | ลบ |  |
| Remove | นำออก |  |
| Task | งาน |  |
| Task group | กลุ่มงาน |  |
| Queue | คิว |  |
| Category | หมวดหมู่ |  |
| File | ไฟล์ |  |
| Folder | โฟลเดอร์ |  |
| Path | เส้นทาง |  |
| File name | ชื่อไฟล์ |  |
| Save location | ตำแหน่งที่บันทึก |  |
| Settings | การตั้งค่า |  |
| Default | ค่าเริ่มต้น |  |
| Advanced | ขั้นสูง |  |
| Speed limit | จำกัดความเร็ว |  |
| Threads | เธรด |  |
| Connections | การเชื่อมต่อ |  |
| Segment | เซกเมนต์ |  |
| Direct | โดยตรง |  |
| Dedicated proxy | Proxy เฉพาะ | Proxy 保持原样 |
| Plugin | ปลั๊กอิน |  |
| Browser extension | ส่วนขยายเบราว์เซอร์ |  |
| Component | คอมโพเนนต์ |  |
| Device | อุปกรณ์ |  |
| Account | บัญชี |  |
| Sign in | ลงชื่อเข้าใช้ |  |
| Sign out | ออกจากระบบ |  |
| Sync | ซิงค์ |  |
| Notification | การแจ้งเตือน |  |
| Theme | ธีม |  |
| Appearance | รูปลักษณ์ |  |
| Language | ภาษา |  |
| Tray | ถาดระบบ |  |
| Shortcut | ทางลัด |  |
| Update | อัปเดต |  |
| Subscription | การสมัครรับ | RSS 订阅 |
| Feed | ฟีด |  |
| Quota | โควตา |  |
| Diagnostics | การวินิจฉัย |  |
| Log | บันทึก |  |
| Blocklist | รายการบล็อก |  |
| Allowlist | รายการที่อนุญาต |  |

## qwen-mt 机器翻译配置

`scripts/i18n-mt` 只读取本节的 JSON 代码块；改上文术语时同步这里。

```json
{
  "targetLang": "Thai",
  "source": "en",
  "domains": "UI strings of FluxDown, a desktop and mobile download manager (HTTP, BitTorrent, eD2K). Standard Thai, following Windows and Android Thai conventions; established transliterations for technical terms (ดาวน์โหลด, อัปโหลด, ปลั๊กอิน). Polite neutral tone: no sentence-final particles such as ครับ, ค่ะ or นะ; avoid addressing the user with 'คุณ' unless the source clearly does. Buttons and menus as short verb or noun phrases. No spaces between words, spaces only between phrases or sentences; no final period on buttons, menus and labels. Arabic digits, decimal point (1.5 GB), space between number and unit, percent attached: {percent}%. Quotes “ ”, ellipsis …. Keep {placeholder} names unchanged.",
  "siteDomains": "Website and documentation of FluxDown, a free open-source download manager (HTTP, BitTorrent, eD2K) for desktop, mobile, NAS and servers. Standard Thai; clear and natural polite-neutral marketing / technical documentation tone without sentence-final particles such as ครับ or ค่ะ; Thai spacing between phrases only. Established transliterations for technical terms. Do not translate code, commands, file paths, config keys, environment variables, URLs, product names or placeholders such as {0} or {name}; keep HTML tags and Markdown markup unchanged.",
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
    "Download": "ดาวน์โหลด",
    "Upload": "อัปโหลด",
    "Seeding": "กำลังซีด",
    "Seed": "ซีด",
    "Peer": "เพียร์",
    "Tracker": "แทรกเกอร์",
    "Completed": "เสร็จสมบูรณ์",
    "Failed": "ล้มเหลว",
    "Paused": "หยุดชั่วคราว",
    "Pause": "หยุดชั่วคราว",
    "Resume": "ดำเนินการต่อ",
    "Start": "เริ่ม",
    "Cancel": "ยกเลิก",
    "Confirm": "ยืนยัน",
    "OK": "ตกลง",
    "Delete": "ลบ",
    "Remove": "นำออก",
    "Task": "งาน",
    "Task group": "กลุ่มงาน",
    "Queue": "คิว",
    "Category": "หมวดหมู่",
    "File": "ไฟล์",
    "Folder": "โฟลเดอร์",
    "Path": "เส้นทาง",
    "File name": "ชื่อไฟล์",
    "Save location": "ตำแหน่งที่บันทึก",
    "Settings": "การตั้งค่า",
    "Default": "ค่าเริ่มต้น",
    "Advanced": "ขั้นสูง",
    "Speed limit": "จำกัดความเร็ว",
    "Threads": "เธรด",
    "Connections": "การเชื่อมต่อ",
    "Segment": "เซกเมนต์",
    "Direct": "โดยตรง",
    "Dedicated proxy": "พร็อกซีเฉพาะ",
    "Plugin": "ปลั๊กอิน",
    "Browser extension": "ส่วนขยายเบราว์เซอร์",
    "Component": "คอมโพเนนต์",
    "Device": "อุปกรณ์",
    "Account": "บัญชี",
    "Sign in": "ลงชื่อเข้าใช้",
    "Sign out": "ออกจากระบบ",
    "Sync": "ซิงค์",
    "Notification": "การแจ้งเตือน",
    "Theme": "ธีม",
    "Appearance": "รูปลักษณ์",
    "Language": "ภาษา",
    "Tray": "ถาดระบบ",
    "Shortcut": "ทางลัด",
    "Update": "อัปเดต",
    "Subscription": "การสมัครรับ",
    "Feed": "ฟีด",
    "Quota": "โควตา",
    "Diagnostics": "การวินิจฉัย",
    "Log": "บันทึก",
    "Blocklist": "รายการบล็อก",
    "Allowlist": "รายการที่อนุญาต"
  },
  "copyKeys": [
    "searchKeywords*", "languageChinese", "webhookTemplatePlaceholder", "urlPlaceholder", "rssProviderBuiltin",
    "rssCookiesHint", "donateDate", "mobileReferrer", "apiServiceApi", "extension*Store"
  ]
}
```
