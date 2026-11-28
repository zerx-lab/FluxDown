# pt-BR（Português do Brasil）翻译术语表

GitHub Actions 工作流 `.github/workflows/i18n-mt.yml`（qwen-mt）翻译 `assets/i18n/pt-BR.json` 与官网 / 文档时读取本表；人工修正术语时也以本表为准。用词对齐 Windows / Android 与 qBittorrent 该语言译文的通行说法。

## 原则

- **语义以 `en.json` 为准**；`zh.json` 只用于理解语境（en 有歧义时看中文怎么说）。
- **巴西葡萄牙语（pt-BR）**，不是欧洲葡萄牙语：`arquivo`（非 `ficheiro`）、`baixar`（非 `descarregar`）、`tela`（非 `ecrã`）、`celular`（非 `telemóvel`）、`pasta`、`salvar`（非 `guardar`）。
- 称呼：**você**（`você pode`、`verifique`）；不用 `tu`，不用 `o senhor`。按钮、菜单、动作用动词不定式或名词（`Salvar`、`Excluir`、`Pausar`）。
- 句首大写（Sentence case），不做英语式 Title Case；按钮、菜单项、字段名不加句号，完整句子以句号结尾。
- 引号用 `“ ”`；省略号用 `…`。
- 数字与单位：小数点用逗号（`1,5 GB`），数值与单位之间留空格；百分号紧跟数值：`{percent}%`；不改 `KB/s`、`MB/s` 等单位。
- `{name}` 占位符原样保留，可调整语序；注意 `de`/`do`/`da`、`em`/`no`/`na` 的缩合，占位符前后必要时改写句子避开。
- 保留的英语借词：`Webhook`、`Endpoint`、`Push`、`Download`/`Upload`（名词）、`Tracker`、`Plugin`；动词用 `baixar`、`enviar`。
- `{name}` 占位符原样保留（同名、同数量），不翻译、不改名，不引入多余花括号。

## 不翻译（原样保留）

FluxDown、fluxdown_engine、fluxdownd、fluxdown-agent、ZerxLabBot、BitTorrent、BT、eD2K、Kad、aria2、yt-dlp、FFmpeg、HLS、DASH、M3U8、RSS、WebDAV、SMB、DHT、PEX、uTP、UPnP、MSE、HTTP、HTTPS、FTP、SOCKS5、TLS、CDN、LAN、VPN、TUN、Wi-Fi、NMH、ComSpec、%SystemRoot%\system32\cmd.exe、chrome://restart、FLUXDOWN_BIND、PUID/PGID、noexec、loopback、link-local、carrier-grade NAT、HMAC-SHA256、X-FluxDown-Signature、CIDR、IPv4、IPv6、infohash、peer ID、Referer、User-Agent、cookie、ntfy、Bark、WeCom、Feishu、DingTalk、Telegram、Discord、Slack、Gotify、Home Assistant、NAS、Docker、macOS、Windows、Linux、Android、iOS、Chrome、Edge、Firefox、Safari、GitHub、Torrent、Magnet、nodes.dat、ipfilter.dat、server.met、JSON、Markdown、API、MCP、CLI、2xx、3xx、4xx、5xx、Webhook、Endpoint、Push。

- `searchKeywords*`、`languageChinese`、`webhookTemplatePlaceholder`、`urlPlaceholder`、`rssProviderBuiltin`、`rssCookiesHint`、`donateDate`、`mobileReferrer`、`apiServiceApi`、各扩展商店名（`extension*Store`）是软件值或搜索索引，原样复制（`copyKeys`）。

## 词汇对照

| en | pt-BR | 备注 |
|---|---|---|
| Download | Download | 名词；状态 `Baixando`；动词用 `Baixar` |
| Downloading | Baixando |  |
| Upload | Upload | 名词；动词用 `Enviar` |
| Seeding | Semeando |  |
| Seed | Semente |  |
| Peer | Par |  |
| Leech | Leecher |  |
| Tracker | Tracker | 沿用 qBittorrent pt-BR |
| Task | Tarefa |  |
| Task group | Grupo de tarefas |  |
| Queue | Fila |  |
| Category | Categoria |  |
| File | Arquivo |  |
| Folder | Pasta |  |
| Path | Caminho |  |
| File name | Nome do arquivo |  |
| Save location | Local de salvamento |  |
| Settings | Configurações |  |
| Default | Padrão |  |
| Advanced | Avançado |  |
| Speed limit | Limite de velocidade |  |
| Threads | Threads |  |
| Connections | Conexões |  |
| Segment | Segmento |  |
| Plugin | Plugin |  |
| Browser extension | Extensão do navegador |  |
| Component | Componente |  |
| Device | Dispositivo |  |
| Account | Conta |  |
| Sign in | Entrar |  |
| Sign out | Sair |  |
| Sync | Sincronização |  |
| Notification | Notificação |  |
| Theme | Tema |  |
| Appearance | Aparência |  |
| Language | Idioma |  |
| Tray | Bandeja do sistema |  |
| Shortcut | Atalho |  |
| Update | Atualização |  |
| Subscription | Assinatura |  |
| Feed | Feed |  |
| Quota | Cota | `Cota`（巴西拼写） |
| Diagnostics | Diagnóstico |  |
| Log | Registro |  |
| Pause | Pausar |  |
| Resume | Retomar |  |
| Start | Iniciar |  |
| Cancel | Cancelar |  |
| Confirm | Confirmar |  |
| OK | OK |  |
| Delete | Excluir |  |
| Remove | Remover |  |
| Completed | Concluído |  |
| Failed | Falhou |  |
| Paused | Pausado |  |
| Direct | Direto |  |
| Dedicated proxy | Proxy dedicado |  |
| Blocklist | Lista de bloqueio |  |
| Allowlist | Lista de permissões |  |
| Access key | Chave de acesso |  |
| Pairing | Pareamento |  |
| Ratio | Proporção |  |

## qwen-mt 机器翻译配置

`scripts/i18n-mt` 只读取本节的 JSON 代码块；改上文术语时同步这里。

```json
{
  "targetLang": "Portuguese",
  "source": "en",
  "domains": "UI strings of FluxDown, a desktop and mobile download manager (HTTP, BitTorrent, eD2K). Brazilian Portuguese (pt-BR), not European Portuguese: arquivo, baixar, tela, celular, salvar. Address the user as você, never tu; buttons and menus in infinitive or noun form (Salvar, Excluir, Pausar). Sentence case, never English-style Title Case; no final period on buttons, menus and labels, full sentences end with a period. Curly quotes “ ”, ellipsis …. Decimal comma (1,5 GB), space between number and unit, percent attached: {percent}%. Keep {placeholder} names unchanged.",
  "siteDomains": "Website and documentation of FluxDown, a free open-source download manager (HTTP, BitTorrent, eD2K) for desktop, mobile, NAS and servers. Brazilian Portuguese (pt-BR), not European Portuguese; clear and natural marketing / technical documentation tone; address the reader as você. Sentence case for headings. Do not translate code, commands, file paths, config keys, environment variables, URLs, product names or placeholders such as {0} or {name}; keep HTML tags and Markdown markup unchanged.",
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
    "Downloading": "Baixando",
    "Upload": "Upload",
    "Seeding": "Semeando",
    "Seed": "Semente",
    "Peer": "Par",
    "Leech": "Leecher",
    "Tracker": "Tracker",
    "Task": "Tarefa",
    "Task group": "Grupo de tarefas",
    "Queue": "Fila",
    "Category": "Categoria",
    "File": "Arquivo",
    "Folder": "Pasta",
    "Path": "Caminho",
    "File name": "Nome do arquivo",
    "Save location": "Local de salvamento",
    "Settings": "Configurações",
    "Default": "Padrão",
    "Advanced": "Avançado",
    "Speed limit": "Limite de velocidade",
    "Threads": "Threads",
    "Connections": "Conexões",
    "Segment": "Segmento",
    "Plugin": "Plugin",
    "Browser extension": "Extensão do navegador",
    "Component": "Componente",
    "Device": "Dispositivo",
    "Account": "Conta",
    "Sign in": "Entrar",
    "Sign out": "Sair",
    "Sync": "Sincronização",
    "Notification": "Notificação",
    "Theme": "Tema",
    "Appearance": "Aparência",
    "Language": "Idioma",
    "Tray": "Bandeja do sistema",
    "Shortcut": "Atalho",
    "Update": "Atualização",
    "Subscription": "Assinatura",
    "Feed": "Feed",
    "Quota": "Cota",
    "Diagnostics": "Diagnóstico",
    "Log": "Registro",
    "Pause": "Pausar",
    "Resume": "Retomar",
    "Start": "Iniciar",
    "Cancel": "Cancelar",
    "Confirm": "Confirmar",
    "OK": "OK",
    "Delete": "Excluir",
    "Remove": "Remover",
    "Completed": "Concluído",
    "Failed": "Falhou",
    "Paused": "Pausado",
    "Direct": "Direto",
    "Dedicated proxy": "Proxy dedicado",
    "Blocklist": "Lista de bloqueio",
    "Allowlist": "Lista de permissões",
    "Access key": "Chave de acesso",
    "Pairing": "Pareamento",
    "Ratio": "Proporção"
  },
  "copyKeys": [
    "searchKeywords*", "languageChinese", "webhookTemplatePlaceholder", "urlPlaceholder", "rssProviderBuiltin",
    "rssCookiesHint", "donateDate", "mobileReferrer", "apiServiceApi", "extension*Store"
  ]
}
```
