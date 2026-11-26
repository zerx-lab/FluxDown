# ko（한국어）翻译术语表

GitHub Actions 工作流 `.github/workflows/i18n-mt.yml`（qwen-mt）翻译 `assets/i18n/ko.json` 与官网 / 文档时读取本表；人工修正术语时也以本表为准。用词对齐 Windows / Android 与 qBittorrent 该语言译文的通行说法。

## 原则

- **语义以 `en.json` 为准**；`zh.json` 只用于理解语境（en 有歧义时看中文怎么说）。
- 标准韩语（韩国）；术语对齐 Windows / Android 韩语界面与 qBittorrent 韩语译文（`다운로드`、`업로드`、`시딩`、`트래커`）。
- 语气：说明与提示句用「합니다체」（`~할 수 있습니다`、`~하세요` 仅用于必须的指引），与 Windows 韩语一致；按钮、菜单、标签用名词或动词词干名词形（`저장`、`삭제`、`다운로드`），不加句号。
- 不混用 해요체与 합니다체；同一屏文案语体一致。
- 助词（`을/를`、`이/가`、`은/는`）随前一音节收音变化：占位符后尽量不直接接助词，调整句式避开。
- 标点：引号 `“ ”`，省略号 `…`；整句以 `.` 结尾，标签不加。
- 数字与单位：小数点 `.`（`1.5 GB`），数值与单位之间留空格；百分号紧跟数值：`{percent}%`。
- 韩文无大小写；英文原文 Title Case 标题直接译成自然的名词短语。
- `{name}` 占位符原样保留（同名、同数量），不翻译、不改名，不引入多余花括号。

## 不翻译（原样保留）

FluxDown、fluxdown_engine、fluxdownd、fluxdown-agent、ZerxLabBot、BitTorrent、BT、eD2K、Kad、aria2、yt-dlp、FFmpeg、HLS、DASH、M3U8、RSS、WebDAV、SMB、DHT、PEX、uTP、UPnP、MSE、HTTP、HTTPS、FTP、SOCKS5、TLS、CDN、LAN、VPN、TUN、Wi-Fi、NMH、ComSpec、%SystemRoot%\system32\cmd.exe、chrome://restart、FLUXDOWN_BIND、PUID/PGID、noexec、loopback、link-local、carrier-grade NAT、HMAC-SHA256、X-FluxDown-Signature、CIDR、IPv4、IPv6、infohash、peer ID、Referer、User-Agent、cookie、ntfy、Bark、WeCom、Feishu、DingTalk、Telegram、Discord、Slack、Gotify、Home Assistant、NAS、Docker、macOS、Windows、Linux、Android、iOS、Chrome、Edge、Firefox、Safari、GitHub、Torrent、Magnet、nodes.dat、ipfilter.dat、server.met、JSON、Markdown、API、MCP、CLI、2xx、3xx、4xx、5xx、Webhook。

- `searchKeywords*`、`languageChinese`、`webhookTemplatePlaceholder`、`urlPlaceholder`、`rssProviderBuiltin`、`rssCookiesHint`、`donateDate`、`mobileReferrer`、`apiServiceApi`、各扩展商店名（`extension*Store`）是软件值或搜索索引，原样复制（`copyKeys`）。

## 词汇对照

| en | ko | 备注 |
|---|---|---|
| Download | 다운로드 |  |
| Downloading | 다운로드 중 |  |
| Upload | 업로드 |  |
| Seeding | 시딩 | 与「做种中」侧栏过滤器一致 |
| Seed | 시드 |  |
| Peer | 피어 |  |
| Leech | 리처 |  |
| Tracker | 트래커 |  |
| Task | 작업 |  |
| Task group | 작업 그룹 |  |
| Queue | 대기열 |  |
| Category | 카테고리 |  |
| File | 파일 |  |
| Folder | 폴더 |  |
| Path | 경로 |  |
| File name | 파일 이름 |  |
| Save location | 저장 위치 |  |
| Settings | 설정 |  |
| Default | 기본값 |  |
| Advanced | 고급 |  |
| Speed limit | 속도 제한 |  |
| Threads | 스레드 |  |
| Connections | 연결 |  |
| Segment | 세그먼트 |  |
| Plugin | 플러그인 |  |
| Browser extension | 브라우저 확장 프로그램 |  |
| Component | 구성 요소 |  |
| Device | 기기 |  |
| Account | 계정 |  |
| Sign in | 로그인 |  |
| Sign out | 로그아웃 |  |
| Sync | 동기화 |  |
| Notification | 알림 |  |
| Theme | 테마 |  |
| Appearance | 모양 |  |
| Language | 언어 |  |
| Tray | 시스템 트레이 |  |
| Shortcut | 단축키 | 键盘快捷键用 `단축키` |
| Update | 업데이트 |  |
| Subscription | 구독 |  |
| Feed | 피드 |  |
| Quota | 할당량 |  |
| Diagnostics | 진단 |  |
| Log | 로그 |  |
| Pause | 일시 중지 |  |
| Resume | 다시 시작 |  |
| Start | 시작 |  |
| Cancel | 취소 |  |
| Confirm | 확인 | 与 OK 同为 `확인` |
| OK | 확인 |  |
| Delete | 삭제 |  |
| Remove | 제거 |  |
| Completed | 완료됨 |  |
| Failed | 실패 |  |
| Paused | 일시 중지됨 |  |
| Direct | 직접 |  |
| Dedicated proxy | 전용 프록시 |  |
| Blocklist | 차단 목록 |  |
| Allowlist | 허용 목록 |  |
| Access key | 액세스 키 |  |
| Pairing | 페어링 |  |
| Ratio | 공유 비율 | BT 分享率 |
| Proxy | 프록시 |  |
| Host | 호스트 |  |
| Push | 푸시 |  |
| Endpoint | 엔드포인트 |  |

## qwen-mt 机器翻译配置

`scripts/i18n-mt` 只读取本节的 JSON 代码块；改上文术语时同步这里。

```json
{
  "targetLang": "Korean",
  "source": "en",
  "domains": "UI strings of FluxDown, a desktop and mobile download manager (HTTP, BitTorrent, eD2K). Natural Korean for South Korea, terminology aligned with Windows and Android Korean UI. Polite formal 합니다체 for messages and hints; buttons, menus and labels in noun form (저장, 삭제, 다운로드), no final period on labels, full sentences end with a period. Do not mix 해요체 and 합니다체. Quotes “ ”, ellipsis …. Decimal point (1.5 GB), space between number and unit, percent attached: {percent}%. Rephrase to avoid particles attached directly to {placeholder}. Keep {placeholder} names unchanged.",
  "siteDomains": "Website and documentation of FluxDown, a free open-source download manager (HTTP, BitTorrent, eD2K) for desktop, mobile, NAS and servers. Natural Korean for South Korea in clear marketing / technical documentation tone; polite formal 합니다체, consistently. Keep spacing between Korean words and Latin terms natural. Do not translate code, commands, file paths, config keys, environment variables, URLs, product names or placeholders such as {0} or {name}; keep HTML tags and Markdown markup unchanged.",
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
    "Download": "다운로드",
    "Downloading": "다운로드 중",
    "Upload": "업로드",
    "Seeding": "시딩",
    "Seed": "시드",
    "Peer": "피어",
    "Leech": "리처",
    "Tracker": "트래커",
    "Task": "작업",
    "Task group": "작업 그룹",
    "Queue": "대기열",
    "Category": "카테고리",
    "File": "파일",
    "Folder": "폴더",
    "Path": "경로",
    "File name": "파일 이름",
    "Save location": "저장 위치",
    "Settings": "설정",
    "Default": "기본값",
    "Advanced": "고급",
    "Speed limit": "속도 제한",
    "Threads": "스레드",
    "Connections": "연결",
    "Segment": "세그먼트",
    "Plugin": "플러그인",
    "Browser extension": "브라우저 확장 프로그램",
    "Component": "구성 요소",
    "Device": "기기",
    "Account": "계정",
    "Sign in": "로그인",
    "Sign out": "로그아웃",
    "Sync": "동기화",
    "Notification": "알림",
    "Theme": "테마",
    "Appearance": "모양",
    "Language": "언어",
    "Tray": "시스템 트레이",
    "Shortcut": "단축키",
    "Update": "업데이트",
    "Subscription": "구독",
    "Feed": "피드",
    "Quota": "할당량",
    "Diagnostics": "진단",
    "Log": "로그",
    "Pause": "일시 중지",
    "Resume": "다시 시작",
    "Start": "시작",
    "Cancel": "취소",
    "Confirm": "확인",
    "OK": "확인",
    "Delete": "삭제",
    "Remove": "제거",
    "Completed": "완료됨",
    "Failed": "실패",
    "Paused": "일시 중지됨",
    "Direct": "직접",
    "Dedicated proxy": "전용 프록시",
    "Blocklist": "차단 목록",
    "Allowlist": "허용 목록",
    "Access key": "액세스 키",
    "Pairing": "페어링",
    "Ratio": "공유 비율",
    "Proxy": "프록시",
    "Host": "호스트",
    "Push": "푸시",
    "Endpoint": "엔드포인트"
  },
  "copyKeys": [
    "searchKeywords*", "languageChinese", "webhookTemplatePlaceholder", "urlPlaceholder", "rssProviderBuiltin",
    "rssCookiesHint", "donateDate", "mobileReferrer", "apiServiceApi", "extension*Store"
  ]
}
```
