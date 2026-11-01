# tr（Türkçe）翻译术语表

ZerxLabBot 翻译自动化（`i18n` 任务）翻译 `assets/i18n/tr.json` 时必读；人工修正术语时也以本表为准。用词对齐 Windows / Android 土耳其语界面与 qBittorrent 土耳其语译文的通行说法。

## 原则

- **语义以 `en.json` 为准**；`zh.json` 只用于理解语境（en 有歧义时看中文怎么说）。
- 标准土耳其语，正确使用 ç ğ ı İ ö ş ü；注意 i / İ、ı / I 的大小写（`İndir`，不是 `Indir`）。
- 语气：中性礼貌。按钮、菜单用动词原形命令式（`İndir`、`Duraklat`、`Sil`）；说明与提示句用礼貌第二人称复数或无人称（`…seçin`、`…yapılabilir`），不用 `sen` 语气。
- 标题与按钮用句首大写（Sentence case），不要每个词首字母大写；按钮、菜单项、字段名不加句号。
- `{name}` 占位符原样保留，可调整语序；占位符后接格后缀时用撇号：`{name}'e`、`{count} dosya`（数量后的名词用单数）。
- 数字与单位：小数点用逗号（`1,5 GB`），数值与单位之间留空格；百分号前置：`%{percent}`（en 为 `{percent}%` 时改成 `%{percent}`）。
- 引号用 `“ ”`；省略号用 `…`。

## 不翻译（原样保留）

FluxDown、FluxCloud、ZerxLabBot、BitTorrent、BT、eD2K、aria2、yt-dlp、FFmpeg、HLS、DASH、M3U8、RSS、Webhook、Telegram、WebDAV、SMB、DHT、UPnP、uTP、PEX、HTTP、HTTPS、FTP、SOCKS5、URL、API、MCP、JSON、CLI、NAS、Docker、Windows、macOS、Linux、Android、iOS、Chrome、Edge、Firefox、Safari、GitHub、Torrent、Magnet（磁力链接写 `Magnet bağlantısı`）。

## 词汇对照

| en | tr | 备注 |
|---|---|---|
| Download（名词 / 动词） | İndirme / İndir | 「下载中」状态 `İndiriliyor` |
| Upload | Yükleme / Yükle | 上传速度 `Yükleme hızı` |
| Seeding | Paylaşılıyor | 与「做种中」侧栏过滤器一致 |
| Completed / Failed / Paused | Tamamlandı / Başarısız / Duraklatıldı | 状态名 |
| Pause / Resume / Start | Duraklat / Sürdür / Başlat | |
| Cancel / Confirm / OK | İptal / Onayla / Tamam | |
| Delete / Remove | Sil / Kaldır | |
| Task | Görev | |
| Task group | Görev grubu | |
| Queue | Kuyruk | |
| Category | Kategori | |
| File / Folder / Path | Dosya / Klasör / Yol | |
| File name / Extension | Dosya adı / Uzantı | 浏览器扩展也是 `Uzantı`，靠语境区分 |
| Save location | Kayıt konumu | |
| Settings | Ayarlar | |
| Default | Varsayılan | |
| Advanced | Gelişmiş | |
| Speed limit | Hız sınırı | |
| Threads / Connections | İş parçacığı / Bağlantı | 指连接数时用 `Bağlantı` |
| Segment | Parça | |
| Proxy | Proxy | 解释性语境可写 `Proxy sunucusu` |
| Direct | Doğrudan | |
| Plugin | Eklenti | |
| Browser extension | Tarayıcı uzantısı | |
| Component | Bileşen | |
| Device | Cihaz | |
| Account / Sign in / Sign out | Hesap / Oturum aç / Oturumu kapat | |
| Sync | Eşitleme | |
| Notification | Bildirim | |
| Theme / Appearance | Tema / Görünüm | |
| Language | Dil | |
| Tray | Sistem tepsisi | |
| Shortcut | Kısayol | |
| Update | Güncelleme | |
| Subscription（RSS） | Abonelik | |
| Feed | Akış | |
| Tracker | İzleyici | |
| Peer | Eş | |
| Plan（套餐） | Plan | |
| Quota | Kota | |
| Diagnostics | Tanılama | |
| Log | Günlük | |
