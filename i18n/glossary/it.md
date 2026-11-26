# it（Italiano）翻译术语表

GitHub Actions 工作流 `.github/workflows/i18n-mt.yml`（qwen-mt）翻译 `assets/i18n/it.json` 与官网 / 文档时读取本表；人工修正术语时也以本表为准。用词对齐 Windows / Android 与 qBittorrent 该语言译文的通行说法。

## 原则

- **语义以 `en.json` 为准**；`zh.json` 只用于理解语境（en 有歧义时看中文怎么说）。
- 标准意大利语（意大利）；术语对齐 Windows / Android 意大利语界面与 qBittorrent 意大利语译文。
- 称呼：**tu**（非正式），与现代应用一致（`puoi`、`controlla`、`riprova`）；不用 `Lei`，同一文本内不混用。按钮、菜单、动作用动词（`Elimina`、`Metti in pausa`、`Riprendi` 为 tu 命令式）或名词。
- 句首大写（Sentence case），不做英语式 Title Case；按钮、菜单项、字段名不加句号，完整句子以句号结尾。
- 保留英语借词：`Download`、`Upload`、`File`、`Account`、`Plugin`、`Peer`、`Tracker`、`Seed`、`Feed`、`Proxy`；任务译为 `attività`，不要用 `compito`。
- 引号用 `“ ”`；省略号用 `…`；省音撇号用 `’`。
- 数字与单位：小数点用逗号（`1,5 GB`），数值与单位之间留空格；百分号紧跟数值：`{percent}%`；不改 `KB/s`、`MB/s` 等单位。
- `{name}` 占位符原样保留，可调整语序；注意冠词与介词缩合（`il`/`lo`/`l’`、`di`/`del`），必要时改写句子避开占位符前的省音。
- `{name}` 占位符原样保留（同名、同数量），不翻译、不改名，不引入多余花括号。

## 不翻译（原样保留）

FluxDown、fluxdown_engine、fluxdownd、fluxdown-agent、ZerxLabBot、BitTorrent、BT、eD2K、Kad、aria2、yt-dlp、FFmpeg、HLS、DASH、M3U8、RSS、WebDAV、SMB、DHT、PEX、uTP、UPnP、MSE、HTTP、HTTPS、FTP、SOCKS5、TLS、CDN、LAN、VPN、TUN、Wi-Fi、NMH、ComSpec、%SystemRoot%\system32\cmd.exe、chrome://restart、FLUXDOWN_BIND、PUID/PGID、noexec、loopback、link-local、carrier-grade NAT、HMAC-SHA256、X-FluxDown-Signature、CIDR、IPv4、IPv6、infohash、peer ID、Referer、User-Agent、cookie、ntfy、Bark、WeCom、Feishu、DingTalk、Telegram、Discord、Slack、Gotify、Home Assistant、NAS、Docker、macOS、Windows、Linux、Android、iOS、Chrome、Edge、Firefox、Safari、GitHub、Torrent、Magnet、nodes.dat、ipfilter.dat、server.met、JSON、Markdown、API、MCP、CLI、2xx、3xx、4xx、5xx、Webhook、Endpoint、Push。

- `searchKeywords*`、`languageChinese`、`webhookTemplatePlaceholder`、`urlPlaceholder`、`rssProviderBuiltin`、`rssCookiesHint`、`donateDate`、`mobileReferrer`、`apiServiceApi`、各扩展商店名（`extension*Store`）是软件值或搜索索引，原样复制（`copyKeys`）。

## 词汇对照

| en | it | 备注 |
|---|---|---|
| Download | Download |  |
| Downloading | Download in corso |  |
| Upload | Upload |  |
| Seeding | In seeding | 状态 `In seeding` |
| Seed | Seed |  |
| Peer | Peer |  |
| Leech | Leecher |  |
| Tracker | Tracker |  |
| Task | Attività | `Attività`，不用 `Compito` |
| Task group | Gruppo di attività |  |
| Queue | Coda |  |
| Category | Categoria |  |
| File | File |  |
| Folder | Cartella |  |
| Path | Percorso |  |
| File name | Nome file |  |
| Save location | Percorso di salvataggio |  |
| Settings | Impostazioni |  |
| Default | Predefinito |  |
| Advanced | Avanzate |  |
| Speed limit | Limite di velocità |  |
| Threads | Thread |  |
| Connections | Connessioni |  |
| Segment | Segmento |  |
| Plugin | Plugin |  |
| Browser extension | Estensione del browser |  |
| Component | Componente |  |
| Device | Dispositivo |  |
| Account | Account |  |
| Sign in | Accedi |  |
| Sign out | Esci |  |
| Sync | Sincronizzazione |  |
| Notification | Notifica |  |
| Theme | Tema |  |
| Appearance | Aspetto |  |
| Language | Lingua |  |
| Tray | Area di notifica |  |
| Shortcut | Scorciatoia |  |
| Update | Aggiornamento |  |
| Subscription | Iscrizione | RSS 订阅 `Iscrizione` |
| Feed | Feed |  |
| Quota | Quota |  |
| Diagnostics | Diagnostica |  |
| Log | Registro |  |
| Pause | Metti in pausa |  |
| Resume | Riprendi |  |
| Start | Avvia |  |
| Cancel | Annulla |  |
| Confirm | Conferma |  |
| OK | OK |  |
| Delete | Elimina |  |
| Remove | Rimuovi |  |
| Completed | Completato |  |
| Failed | Non riuscito | Windows 用语 `Non riuscito` |
| Paused | In pausa |  |
| Direct | Diretta |  |
| Dedicated proxy | Proxy dedicato |  |
| Blocklist | Elenco di blocco |  |
| Allowlist | Elenco consentiti |  |
| Access key | Chiave di accesso |  |
| Pairing | Associazione |  |
| Ratio | Rapporto di condivisione |  |

## qwen-mt 机器翻译配置

`scripts/i18n-mt` 只读取本节的 JSON 代码块；改上文术语时同步这里。

```json
{
  "targetLang": "Italian",
  "source": "en",
  "domains": "UI strings of FluxDown, a desktop and mobile download manager (HTTP, BitTorrent, eD2K). Standard Italian (Italy), terminology aligned with Windows and Android Italian UI. Informal tu in messages and hints, never Lei; buttons and menus in imperative or noun form (Elimina, Metti in pausa, Riprendi). Keep common English loanwords (Download, Upload, File, Account, Plugin, Peer, Tracker). Sentence case, never English-style Title Case; no final period on buttons, menus and labels, full sentences end with a period. Curly quotes “ ”, ellipsis …. Decimal comma (1,5 GB), space between number and unit, percent attached: {percent}%. Keep {placeholder} names unchanged.",
  "siteDomains": "Website and documentation of FluxDown, a free open-source download manager (HTTP, BitTorrent, eD2K) for desktop, mobile, NAS and servers. Standard Italian (Italy), clear and natural marketing / technical documentation tone; address the reader as tu. Sentence case for headings. Do not translate code, commands, file paths, config keys, environment variables, URLs, product names or placeholders such as {0} or {name}; keep HTML tags and Markdown markup unchanged.",
  "keep": [
    "FluxDown", "fluxdown_engine", "fluxdownd", "fluxdown-agent", "ZerxLabBot", "BitTorrent", "BT", "eD2K", "Kad", "aria2", "yt-dlp", "FFmpeg",
    "HLS", "DASH", "M3U8", "RSS", "WebDAV", "SMB", "DHT", "PEX", "uTP", "UPnP", "MSE", "HTTP",
    "HTTPS", "FTP", "SOCKS5", "TLS", "CDN", "LAN", "VPN", "TUN", "Wi-Fi", "NMH", "ComSpec", "%SystemRoot%\\system32\\cmd.exe",
    "chrome://restart", "FLUXDOWN_BIND", "PUID/PGID", "noexec", "loopback", "link-local", "carrier-grade NAT", "HMAC-SHA256", "X-FluxDown-Signature", "CIDR", "IPv4", "IPv6",
    "infohash", "peer ID", "Referer", "User-Agent", "cookie", "ntfy", "Bark", "WeCom", "Feishu", "DingTalk", "Telegram", "Discord",
    "Slack", "Gotify", "Home Assistant", "NAS", "Docker", "macOS", "Windows", "Linux", "Android", "iOS", "Chrome", "Edge",
    "Firefox", "Safari", "GitHub", "Torrent", "Magnet", "nodes.dat", "ipfilter.dat", "server.met", "JSON", "Markdown", "API", "MCP",
    "CLI", "2xx", "3xx", "4xx", "5xx", "Webhook", "Endpoint", "Push"
  ],
  "terms": {
    "Download": "Download",
    "Downloading": "Download in corso",
    "Upload": "Upload",
    "Seeding": "In seeding",
    "Seed": "Seed",
    "Peer": "Peer",
    "Leech": "Leecher",
    "Tracker": "Tracker",
    "Task": "Attività",
    "Task group": "Gruppo di attività",
    "Queue": "Coda",
    "Category": "Categoria",
    "File": "File",
    "Folder": "Cartella",
    "Path": "Percorso",
    "File name": "Nome file",
    "Save location": "Percorso di salvataggio",
    "Settings": "Impostazioni",
    "Default": "Predefinito",
    "Advanced": "Avanzate",
    "Speed limit": "Limite di velocità",
    "Threads": "Thread",
    "Connections": "Connessioni",
    "Segment": "Segmento",
    "Plugin": "Plugin",
    "Browser extension": "Estensione del browser",
    "Component": "Componente",
    "Device": "Dispositivo",
    "Account": "Account",
    "Sign in": "Accedi",
    "Sign out": "Esci",
    "Sync": "Sincronizzazione",
    "Notification": "Notifica",
    "Theme": "Tema",
    "Appearance": "Aspetto",
    "Language": "Lingua",
    "Tray": "Area di notifica",
    "Shortcut": "Scorciatoia",
    "Update": "Aggiornamento",
    "Subscription": "Iscrizione",
    "Feed": "Feed",
    "Quota": "Quota",
    "Diagnostics": "Diagnostica",
    "Log": "Registro",
    "Pause": "Metti in pausa",
    "Resume": "Riprendi",
    "Start": "Avvia",
    "Cancel": "Annulla",
    "Confirm": "Conferma",
    "OK": "OK",
    "Delete": "Elimina",
    "Remove": "Rimuovi",
    "Completed": "Completato",
    "Failed": "Non riuscito",
    "Paused": "In pausa",
    "Direct": "Diretta",
    "Dedicated proxy": "Proxy dedicato",
    "Blocklist": "Elenco di blocco",
    "Allowlist": "Elenco consentiti",
    "Access key": "Chiave di accesso",
    "Pairing": "Associazione",
    "Ratio": "Rapporto di condivisione"
  },
  "copyKeys": [
    "searchKeywords*", "languageChinese", "webhookTemplatePlaceholder", "urlPlaceholder", "rssProviderBuiltin",
    "rssCookiesHint", "donateDate", "mobileReferrer", "apiServiceApi", "extension*Store"
  ]
}
```
