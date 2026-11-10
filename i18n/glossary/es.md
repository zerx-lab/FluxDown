# es (Español): glosario de traducción

Glosario de la localización española de FluxDown. Se aplica a cualquier cambio en
`assets/i18n/es.json` para que la terminología no se disperse. La localización es
completa: los 3715 valores de `en.json` están presentes y no se usa el respaldo
por clave (`per-key fallback`).

## Principios

- **El significado se toma de `en.json`**; `zh.json` solo sirve para desambiguar
  contextos en los que el inglés es ambiguo.
- Español de España, neutro y natural: `añadir` (no `agregar`), `ordenador`
  (no `computadora`), `móvil` (no `celular`), `vídeo`.
- Trato de **tú** en mensajes y avisos (`puedes`, `inténtalo`, `comprueba`);
  botones, menús y acciones en infinitivo o sustantivo (`Guardar`, `Eliminar`,
  `Descargar`). No mezclar `tú` y `usted` dentro del mismo texto.
- **Sentence case**: mayúscula solo en la primera palabra y en nombres propios.
  Botones, menús y etiquetas sin punto final; frases completas con punto.
- Comillas `“ ”`; puntos suspensivos `…`; raya `—`; separador ` · ` como en
  inglés. No usar comillas rectas salvo que formen parte de un valor técnico.
- Placeholders `{name}`: mismo nombre, mismo número y misma grafía que en
  `en.json`. Se pueden reordenar, pero nunca traducir ni renombrar. No introducir
  llaves sueltas: la prueba `translated_locales_keep_baseline_placeholders`
  compara el conjunto de placeholders con la base inglesa.
- Números y unidades: coma decimal (`1,5 GB`), espacio entre el número y la
  unidad; porcentaje `{percent} %`; no cambiar `KB/s`, `MB/s`, `MiB`, `KiB/s`, etc.
- Duraciones humanizadas: `min`, `h`, `d`, `s`, según el contexto
  (`en` usa `min`, `hr`, `day`, `sec`).
- No dejar texto en inglés salvo los valores indicados en «No traducir».

## No traducir (se conserva tal cual)

FluxDown, `fluxdown_engine`, `fluxdownd`, `fluxdown-agent`, ZerxLabBot, BitTorrent,
**BT**, eD2K, Kad, aria2, yt-dlp, FFmpeg, HLS, DASH, M3U8, RSS, WebDAV, SMB, DHT,
PEX, uTP, UPnP, **MSE**, HTTP(S), FTP, SOCKS5, TLS, CDN, LAN, VPN, TUN, Wi-Fi,
**NMH** (native messaging host, no desarrollar la sigla), **ComSpec**,
`%SystemRoot%\system32\cmd.exe`, `chrome://restart`, `FLUXDOWN_BIND`,
`PUID/PGID`, `noexec`, `loopback`, `link-local`, `carrier-grade NAT`,
HMAC-SHA256, `X-FluxDown-Signature`, CIDR, IPv4/IPv6, infohash, peer ID, Referer,
**User-Agent**, cookie, ntfy, Bark, WeCom, Feishu, DingTalk, Telegram, Discord,
Slack, Gotify, Home Assistant, NAS, Docker, macOS, Windows, Linux, Android, iOS,
Chrome, Edge, Firefox, Safari, GitHub, Torrent, Magnet, `nodes.dat`,
`ipfilter.dat`, `server.met`, JSON, Markdown, API, MCP, CLI, 2xx/3xx/4xx/5xx.

- `searchKeywords*`: se quedan en inglés. Son índices internos de búsqueda, no
  texto de interfaz. `searchKeywordsSilentSkipSelection` conserva además los
  términos chinos del original, exactamente como en `en.json`.
- `languageChinese`: `中文`.
- `webhookTemplatePlaceholder`: el ejemplo JSON se conserva tal cual; solo los
  placeholders son dinámicos.
- `urlPlaceholder`, `rssProviderBuiltin`, `rssCookiesHint`, `donateDate`,
  `mobileReferrer`, `apiServiceApi` y los nombres de las tiendas de extensiones
  son valores o formatos del software: se mantienen como en `en.json`.
- `languageEnglish`: se muestra como `English` (nombre nativo del idioma), igual
  que `中文`; el nombre de esta localización es `Español`.

## Diccionario

| en | es | Nota |
|---|---|---|
| Download (sust./verbo) | Descarga / Descargar | Estado: `Descargando` |
| Upload | Subida / Subir | `Límite de subida` |
| Seeding | Siembra / Sembrando | Filtro y estado: `Sembrando` |
| Seed / Peer / Leech | Semilla / Par / Sanguijuela | Terminología de BitTorrent |
| Anti-leech | Anti-sanguijuela | Protección `antisanguijuelas` |
| Observe / enforce | Observar / Aplicar | Modos de Guard |
| Throttle | Limitar | Acción de Guard |
| Choke | Bloquear la subida | Matiz de «dejar de subir» |
| Ban | Bloqueo | `Bloqueo de 1 hora` |
| Blocklist / Allowlist | Lista de bloqueados / Lista de permitidos | |
| Evidence | Evidencia | `Ver evidencia`, `Envío de evidencia` |
| Strike | Aviso | Estadística de Guard |
| Task / Task group | Tarea / Grupo de tareas | |
| Queue | Cola | |
| Category | Categoría | |
| File / Folder / Path | Archivo / Carpeta / Ruta | |
| Extension | Extensión | La del navegador: `extensión del navegador` |
| Save location | Ubicación de guardado | Campo: `Guardar en` |
| Settings | Configuración | |
| Default | Predeterminado | |
| Advanced | Avanzado | |
| Speed limit | Límite de velocidad | |
| Threads | Hilos | Segmentos de descarga HTTP |
| Connection(s) | Conexión / Conexiones | |
| Segment | Segmento | `Detalles de segmentos` |
| Proxy | Proxy | |
| Dedicated proxy | Proxy dedicado | |
| Plugin | Complemento | |
| Resolver | Resolutor | |
| Browser extension | Extensión del navegador | |
| Component | Componente | FFmpeg / yt-dlp |
| Device | Dispositivo | |
| Account / Sign in / Sign out | Cuenta / Iniciar sesión / Cerrar sesión | |
| Membership / Membership card | Membresía / Tarjeta de membresía | |
| Quota | Cuota | |
| Sync | Sincronización | |
| Notification | Notificación | |
| Push | Push / Notificación push | |
| Webhook / Endpoint | Webhook / Endpoint | No usar «punto final» |
| Delivery | Entrega | `Registro de entregas` |
| Diagnostics (Doctor) | Diagnóstico | |
| Log | Registro | No usar «log» |
| Language | Idioma | `languageNativeName` = `Español` |
| Theme / Appearance | Tema / Apariencia | |
| Tray | Bandeja del sistema | `Menú de la bandeja` |
| Shortcut | Atajo | Pestaña: `Atajos de teclado` |
| Update | Actualización / Actualizar | |
| Subscription (RSS) | Suscripción | |
| Feed | Fuente | `Fuente RSS` |
| Tracker | Rastreador | |
| Host | Host | Motor remoto |
| Pairing / paired | Emparejamiento / emparejado | |
| Access key | Clave de acceso | Acceso web / servidor |
| Manifest (file list) | Manifiesto (lista de archivos) | |
| Placeholder | Marcador de posición | |
| Ratio | Ratio | Se mantiene el anglicismo |

## Diferencias intencionadas respecto a en

- `searchKeywords*`, `languageChinese`, `webhookTemplatePlaceholder`,
  `urlPlaceholder`, `rssProviderBuiltin`, `rssCookiesHint`, `donateDate`,
  `mobileReferrer`, `apiServiceApi` y los nombres de tiendas se conservan como
  en `en.json`: son valores del software, formatos o índices de búsqueda.
- `Wi-Fi` usa un guion normal; en inglés aparece a veces el guion duro U+2011
  (`Wi‑Fi`), pero la semántica es la misma.
- Los formatos de fecha con placeholders (`donateDate`) no se reordenan: son
  plantillas del software y deben coincidir con las de `en.json`.
