# fr（Français）翻译术语表

GitHub Actions 工作流 `.github/workflows/i18n-mt.yml`（qwen-mt）翻译 `assets/i18n/fr.json` 与官网 / 文档时读取本表；人工修正术语时也以本表为准。用词对齐 Windows / Android 与 qBittorrent 该语言译文的通行说法。

## 原则

- **语义以 `en.json` 为准**；`zh.json` 只用于理解语境（en 有歧义时看中文怎么说）。
- 标准法语（法国）；术语对齐 Windows / Android 法语界面与 qBittorrent 法语译文。
- 称呼：**vous**（正式），说明与提示句统一用 `vous`；按钮、菜单、动作用动词不定式（`Télécharger`、`Supprimer`、`Mettre en pause`）。不要混用 `tu`。
- 句首大写（Sentence case），不做英语式 Title Case；按钮、菜单项、字段名不加句号，完整句子以句号结尾。
- 法语排版：`: ; ? !` 前加不换行空格，引号用 `« »`（内侧加不换行空格），省略号用 `…`，撇号用弯撇号 `’`。
- 数字与单位：小数点用逗号，数值与单位之间留空格；不改 `GB`、`MB/s` 等单位符号；百分号前留空格：`{percent} %`。
- 注意 `de`/`d’`、`le`/`l’` 的省音，必要时重写句子避开占位符前的省音。
- 保留的英语借词：`Webhook`、`Endpoint`、`Push`、`Ratio`；`proxy`、`tracker` 按法语界面惯例写，`Proxy` 不强制大写。
- `{name}` 占位符原样保留（同名、同数量），不翻译、不改名，不引入多余花括号。

## 不翻译（原样保留）

FluxDown、fluxdown_engine、fluxdownd、fluxdown-agent、ZerxLabBot、BitTorrent、BT、eD2K、Kad、aria2、yt-dlp、FFmpeg、HLS、DASH、M3U8、RSS、WebDAV、SMB、DHT、PEX、uTP、UPnP、MSE、HTTP、HTTPS、FTP、SOCKS5、TLS、CDN、LAN、VPN、TUN、Wi-Fi、NMH、ComSpec、%SystemRoot%\system32\cmd.exe、chrome://restart、FLUXDOWN_BIND、PUID/PGID、noexec、loopback、link-local、carrier-grade NAT、HMAC-SHA256、X-FluxDown-Signature、CIDR、IPv4、IPv6、infohash、peer ID、Referer、User-Agent、cookie、ntfy、Bark、WeCom、Feishu、DingTalk、Telegram、Discord、Slack、Gotify、Home Assistant、NAS、Docker、macOS、Windows、Linux、Android、iOS、Chrome、Edge、Firefox、Safari、GitHub、Torrent、Magnet、nodes.dat、ipfilter.dat、server.met、JSON、Markdown、API、MCP、CLI、2xx、3xx、4xx、5xx、Webhook、Endpoint、Push、Ratio。

- `searchKeywords*`、`languageChinese`、`webhookTemplatePlaceholder`、`urlPlaceholder`、`rssProviderBuiltin`、`rssCookiesHint`、`donateDate`、`mobileReferrer`、`apiServiceApi`、各扩展商店名（`extension*Store`）是软件值或搜索索引，原样复制（`copyKeys`）。

## 词汇对照

| en | fr | 备注 |
|---|---|---|
| Download | Téléchargement |  |
| Downloading | Téléchargement en cours |  |
| Upload | Envoi | `Envoi`，不用 `Téléversement` |
| Seeding | Partage | qBittorrent fr 用语 `Partage` |
| Seed | Source | qBittorrent fr：`Source` |
| Peer | Pair |  |
| Leech | Leecher |  |
| Tracker | Tracker |  |
| Task | Tâche |  |
| Task group | Groupe de tâches |  |
| Queue | File d’attente |  |
| Category | Catégorie |  |
| File | Fichier |  |
| Folder | Dossier |  |
| Path | Chemin |  |
| File name | Nom du fichier |  |
| Save location | Emplacement d’enregistrement |  |
| Settings | Paramètres |  |
| Default | Par défaut |  |
| Advanced | Avancé |  |
| Speed limit | Limite de vitesse |  |
| Threads | Threads |  |
| Connections | Connexions |  |
| Segment | Segment |  |
| Plugin | Plugin | 保留 `Plugin`；浏览器扩展是 `Extension` |
| Browser extension | Extension du navigateur |  |
| Component | Composant |  |
| Device | Appareil |  |
| Account | Compte |  |
| Sign in | Se connecter |  |
| Sign out | Se déconnecter |  |
| Sync | Synchronisation |  |
| Notification | Notification |  |
| Theme | Thème |  |
| Appearance | Apparence |  |
| Language | Langue |  |
| Tray | Zone de notification |  |
| Shortcut | Raccourci |  |
| Update | Mise à jour |  |
| Subscription | Abonnement |  |
| Feed | Flux |  |
| Quota | Quota |  |
| Diagnostics | Diagnostic |  |
| Log | Journal |  |
| Pause | Mettre en pause |  |
| Resume | Reprendre |  |
| Start | Démarrer |  |
| Cancel | Annuler |  |
| Confirm | Confirmer |  |
| OK | OK |  |
| Delete | Supprimer |  |
| Remove | Retirer |  |
| Completed | Terminé |  |
| Failed | Échec | 状态名 `Échec` |
| Paused | En pause |  |
| Direct | Direct |  |
| Dedicated proxy | Proxy dédié |  |
| Blocklist | Liste de blocage |  |
| Allowlist | Liste d’autorisation |  |
| Access key | Clé d’accès |  |
| Pairing | Association |  |
| Ratio | Ratio |  |
| Host | Hôte |  |

## qwen-mt 机器翻译配置

`scripts/i18n-mt` 只读取本节的 JSON 代码块；改上文术语时同步这里。

```json
{
  "targetLang": "French",
  "source": "en",
  "domains": "UI strings of FluxDown, a desktop and mobile download manager (HTTP, BitTorrent, eD2K). Standard French (France), terminology aligned with Windows and Android French UI. Formal vous in messages and hints, never tu; buttons and menus in infinitive (Télécharger, Supprimer, Mettre en pause). Sentence case, never English-style Title Case; no final period on buttons, menus and labels, full sentences end with a period. French typography: non-breaking space before : ; ? !, guillemets « », curly apostrophe ’, ellipsis …. Decimal comma, space between number and unit (do not change units such as GB, MB/s), percent with space: {percent} %. Keep {placeholder} names unchanged.",
  "siteDomains": "Website and documentation of FluxDown, a free open-source download manager (HTTP, BitTorrent, eD2K) for desktop, mobile, NAS and servers. Standard French (France), clear and natural marketing / technical documentation tone; address the reader as vous. Sentence case for headings; French typography (non-breaking space before : ; ? !, guillemets « »). Do not translate code, commands, file paths, config keys, environment variables, URLs, product names or placeholders such as {0} or {name}; keep HTML tags and Markdown markup unchanged.",
  "keep": [
    "FluxDown", "fluxdown_engine", "fluxdownd", "fluxdown-agent", "ZerxLabBot", "BitTorrent", "BT", "eD2K", "Kad", "aria2", "yt-dlp", "FFmpeg",
    "HLS", "DASH", "M3U8", "RSS", "WebDAV", "SMB", "DHT", "PEX", "uTP", "UPnP", "MSE", "HTTP",
    "HTTPS", "FTP", "SOCKS5", "TLS", "CDN", "LAN", "VPN", "TUN", "Wi-Fi", "NMH", "ComSpec", "%SystemRoot%\\system32\\cmd.exe",
    "chrome://restart", "FLUXDOWN_BIND", "PUID/PGID", "noexec", "loopback", "link-local", "carrier-grade NAT", "HMAC-SHA256", "X-FluxDown-Signature", "CIDR", "IPv4", "IPv6",
    "infohash", "peer ID", "Referer", "User-Agent", "cookie", "ntfy", "Bark", "WeCom", "Feishu", "DingTalk", "Telegram", "Discord",
    "Slack", "Gotify", "Home Assistant", "NAS", "Docker", "macOS", "Windows", "Linux", "Android", "iOS", "Chrome", "Edge",
    "Firefox", "Safari", "GitHub", "Torrent", "Magnet", "nodes.dat", "ipfilter.dat", "server.met", "JSON", "Markdown", "API", "MCP",
    "CLI", "2xx", "3xx", "4xx", "5xx", "Webhook", "Endpoint", "Push", "Ratio"
  ],
  "terms": {
    "Download": "Téléchargement",
    "Downloading": "Téléchargement en cours",
    "Upload": "Envoi",
    "Seeding": "Partage",
    "Seed": "Source",
    "Peer": "Pair",
    "Leech": "Leecher",
    "Tracker": "Tracker",
    "Task": "Tâche",
    "Task group": "Groupe de tâches",
    "Queue": "File d’attente",
    "Category": "Catégorie",
    "File": "Fichier",
    "Folder": "Dossier",
    "Path": "Chemin",
    "File name": "Nom du fichier",
    "Save location": "Emplacement d’enregistrement",
    "Settings": "Paramètres",
    "Default": "Par défaut",
    "Advanced": "Avancé",
    "Speed limit": "Limite de vitesse",
    "Threads": "Threads",
    "Connections": "Connexions",
    "Segment": "Segment",
    "Plugin": "Plugin",
    "Browser extension": "Extension du navigateur",
    "Component": "Composant",
    "Device": "Appareil",
    "Account": "Compte",
    "Sign in": "Se connecter",
    "Sign out": "Se déconnecter",
    "Sync": "Synchronisation",
    "Notification": "Notification",
    "Theme": "Thème",
    "Appearance": "Apparence",
    "Language": "Langue",
    "Tray": "Zone de notification",
    "Shortcut": "Raccourci",
    "Update": "Mise à jour",
    "Subscription": "Abonnement",
    "Feed": "Flux",
    "Quota": "Quota",
    "Diagnostics": "Diagnostic",
    "Log": "Journal",
    "Pause": "Mettre en pause",
    "Resume": "Reprendre",
    "Start": "Démarrer",
    "Cancel": "Annuler",
    "Confirm": "Confirmer",
    "OK": "OK",
    "Delete": "Supprimer",
    "Remove": "Retirer",
    "Completed": "Terminé",
    "Failed": "Échec",
    "Paused": "En pause",
    "Direct": "Direct",
    "Dedicated proxy": "Proxy dédié",
    "Blocklist": "Liste de blocage",
    "Allowlist": "Liste d’autorisation",
    "Access key": "Clé d’accès",
    "Pairing": "Association",
    "Ratio": "Ratio",
    "Host": "Hôte"
  },
  "copyKeys": [
    "searchKeywords*", "languageChinese", "webhookTemplatePlaceholder", "urlPlaceholder", "rssProviderBuiltin",
    "rssCookiesHint", "donateDate", "mobileReferrer", "apiServiceApi", "extension*Store"
  ]
}
```
