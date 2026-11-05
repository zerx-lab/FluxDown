# Глоссарий русской локали (ru)

Глоссарий русской локали FluxDown. Написан по образцу `tr.md`; действует при любых правках `assets/i18n/ru.json`, чтобы термины не расходились. Локаль полная (все 3715 ключей `en.json`), per-key fallback на en не используется.

## Правила

- **Смысл берётся из `en.json`**; `zh.json` — только для контекста, где en двусмысленен.
- Стандартный русский, живой, но без жаргона: слово «шорткат» запрещено (только «сочетание клавиш»); «лич» допустим — это устоявшийся термин BitTorrent в контексте анти-лича.
- Кавычки **«…»**; `“ ”` из en заменяются на «…».
- Диапазоны — эн-дэш без пробелов (`1–100`); знак умножения — `×` (`3×`); перечисления внутри строки — ` · ` (как в en); многоточие — `…`.
- Плейсхолдеры `{name}` — тот же набор и регистр, что в en (переставлять порядок слов можно). Лишние одиночные `{`/`}` запрещены: upstream-тест `translated_locales_keep_baseline_placeholders` принимает НАБОР плейсхолдеров между en-базой и локалью.
- Числа и единицы: пробел между числом и единицей; проценты слитно (`{percent}%`). Единицы — русские приставки при IEC/метрических суффиксах: `МиБ`, `КиБ/с`, `МБ/с`, `ГБ/с`.
- Буква «ё» в уже переведённых строках не заменяется на «е» при правках.
- Технотермины — латиницей; имена файлов, команд и путей не переводить принудительно.

## Не переводить (остаётся латиницей)

FluxDown, `fluxdown_engine`, `fluxdownd`, `fluxdown-agent`, ZerxLabBot, BitTorrent, **BT**, eD2K, Kad, aria2, yt-dlp, FFmpeg, HLS, DASH, M3U8, RSS, WebDAV, SMB, DHT, PEX, uTP, UPnP, **MSE**, HTTP(S), FTP, SOCKS5, TLS, CDN, LAN, VPN, TUN, Wi-Fi, **NMH** (native messaging host — не расшифровывать), **ComSpec**, `%SystemRoot%\system32\cmd.exe`, `chrome://restart`, `FLUXDOWN_BIND`, `PUID/PGID`, `noexec`, `loopback`, `link-local`, `carrier-grade NAT`, HMAC-SHA256, `X-FluxDown-Signature`, CIDR, IPv4/IPv6, infohash, peer ID, Referer, **User-Agent**, cookie, ntfy, Bark, WeCom, Feishu, DingTalk, Telegram, Discord, Slack, Gotify, Home Assistant, NAS, Docker, macOS, Windows, Linux, Android, iOS, Chrome, Edge, Firefox, Safari, GitHub, Torrent, Magnet, `nodes.dat`, `ipfilter.dat`, `server.met`, JSON, Markdown, API, MCP, CLI, 2xx/3xx/4xx/5xx.

- Переменные шаблона вебхука вида `{task.fileName}` — только сами плейсхолдеры сохраняются, текст примера шаблона (`webhookTemplatePlaceholder`) остаётся как в en.
- `searchKeywords*` — остаются английскими (индексы поиска, не пользовательский текст).

## Словарь

| en | ru | Примечание |
|---|---|---|
| Download (сущ./гл.) | загрузка / скачать | «Скорость загрузки»; кнопка UI — «Скачать» |
| Upload | отдача | «Скорость отдачи»; мобильный чип — «Отдача» |
| Seeding | раздача | статус «Раздаётся», сводка — «Раздач: {n}» |
| Peer | пир | мн. ч. «пиры» |
| Leech(er) | лич | только в контексте анти-лича |
| Anti-leech | анти-лич | защита — «Защита от личей» |
| Observe / enforce | наблюдать / применять | режимы Guard: «Только наблюдение», «Применять» |
| Throttle | дросселировать | действие Guard |
| Choke | душить | "(перестать отдавать)" в подсказке |
| Ban | бан | «Бан на 1 час», «Забанен {group}» |
| Blocklist / Allowlist | список блокировок / список разрешённых | |
| Evidence | улики | «Посмотреть улики», «Передача улик» — не «доказательства» |
| Strike | страйк | статистика Guard («Наблюдаемые срабатывания» — отдельный термин) |
| Task | задача | |
| Task group | группа задач | |
| Queue | очередь | |
| Category | категория | |
| File / Folder / Path | файл / папка / путь | |
| Extension | расширение | файла; браузерное — «расширение браузера» |
| Save location | папка сохранения | поле — «Сохранять в» |
| Settings | Настройки | |
| Default | по умолчанию | не «дефолт» |
| Advanced | Дополнительно | |
| Speed limit | лимит скорости | |
| Threads | потоки | сегменты HTTP-загрузки |
| Connection(s) | соединение(я) | |
| Segment | сегмент | «детали сегментов» |
| Proxy | прокси | |
| Dedicated proxy | выделенный прокси | |
| Plugin | плагин | |
| Resolver | резолвер | |
| Browser extension | расширение браузера | |
| Component | компонент | FFmpeg / yt-dlp |
| Device | устройство | |
| Account / Sign in / Sign out | аккаунт / Войти / Выйти | не «учётная запись» |
| Membership / Membership card | участие / карта участия | показывать тариф — «план» (облачный пуш) |
| Quota | квота | |
| Sync | синхронизация | |
| Notification | уведомление | |
| Push | пуш | «Облачный пуш», «пуш вебхуков» |
| Webhook / Endpoint | вебхук / точка приёма | не «эндпоинт» |
| Delivery | доставка | «Журнал доставок» |
| Diagnostics (Doctor) | Диагностика | страница «Диагностика окружения» |
| Log | журнал | вкладка задачи и общие журналы — не «лог» |
| Language | Язык | посл. ключ `languageNativeName` = «Русский» |
| Theme / Appearance | тема / внешний вид | |
| Tray | трей | «меню трея» |
| Shortcut | сочетание клавиш | вкладка — «Сочетания клавиш» |
| Update | обновление | |
| Subscription (RSS) | подписка | |
| Feed | фид | |
| Tracker | трекер | |
| Host | хост | удалённый движок |
| Pairing / paired | сопряжение / сопряжённый | |
| Access key | ключ доступа | веб-вход / сервер |
| Manifest (file list) | список файлов | диалог выбора файлов |
| Placeholder | плейсхолдер | |

## Намеренные отличия от en

- `searchKeywords*` — английские (поисковые индексы), кроме `searchKeywordsSilentSkipSelection`, где в en смесь с китайскими словами — воспроизведено 1:1.
- `languageChinese: "中文"`, `第02话` в `rssSmartEpisodeDesc`, шаблон в `webhookTemplatePlaceholder` — сохранены как в en (примеры/данные).
- `Wi-Fi` — обычный дефис; в en местами стоит непереносимый U+2011 (`Wi‑Fi`), семантика та же.
- Время/размер в humanized-строках: `мин`, `ч`, `дн`, `с` учитывая контекст (en `min/hr/day/sec`).
