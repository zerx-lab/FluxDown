// 多语言（en / zh）—— LocaleProvider + useI18n()。
// - 解析顺序：localStorage `fluxdown.locale`（用户显式选择）→ 服务器默认语言
//   （无鉴权 `/ping` 的 language，实时求值：设置页保存的 `web_language` 优先，
//   未保存时回退部署环境 FLUXDOWN_LANG；登录页同样生效）→ 浏览器语言 → en。
// - 持久化：仅设置页的显式切换写 localStorage 并写穿服务器 config `web_language`
//   （PUT /api/v1/config）；采用服务器/浏览器默认值不落盘，服务器侧变更随时可生效。
// - 后端返回：wire message 是稳定英文契约（CLI/客户端字符串匹配），不按语言变体；
//   展示层经 translateBackendMessage() 按当前语言映射。

import { createContext, useCallback, useContext, useEffect, useMemo, useState } from 'react'
import { api } from './api'

export type Locale = 'en' | 'zh'

const LOCALE_KEY = 'fluxdown.locale'
/** 服务器 config 表中的语言键（设置页写穿）。 */
export const LANGUAGE_CONFIG_KEY = 'web_language'

// ---------------------------------------------------------------------------
// 字典
// ---------------------------------------------------------------------------

const en = {
  // common
  'common.cancel': 'Cancel',
  'common.confirm': 'OK',
  'common.gotIt': 'Got it',
  'common.confirmTitle': 'Confirm',
  'common.noticeTitle': 'Notice',
  'common.close': 'Close',
  'common.back': 'Back',
  'common.delete': 'Delete',
  'common.pause': 'Pause',
  'common.resume': 'Resume',
  'common.retry': 'Retry',
  'common.done': 'Done',
  'common.loading': 'Loading…',
  'common.copy': 'Copy',
  'common.copied': 'Copied',
  'common.selectAll': 'Select all',
  'common.optional': 'Optional',
  'common.custom': 'Custom',
  'common.startDownload': 'Start download',
  'common.settings': 'Settings',
  'common.logout': 'Log out',
  'common.unknown': 'Unknown',

  // login
  'login.title': 'Connect to FluxDown Server',
  'login.subtitle': 'Downloads, Supercharged. — Manage your download engine remotely',
  'login.serverAddress': 'Server address',
  'login.token': 'Access token',
  'login.remember': 'Remember this device',
  'login.connecting': 'Connecting…',
  'login.connect': 'Connect',
  'login.hint': 'Generate the token in server Settings → Security & Access; connect over LAN or HTTPS via a reverse proxy.',
  'login.connectFailed': 'Cannot reach the server, please check the address',
  'login.invalidToken': 'Invalid token',
  'login.featEngine': 'Rust engine',
  'login.featEngineDesc': 'HTTP · FTP · BT · HLS · DASH',
  'login.featRealtime': 'Realtime push',
  'login.featRealtimeDesc': 'WebSocket progress / segment splits',
  'login.featPrivacy': 'Zero tracking',
  'login.featPrivacyDesc': 'No account · data stays on your server',

  // status
  'status.pending': 'Queued',
  'status.downloading': 'Downloading',
  'status.paused': 'Paused',
  'status.completed': 'Completed',
  'status.error': 'Error',
  'status.preparing': 'Preparing',
  'status.preparingEllipsis': 'Preparing…',
  'status.downloadFailed': 'Download error',
  'status.eta': 'ETA {eta}',

  // time groups
  'time.today': 'Today',
  'time.yesterday': 'Yesterday',
  'time.thisWeek': 'This week',
  'time.thisMonth': 'This month',
  'time.older': 'Older',
  'time.secs': '{s} s',
  'time.minSecs': '{m} min {s} s',
  'time.hourMin': '{h} h {m} min',

  // file types
  'type.all': 'All',
  'type.video': 'Video',
  'type.audio': 'Audio',
  'type.document': 'Documents',
  'type.image': 'Images',
  'type.archive': 'Archives',
  'type.other': 'Other',

  // sidebar
  'sidebar.fileTypes': 'File types',
  'sidebar.queues': 'Queues',
  'sidebar.newQueue': 'New queue',
  'sidebar.newQueuePrompt': 'New queue name',
  'sidebar.allTasks': 'All tasks',
  'sidebar.deleteQueue': 'Delete queue',
  'sidebar.deleteQueueMsg': 'Delete queue "{name}"? Its tasks will move to the default queue.',
  'sidebar.idle': 'Idle',
  'sidebar.connected': 'Connected',
  'sidebar.connectedRtt': 'Connected · {rtt}ms',
  'sidebar.connecting': 'Connecting…',
  'sidebar.disconnected': 'Disconnected',
  'sidebar.feedback': 'Feedback',
  'sidebar.logoutTitle': 'Log out',
  'sidebar.logoutMsg': 'Disconnect from the server and clear the token saved on this device; you will need to sign in again next time.',
  'sidebar.version': 'Version {version}',
  'sidebar.newVersion': 'New version {version}',

  // topbar
  'topbar.searchPlaceholder': 'Search tasks… (Ctrl+F)',
  'topbar.manage': 'Batch manage',
  'topbar.pauseResumeAll': 'Pause / resume all',
  'topbar.newDownload': 'New download',
  'topbar.speedLimitOn': 'Global speed limit: {speed}',
  'topbar.speedLimitOff': 'Global speed limit: off',
  'topbar.goSettings': '{label} (adjust in Settings)',

  // status tabs
  'tabs.all': 'All',
  'tabs.downloading': 'Downloading',
  'tabs.completed': 'Completed',
  'tabs.paused': 'Paused',
  'tabs.error': 'Error',

  // task list
  'list.empty': 'No matching tasks',

  // manage bar
  'manage.selected': '{n} selected',
  'manage.deleteTitle': 'Delete tasks',
  'manage.deleteMsg': 'Delete the {n} selected tasks?',

  // task row / context menu
  'task.pause': 'Pause',
  'task.resume': 'Resume',
  'task.retry': 'Retry',
  'task.boost': 'Boost priority',
  'task.saveToLocal': 'Save to this device',
  'task.copyUrl': 'Copy download link',
  'task.copyPath': 'Copy file path on server',
  'task.moveToQueue': 'Move to queue…',
  'task.delete': 'Delete',
  'task.deleteWithFiles': 'Delete with files',
  'task.deleteTitle': 'Delete task',
  'task.deleteWithFilesMsg': 'Delete the task and its local files? This cannot be undone.',
  'task.deleteMsg': 'Delete this task?',

  // detail panel
  'detail.tabGeneral': 'General',
  'detail.tabSegments': 'Segments',
  'detail.tabQueue': 'Queue',
  'detail.tabLog': 'Log',
  'detail.tabAdvanced': 'Advanced',
  'detail.collapse': 'Collapse panel',
  'detail.expand': 'Click to expand',
  'detail.collapseValue': 'Click to collapse',
  'detail.downloaded': 'Downloaded',
  'detail.totalSize': 'Total size',
  'detail.speed': 'Speed',
  'detail.eta': 'ETA',
  'detail.remaining': 'ETA {eta}',
  'detail.fileName': 'File name',
  'detail.url': 'URL',
  'detail.savePath': 'Save path',
  'detail.createdAt': 'Created',
  'detail.status': 'Status',
  'detail.defaultQueue': 'Default queue',
  'detail.currentQueue': 'Current queue',
  'detail.moveTo': 'Move to',
  'detail.segments': 'segments',
  'detail.segmentCount': '{n} segments',
  'detail.noSegments': 'No segment data',
  'detail.logEmpty': 'No events this session',
  'detail.checksum': 'Checksum',
  'detail.proxy': 'Proxy',
  'detail.taskId': 'Task ID',
  'detail.notSet': 'Not set',
  'detail.protoQueue': 'Protocol / Queue',
  'detail.threads': 'Threads',
  'detail.error': 'Error',
  'detail.segCleared': 'Task completed; segment data has been cleared from task_segments.',
  'detail.noSegmentsHint': 'No segment data yet (waiting for engine probe or assignment).',
  'detail.segAdvisorSummary': '{n} segments · segment_advisor decides dynamically',
  'detail.segFooterNote':
    'Slow segments are proactively split / rescued by segment_coordinator; SegmentSplit events are pushed live over WebSocket and trigger the list animation.',
  'detail.currentQueueValue': 'Current: {name}',
  'detail.moveToOther': 'Move to another queue',
  'detail.noLimit': 'No limit',
  'detail.concurrency': 'Concurrency {n}',
  'detail.queueFooterNote': 'Each named queue has its own speed limit / concurrency / default directory / default thread settings.',
  'detail.currentStatus': 'Current status: {status}',
  'detail.logEmptyNote':
    'This panel only records status transitions and segment splits observed during this session; see the server-side logs/ directory for the full audit log.',
  'detail.checksumNotSet': 'Not set (verification skipped after download)',
  'detail.proxyNotSet': 'Follow global setting (Settings → Proxy)',
  'detail.checksumFooterNote': 'Checksum format: algo=hexhash (e.g. sha256=…); the engine verifies it on the server after the task completes.',

  // event log
  'event.statusChanged': 'Status: {from} → {to}',
  'event.errored': 'Error: {message}',
  'event.unknownError': 'Unknown error',
  'event.segmentSplit': 'Segment #{parent} {kind} split → new #{child} ({total} segments)',
  'event.proactive': 'proactive',
  'event.reactive': 'reactive',

  // status bar
  'statusbar.tasks': '{active} active · {total} tasks',
  'statusbar.limit': 'Speed limit:',
  'statusbar.limitOff': 'Off',
  'statusbar.demoMode': 'Demo mode',
  'statusbar.demoTitle': 'Demo mode: only {url} can be downloaded',
  'statusbar.diskTitle': 'Free disk space on server',
  'statusbar.diskFree': '{dir} · {free} free',

  // dialogs: new download
  'newDl.title': 'New Download',
  'newDl.urlLabel': 'Download links (one per line, HTTP / FTP / magnet / M3U8)',
  'newDl.urlLabelDemo': 'Download link (demo mode, locked)',
  'newDl.fileName': 'File name',
  'newDl.fileNamePlaceholder': 'Auto-detect from URL',
  'newDl.saveDir': 'Save directory',
  'newDl.segments': 'Threads',
  'newDl.segmentsAuto': 'Auto (segment_advisor)',
  'newDl.segmentsN': '{n} threads',
  'newDl.queue': 'Queue',
  'newDl.defaultQueue': 'Default queue',
  'newDl.userAgent': 'User-Agent',
  'newDl.globalDefault': 'Global default',
  'newDl.advanced': 'Advanced options',
  'newDl.cookies': 'Cookies',
  'newDl.headers': 'Custom headers',
  'newDl.headersAdd': 'Add header',
  'newDl.headerName': 'Header name',
  'newDl.headerValue': 'Value',
  'newDl.proxy': 'Task proxy',
  'newDl.checksum': 'Checksum',
  'newDl.checksumPlaceholder': 'e.g. sha256=abcdef…',
  'newDl.create': 'Start download',
  'newDl.desc': 'Enter download links and options to create one or more download tasks',
  'newDl.demoHint': 'Demo mode: only the file above can be downloaded; other links will be rejected by the server.',
  'newDl.lineError': 'Line {n} {line}: {error}',
  'newDl.creating': 'Creating…',

  // dialogs: fs picker
  'fs.browse': 'Browse',
  'fs.title': 'Choose save directory',
  'fs.desc': 'Browse the server file system and choose a save directory',
  'fs.up': 'Parent directory',
  'fs.loadFailed': 'Failed to read directory',
  'fs.emptyDir': 'No subdirectories here',
  'fs.choose': 'Use this directory',

  // dialogs: hls
  'hls.title': 'Select quality',
  'hls.desc': '{n} variants detected · highest bandwidth is used automatically after 60 s',
  'hls.variant': 'Variant {n}',

  // dialogs: bt
  'bt.title': 'Select files to download',
  'bt.summary': '{n} files · {size} total',
  'bt.selected': '{n} selected · {size}',

  // settings nav
  'set.general': 'General',
  'set.appearance': 'Appearance',
  'set.download': 'Download',
  'set.bt': 'BitTorrent',
  'set.proxy': 'Proxy',
  'set.security': 'Security & Access',
  'set.about': 'About',
  'set.title': 'Settings',
  'set.loadFailed': 'Failed to load configuration',

  // settings: general
  'set.general.desc': 'Server behavior, stored in the server config table',
  'set.general.maxConcurrent': 'Max concurrent tasks',
  'set.general.maxConcurrentDesc': 'Upper bound of simultaneously downloading tasks',
  'set.general.segments': 'Max connections',
  'set.general.segmentsDesc':
    'Per-task connection cap (ramped up gradually); 0 = decided by segment_advisor per file size',
  'set.general.autoMaxConn': 'Auto mode connection cap',
  'set.general.autoMaxConnDesc': 'Maximum connections the auto scheduler may use',
  'set.general.connPolicy': 'Learned server policies',
  'set.general.connPolicyDesc': 'Connection caps learned from 403/429 rejections (24h); clear to relearn',
  'set.general.connPolicyClear': 'Clear',
  'set.general.connPolicyCount': '{count} records',
  'set.general.connPolicyEmpty': 'No records',
  'set.general.retries': 'Max auto retries',
  'set.general.retriesDesc': 'Maximum automatic retries after a failed download',
  'set.general.retryDelay': 'Retry delay',
  'set.general.retryDelayDesc': 'Seconds to wait before the next automatic retry',

  // settings: appearance
  'set.appearance.desc': 'Theme and colors (stored in this browser)',
  'set.appearance.themeMode': 'Theme mode',
  'set.appearance.light': 'Light',
  'set.appearance.dark': 'Dark',
  'set.appearance.system': 'System',
  'set.appearance.accent': 'Accent color',
  'set.appearance.accentNames': 'Blue / Green / Purple / Orange / Rose',
  'set.appearance.language': 'Language',
  'set.appearance.languageDesc': 'UI language; also stored in the server config table',

  // settings: download
  'set.download.desc': 'Stored in the server config table, applied to the download engine',
  'set.download.saveDir': 'Default save directory',
  'set.download.saveDirDesc': 'Path on the server file system',
  'set.download.speedLimit': 'Global speed limit',
  'set.download.speedLimitDesc': 'MB/s, token bucket, 0 = unlimited',
  'set.download.ua': 'Global User-Agent',
  'set.download.uaDesc': 'Browser identity for download requests. Set to "netdisk" for Baidu Pan direct links',
  'set.download.uaDefault': 'Default (unset)',
  'set.download.uaCustomPlaceholder': 'Custom User-Agent',
  'set.download.serverTime': 'Use server file time',
  'set.download.serverTimeDesc':
    "Set completed files' modified time to the server-provided Last-Modified instead of the completion time",

  // settings: bt
  'set.bt.desc': 'librqbit engine parameters (server side)',
  'set.bt.dht': 'Enable DHT',
  'set.bt.dhtDesc': 'Discover peers via distributed hash table without trackers',
  'set.bt.upnp': 'Enable UPnP',
  'set.bt.upnpDesc': 'Automatically map router ports',
  'set.bt.ports': 'Listen port range',
  'set.bt.portsDesc': 'Port range for DHT / outgoing connections',
  'set.bt.trackers': 'Custom trackers',
  'set.bt.trackersDesc': 'One tracker URL per line',

  // settings: proxy
  'set.proxy.desc': 'Server outbound proxy',
  'set.proxy.webNote': 'Web difference: "System proxy" reads the OS registry; "Manual" is recommended on servers.',
  'set.proxy.webNoteTitle': 'Web difference',
  'set.proxy.mode': 'Proxy mode',
  'set.proxy.none': 'No proxy',
  'set.proxy.system': 'System proxy',
  'set.proxy.manual': 'Manual',
  'set.proxy.type': 'Type',
  'set.proxy.host': 'Host',
  'set.proxy.port': 'Port',
  'set.proxy.username': 'Username',
  'set.proxy.password': 'Password',
  'set.proxy.noList': 'Bypass list',
  'set.proxy.noListDesc': 'Domains that skip the proxy, comma separated',
  'set.proxy.test': 'Connectivity test',
  'set.proxy.testRun': 'Test',
  'set.proxy.testing': 'Testing…',
  'set.proxy.testOk': 'Reachable · {ms}ms',
  'set.proxy.testFailed': 'Test failed',

  // settings: security
  'set.sec.desc': 'Maps to the local_server_* config group · the service listens on the configured address only',
  'set.sec.token': 'Access token',
  'set.sec.tokenDesc': 'Required auth for Web / management API (Authorization: Bearer) · customizable, effective after server restart',
  'set.sec.tokenPlaceholder': 'Type or generate a token',
  'set.sec.tokenSaved': 'Access token saved; effective after server restart',
  'set.sec.showToken': 'Show token',
  'set.sec.hideToken': 'Hide token',
  'set.sec.copyToken': 'Copy token',
  'set.sec.genToken': 'Generate random token',
  'set.sec.takeover': 'Browser script takeover',
  'set.sec.takeoverDesc': 'Lets the FluxDown userscript / browser extension take over downloads',
  'set.sec.jsonrpc': 'aria2 RPC compatible',
  'set.sec.jsonrpcDesc': 'Implements the aria2 JSON-RPC protocol for "send to aria2" scripts or clients like AriaNg',
  'set.sec.api': 'Management API',
  'set.sec.apiDesc': 'HTTP API for querying and controlling tasks, for MCP servers and automation scripts (always on for headless server, authentication required)',
  'set.sec.mcp': 'MCP endpoint',
  'set.sec.mcpDesc': 'Model Context Protocol endpoint for AI clients like Claude Desktop, Cursor, and Cline (shares the access token, authentication required · takes effect after server restart)',
  'set.sec.copyAddr': 'Copy address',
  'set.sec.ws': 'This device WebSocket',
  'set.sec.wsConnected': 'Connected · latency {rtt}ms',
  'set.sec.wsDisconnected': 'Not connected',
  'set.sec.wsSessions': '{n} sessions on server',

  // settings: about
  'set.about.version': 'Server version',
  'set.about.logout': 'Log out',
  'set.about.logoutDesc': 'Clear the server address and token saved locally',
  'set.about.upToDate': 'Up to date',
  'set.about.newVersion': 'New version {version} available',
  'set.about.getUpdate': 'Get update',
  'set.about.tagline': 'No ads · No tracking · No account · Data stays on your server',
} as const

export type I18nKey = keyof typeof en

const zh: Record<I18nKey, string> = {
  'common.cancel': '取消',
  'common.confirm': '确定',
  'common.gotIt': '知道了',
  'common.confirmTitle': '确认操作',
  'common.noticeTitle': '提示',
  'common.close': '关闭',
  'common.back': '返回',
  'common.delete': '删除',
  'common.pause': '暂停',
  'common.resume': '恢复',
  'common.retry': '重试',
  'common.done': '完成',
  'common.loading': '加载中…',
  'common.copy': '复制',
  'common.copied': '已复制',
  'common.selectAll': '全选',
  'common.optional': '可选',
  'common.custom': '自定义',
  'common.startDownload': '开始下载',
  'common.settings': '设置',
  'common.logout': '退出登录',
  'common.unknown': '未知',

  'login.title': '连接到 FluxDown 服务器',
  'login.subtitle': 'Downloads, Supercharged. — 远程管理你的下载引擎',
  'login.serverAddress': '服务器地址',
  'login.token': '访问令牌',
  'login.remember': '记住此设备',
  'login.connecting': '连接中…',
  'login.connect': '连 接',
  'login.hint': '令牌在服务器「设置 → 安全与访问」中生成；连接仅限局域网或经反向代理的 HTTPS。',
  'login.connectFailed': '无法连接到服务器，请检查地址',
  'login.invalidToken': '令牌无效',
  'login.featEngine': 'Rust 引擎',
  'login.featEngineDesc': 'HTTP · FTP · BT · HLS · DASH',
  'login.featRealtime': '实时推送',
  'login.featRealtimeDesc': 'WebSocket 进度 / 分段拆分',
  'login.featPrivacy': '零追踪',
  'login.featPrivacyDesc': '无账号 · 数据全在你的服务器',

  'status.pending': '排队中',
  'status.downloading': '下载中',
  'status.paused': '已暂停',
  'status.completed': '已完成',
  'status.error': '错误',
  'status.preparing': '正在准备',
  'status.preparingEllipsis': '正在准备…',
  'status.downloadFailed': '下载出错',
  'status.eta': '剩余 {eta}',

  'time.today': '今天',
  'time.yesterday': '昨天',
  'time.thisWeek': '本周',
  'time.thisMonth': '本月',
  'time.older': '更早',
  'time.secs': '{s} 秒',
  'time.minSecs': '{m} 分 {s} 秒',
  'time.hourMin': '{h} 小时 {m} 分',

  'type.all': '全部',
  'type.video': '视频',
  'type.audio': '音频',
  'type.document': '文档',
  'type.image': '图片',
  'type.archive': '压缩包',
  'type.other': '其他',

  'sidebar.fileTypes': '文件类型',
  'sidebar.queues': '队列',
  'sidebar.newQueue': '新建队列',
  'sidebar.newQueuePrompt': '新队列名称',
  'sidebar.allTasks': '全部任务',
  'sidebar.deleteQueue': '删除队列',
  'sidebar.deleteQueueMsg': '删除队列「{name}」？其中的任务会移动到默认队列。',
  'sidebar.idle': '空闲',
  'sidebar.connected': '已连接',
  'sidebar.connectedRtt': '已连接 · 延迟 {rtt}ms',
  'sidebar.connecting': '连接中…',
  'sidebar.disconnected': '已断开',
  'sidebar.feedback': '反馈',
  'sidebar.logoutTitle': '退出登录',
  'sidebar.logoutMsg': '将断开与服务器的连接并清除本设备上保存的令牌，下次访问需重新登录。',
  'sidebar.version': '版本 {version}',
  'sidebar.newVersion': '新版本 {version}',

  'topbar.searchPlaceholder': '搜索任务名称…（Ctrl+F）',
  'topbar.manage': '批量管理',
  'topbar.pauseResumeAll': '全部暂停 / 恢复',
  'topbar.newDownload': '新建下载',
  'topbar.speedLimitOn': '全局限速：{speed}',
  'topbar.speedLimitOff': '全局限速：未开启',
  'topbar.goSettings': '{label}（前往设置调整）',

  'tabs.all': '全部',
  'tabs.downloading': '下载中',
  'tabs.completed': '已完成',
  'tabs.paused': '已暂停',
  'tabs.error': '错误',

  'list.empty': '没有匹配的任务',

  'manage.selected': '已选 {n} 项',
  'manage.deleteTitle': '删除任务',
  'manage.deleteMsg': '删除选中的 {n} 个任务？',

  'task.pause': '暂停',
  'task.resume': '继续',
  'task.retry': '重试',
  'task.boost': 'Boost 优先下载',
  'task.saveToLocal': '保存到本地',
  'task.copyUrl': '复制下载链接',
  'task.copyPath': '复制文件服务器路径',
  'task.moveToQueue': '移动到队列…',
  'task.delete': '删除',
  'task.deleteWithFiles': '删除并删文件',
  'task.deleteTitle': '删除任务',
  'task.deleteWithFilesMsg': '删除任务并删除本地文件？此操作不可撤销。',
  'task.deleteMsg': '删除该任务？',

  'detail.tabGeneral': '常规',
  'detail.tabSegments': '分段',
  'detail.tabQueue': '队列',
  'detail.tabLog': '日志',
  'detail.tabAdvanced': '高级',
  'detail.collapse': '收起面板',
  'detail.expand': '点击展开完整内容',
  'detail.collapseValue': '点击收起',
  'detail.downloaded': '已下载',
  'detail.totalSize': '总大小',
  'detail.speed': '速度',
  'detail.eta': '剩余时间',
  'detail.remaining': '剩余 {eta}',
  'detail.fileName': '文件名',
  'detail.url': '下载链接',
  'detail.savePath': '保存路径',
  'detail.createdAt': '创建时间',
  'detail.status': '状态',
  'detail.defaultQueue': '默认队列',
  'detail.currentQueue': '当前队列',
  'detail.moveTo': '移动到',
  'detail.segments': '个分段',
  'detail.segmentCount': '{n} 个分段',
  'detail.noSegments': '暂无分段数据',
  'detail.logEmpty': '本次会话暂无事件',
  'detail.checksum': '校验和',
  'detail.proxy': '代理',
  'detail.taskId': '任务 ID',
  'detail.notSet': '未设置',
  'detail.protoQueue': '协议 / 队列',
  'detail.threads': '线程',
  'detail.error': '错误',
  'detail.segCleared': '任务已完成，分段信息已从 task_segments 表清理。',
  'detail.noSegmentsHint': '暂无分段数据（等待引擎探测完成或分配分段）。',
  'detail.segAdvisorSummary': '共 {n} 个分段 · segment_advisor 动态决定',
  'detail.segFooterNote': '慢速分段由 segment_coordinator 主动拆分 / 抢救；拆分事件（SegmentSplit）经 WebSocket 实时推送并触发列表动画。',
  'detail.currentQueueValue': '当前：{name}',
  'detail.moveToOther': '移动到其它队列',
  'detail.noLimit': '不限速',
  'detail.concurrency': '并发 {n}',
  'detail.queueFooterNote': '每个命名队列拥有独立限速 / 并发 / 默认目录 / 默认线程配置。',
  'detail.currentStatus': '当前状态：{status}',
  'detail.logEmptyNote': '本面板仅记录本次会话内观测到的状态迁移与分段拆分事件；完整审计日志见服务器端 logs/ 目录。',
  'detail.checksumNotSet': '未设置（下载完成后跳过校验）',
  'detail.proxyNotSet': '跟随全局（设置 → 代理）',
  'detail.checksumFooterNote': 'Checksum 格式 algo=hexhash（如 sha256=…），任务完成后由引擎在服务器端校验。',

  'event.statusChanged': '状态变更：{from} → {to}',
  'event.errored': '出错：{message}',
  'event.unknownError': '未知错误',
  'event.segmentSplit': '分段 #{parent} {kind}拆分 → 新增 #{child}（共 {total} 个分段）',
  'event.proactive': '主动',
  'event.reactive': '被动',

  'statusbar.tasks': '{active} 个活跃 · {total} 个任务',
  'statusbar.limit': '限速：',
  'statusbar.limitOff': '关闭',
  'statusbar.demoMode': '演示模式',
  'statusbar.demoTitle': '演示模式：仅允许下载 {url}',
  'statusbar.diskTitle': '服务器磁盘剩余空间',
  'statusbar.diskFree': '{dir} · 剩余 {free}',

  'newDl.title': '新建下载',
  'newDl.urlLabel': '下载链接（每行一条，支持 HTTP / FTP / 磁力链 / M3U8）',
  'newDl.urlLabelDemo': '下载链接（演示模式，已锁定）',
  'newDl.fileName': '文件名',
  'newDl.fileNamePlaceholder': '自动从 URL 推断',
  'newDl.saveDir': '保存目录',
  'newDl.segments': '线程数',
  'newDl.segmentsAuto': '自动（segment_advisor）',
  'newDl.segmentsN': '{n} 线程',
  'newDl.queue': '队列',
  'newDl.defaultQueue': '默认队列',
  'newDl.userAgent': 'User-Agent',
  'newDl.globalDefault': '全局默认',
  'newDl.advanced': '高级选项',
  'newDl.cookies': 'Cookies',
  'newDl.headers': '自定义请求头',
  'newDl.headersAdd': '添加请求头',
  'newDl.headerName': '请求头名称',
  'newDl.headerValue': '值',
  'newDl.proxy': '单任务代理',
  'newDl.checksum': '校验和',
  'newDl.checksumPlaceholder': '如 sha256=abcdef…',
  'newDl.create': '开始下载',
  'newDl.desc': '填写下载链接与选项，创建一个或多个下载任务',
  'newDl.demoHint': '演示模式：仅允许下载上方的演示文件，其它链接会被服务器拒绝。',
  'newDl.lineError': '第 {n} 行 {line}：{error}',
  'newDl.creating': '创建中…',

  'fs.browse': '浏览',
  'fs.title': '选择保存目录',
  'fs.desc': '浏览服务器文件系统并选择任务保存目录',
  'fs.up': '上级目录',
  'fs.loadFailed': '目录读取失败',
  'fs.emptyDir': '此目录下没有子目录',
  'fs.choose': '选择此目录',

  'hls.title': '选择画质',
  'hls.desc': '检测到 {n} 个码率 · 60 秒内未选择将自动使用最高带宽',
  'hls.variant': '变体 {n}',

  'bt.title': '选择要下载的文件',
  'bt.summary': '{n} 个文件 · 共 {size}',
  'bt.selected': '已选 {n} 个 · {size}',

  'set.general': '通用',
  'set.appearance': '外观',
  'set.download': '下载',
  'set.bt': 'BitTorrent',
  'set.proxy': '代理',
  'set.security': '安全与访问',
  'set.about': '关于',
  'set.title': '设置',
  'set.loadFailed': '配置加载失败',

  'set.general.desc': '服务器行为设置，保存在服务器 config 表',
  'set.general.maxConcurrent': '最大并发任务',
  'set.general.maxConcurrentDesc': '同时进行下载的任务数量上限',
  'set.general.segments': '最大连接数',
  'set.general.segmentsDesc': '每任务连接数上限（渐进提升）；0 = 由 segment_advisor 按文件大小动态决定',
  'set.general.autoMaxConn': 'Auto 模式连接上限',
  'set.general.autoMaxConnDesc': 'Auto 模式下智能调度允许的最大连接数',
  'set.general.connPolicy': '已学习的服务器策略',
  'set.general.connPolicyDesc': '服务器拒绝多连接（403/429）后记住的连接上限（24 小时）；可清除重新学习',
  'set.general.connPolicyClear': '清除',
  'set.general.connPolicyCount': '{count} 条记录',
  'set.general.connPolicyEmpty': '暂无记录',
  'set.general.retries': '自动重试次数上限',
  'set.general.retriesDesc': '下载失败后自动重试的最大次数',
  'set.general.retryDelay': '重试间隔',
  'set.general.retryDelayDesc': '失败后到下一次自动重试的等待秒数',

  'set.appearance.desc': '主题与配色（保存在浏览器本地）',
  'set.appearance.themeMode': '主题模式',
  'set.appearance.light': '浅色',
  'set.appearance.dark': '深色',
  'set.appearance.system': '跟随系统',
  'set.appearance.accent': '强调色',
  'set.appearance.accentNames': '默认蓝 / 绿 / 紫 / 橙 / 玫红',
  'set.appearance.language': '语言 / Language',
  'set.appearance.languageDesc': '界面语言；同时保存在服务器 config 表',

  'set.download.desc': '保存在服务器 config 表，作用于下载引擎',
  'set.download.saveDir': '默认保存目录',
  'set.download.saveDirDesc': '服务器文件系统路径',
  'set.download.speedLimit': '全局限速',
  'set.download.speedLimitDesc': '单位 MB/s，Token Bucket，0 = 不限速',
  'set.download.ua': '全局 User-Agent',
  'set.download.uaDesc': '下载请求时使用的浏览器标识。百度网盘直链下载需设为 netdisk',
  'set.download.uaDefault': '默认（不设置）',
  'set.download.uaCustomPlaceholder': '自定义 User-Agent',
  'set.download.serverTime': '使用服务器文件时间',
  'set.download.serverTimeDesc': '下载完成后将文件修改时间设为服务器提供的 Last-Modified，而非下载完成时间',

  'set.bt.desc': 'librqbit 引擎参数（服务器端）',
  'set.bt.dht': '启用 DHT',
  'set.bt.dhtDesc': '无 Tracker 时通过分布式哈希表发现节点',
  'set.bt.upnp': '启用 UPnP',
  'set.bt.upnpDesc': '自动映射路由器端口',
  'set.bt.ports': '监听端口范围',
  'set.bt.portsDesc': 'DHT / 出站连接监听端口区间',
  'set.bt.trackers': '自定义 Tracker',
  'set.bt.trackersDesc': '每行一个 Tracker 地址',

  'set.proxy.desc': '服务器出站代理',
  'set.proxy.webNote': '「系统代理」需读取系统注册表，服务器端建议使用「手动配置」。',
  'set.proxy.webNoteTitle': 'Web 版差异',
  'set.proxy.mode': '代理模式',
  'set.proxy.none': '不使用代理',
  'set.proxy.system': '系统代理',
  'set.proxy.manual': '手动配置',
  'set.proxy.type': '类型',
  'set.proxy.host': '地址',
  'set.proxy.port': '端口',
  'set.proxy.username': '用户名',
  'set.proxy.password': '密码',
  'set.proxy.noList': '排除列表',
  'set.proxy.noListDesc': '不走代理的域名，逗号分隔',
  'set.proxy.test': '连通性测试',
  'set.proxy.testRun': '测试',
  'set.proxy.testing': '测试中…',
  'set.proxy.testOk': '连通 · {ms}ms',
  'set.proxy.testFailed': '测试失败',

  'set.sec.desc': '对应 local_server_* 配置组 · 服务仅监听配置的地址',
  'set.sec.token': '访问令牌',
  'set.sec.tokenDesc': 'Web / 管理 API 强制鉴权（Authorization: Bearer）· 可自定义，重启服务器后生效',
  'set.sec.tokenPlaceholder': '自定义或生成令牌',
  'set.sec.tokenSaved': '访问令牌已保存，重启服务器后生效',
  'set.sec.showToken': '显示令牌',
  'set.sec.hideToken': '隐藏令牌',
  'set.sec.copyToken': '复制令牌',
  'set.sec.genToken': '随机生成令牌',
  'set.sec.takeover': '浏览器脚本接管',
  'set.sec.takeoverDesc': '供 FluxDown 油猴脚本 / 浏览器扩展接管下载',
  'set.sec.jsonrpc': 'aria2 RPC 兼容',
  'set.sec.jsonrpcDesc': '兼容 aria2 JSON-RPC 协议，供"发送到 aria2"类脚本或 AriaNg 等客户端使用',
  'set.sec.api': '管理 API',
  'set.sec.apiDesc': '提供任务查询与控制的 HTTP API，供 MCP、自动化脚本等外部程序调用（服务器版恒定开启，强制鉴权）',
  'set.sec.mcp': 'MCP 端点',
  'set.sec.mcpDesc': '暴露 Model Context Protocol 端点，供 Claude Desktop、Cursor、Cline 等 AI 客户端接入（与访问令牌共用，强制鉴权 · 重启服务器后生效）',
  'set.sec.copyAddr': '复制地址',
  'set.sec.ws': '本机 WebSocket 连接',
  'set.sec.wsConnected': '已连接 · 延迟 {rtt}ms',
  'set.sec.wsDisconnected': '未连接',
  'set.sec.wsSessions': '服务器共 {n} 个会话',

  'set.about.version': '服务器版本',
  'set.about.logout': '退出登录',
  'set.about.logoutDesc': '清除本地保存的服务器地址与令牌',
  'set.about.upToDate': '已是最新版本',
  'set.about.newVersion': '发现新版本 {version}',
  'set.about.getUpdate': '前往下载',
  'set.about.tagline': '零广告 · 零追踪 · 无需账号 · 数据全在你的服务器',
}

const DICTS: Record<Locale, Record<I18nKey, string>> = { en, zh }

// ---------------------------------------------------------------------------
// 后端 message 本地化（wire 契约保持英文，展示层映射）
// ---------------------------------------------------------------------------

/** 后端规范英文 message → zh。en 语言下原样透传。 */
const BACKEND_ZH: Record<string, string> = {
  'not found': '未找到',
  'task not found': '任务不存在',
  'unknown endpoint': '未知端点',
  'app shutting down': '服务正在关闭',
  'invalid or missing token': '令牌无效或缺失',
  'missing X-FluxDown-Client header': '缺少 X-FluxDown-Client 头',
  'management API requires a token; set one in Settings > API Service':
    '管理 API 需要令牌；请在「设置 → API 服务」中设置',
  'queue name is required': '队列名称不能为空',
  'url is required': 'URL 不能为空',
  'task is not completed': '任务尚未完成',
  'file not found on disk': '磁盘上找不到该文件',
  'failed to persist task': '任务持久化失败',
  'demo mode: only the designated demo file can be downloaded':
    '演示模式：仅允许下载指定的演示文件',
}

/** 按语言本地化后端返回的 message；未识别的消息原样返回。 */
export function translateBackendMessage(message: string, locale?: Locale): string {
  const loc = locale ?? currentLocale
  if (loc === 'en') return message
  return BACKEND_ZH[message.trim()] ?? message
}

// ---------------------------------------------------------------------------
// t() —— 模块级当前语言（非 React 代码可直接用），Provider 负责触发重渲染
// ---------------------------------------------------------------------------

function readStoredLocale(): Locale | null {
  const v = localStorage.getItem(LOCALE_KEY)
  return v === 'en' || v === 'zh' ? v : null
}

/** 浏览器首选语言 → 支持的语言（主语言子标签为 zh 即中文，其余英文）。 */
function detectBrowserLocale(): Locale {
  return navigator.language?.toLowerCase().startsWith('zh') ? 'zh' : 'en'
}

let currentLocale: Locale = readStoredLocale() ?? detectBrowserLocale()

export function getLocale(): Locale {
  return currentLocale
}

/** 翻译 key，支持 `{name}` 占位插值。 */
export function t(key: I18nKey, params?: Record<string, string | number>): string {
  let s: string = DICTS[currentLocale][key] ?? en[key] ?? key
  if (params) {
    for (const [k, v] of Object.entries(params)) s = s.replaceAll(`{${k}}`, String(v))
  }
  return s
}

// ---------------------------------------------------------------------------
// Provider / hook
// ---------------------------------------------------------------------------

interface I18nCtx {
  locale: Locale
  setLocale: (l: Locale) => void
  t: typeof t
}

const Ctx = createContext<I18nCtx>({ locale: currentLocale, setLocale: () => {}, t })

export function I18nProvider({ children }: { children: React.ReactNode }) {
  const [locale, setLocaleState] = useState<Locale>(currentLocale)

  useEffect(() => {
    document.documentElement.lang = locale === 'zh' ? 'zh-CN' : 'en'
    document.title = locale === 'zh' ? 'FluxDown — 下载管理' : 'FluxDown — Download Manager'
  }, [locale])

  // 切换语言但不落盘：采用服务器/浏览器默认值时用，保持「未显式选择」状态。
  const applyLocale = useCallback((l: Locale) => {
    currentLocale = l
    setLocaleState(l)
  }, [])

  // 用户显式选择（设置页）：落盘 localStorage，此后默认值不再覆盖本浏览器。
  const setLocale = useCallback(
    (l: Locale) => {
      localStorage.setItem(LOCALE_KEY, l)
      applyLocale(l)
    },
    [applyLocale],
  )

  // 挂载时从 /ping（无鉴权）采用服务器默认语言，登录页同样生效；
  // 本地已显式选择过语言（localStorage 有值）则以本地为准。
  useEffect(() => {
    if (readStoredLocale() !== null) return
    api
      .ping()
      .then(({ language }) => {
        if ((language === 'zh' || language === 'en') && readStoredLocale() === null) {
          applyLocale(language)
        }
      })
      .catch(() => {})
  }, [applyLocale])

  const value = useMemo(() => ({ locale, setLocale, t }), [locale, setLocale])
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>
}

export function useI18n() {
  return useContext(Ctx)
}
