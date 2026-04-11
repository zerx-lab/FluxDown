/**
 * 简体中文翻译
 */
const zhCN = {
  // Header
  "header.themeToggle": "切换主题",
  "header.checking": "检测中...",
  "header.connected": "已连接",
  "header.disconnected": "未连接",

  // Main switch
  "switch.label": "下载拦截",
  "switch.enabled": "已开启",
  "switch.disabled": "已关闭",

  // Stats
  "stats.title": "今日统计",
  "stats.sent": "已接管",
  "stats.failed": "失败",
  "stats.reset": "重置统计",
  "stats.resetDone": "统计已重置",

  // Quick settings
  "settings.title": "快捷设置",
  "settings.interceptMode": "拦截模式",
  "settings.modeSmart": "智能模式",
  "settings.modeExtension": "仅扩展名",
  "settings.modeAll": "拦截所有",
  "settings.hintSmart": "综合文件名、类型、大小智能判断",
  "settings.hintExtension": "仅按 URL/文件名扩展名拦截",
  "settings.hintAll": "拦截所有下载（除排除域名外）",
  "settings.minFileSize": "最小文件大小",
  "settings.sizeNoLimit": "不限",
  "settings.altClickHint": "按 Alt+Shift+D 快捷键可快速切换下载拦截开关",
  "settings.dotVisible": "悬浮球",

  // File type management
  "fileType.title": "拦截文件类型",
  "fileType.addTitle": "添加扩展名",
  "fileType.placeholder": "输入扩展名，如 .pdf",
  "fileType.add": "添加",
  "fileType.cancel": "取消",
  "fileType.removed": "已移除 {ext}",
  "fileType.invalidFormat": "扩展名格式无效",
  "fileType.exists": "{ext} 已存在",
  "fileType.added": "已添加 {ext}",

  // Domain exclusion
  "domain.title": "排除域名",
  "domain.addTitle": "手动添加域名",
  "domain.placeholder": "输入域名，如 example.com",
  "domain.currentSite": "当前站点",
  "domain.empty": "暂无排除域名",
  "domain.removed": "已移除 {domain}",
  "domain.exists": "{domain} 已在排除列表中",
  "domain.excluded": "已排除 {domain}",
  "domain.cannotGetDomain": "无法获取当前页面域名",

  // Notifications
  "notify.batchNoLinks": "未找到链接",
  "notify.batchNoLinksDetail": "当前页面未发现任何链接",
  "notify.batchNoDownloadableLinks": "当前页面未发现可下载的文件链接",
  "notify.batchComplete": "批量下载完成",
  "notify.batchResult": "共 {total} 个文件，成功 {sent} 个，失败 {failed} 个",
  "notify.batchExtractFailed": "提取页面链接失败，请检查页面权限",
  "notify.downloadSent": "下载已发送",
  "notify.sentToFluxDown": "{name} 已发送到 FluxDown",
  "notify.sendFailed": "发送失败",
  "notify.connectionFailed": "无法连接到 FluxDown 应用: {message}",
  "notify.fallbackBrowser": "已回退到浏览器下载",
  "notify.fallbackBrowserDetail":
    "无法发送到 FluxDown，已交由浏览器继续下载: {url}",

  // Resource sniffer & panel
  "sniffer.title": "资源嗅探",
  "sniffer.resourceSniffing": "资源嗅探",
  "sniffer.resourceSniffingHint": "自动检测网页中的可下载资源",
  "sniffer.showFloatingButton": "视频浮动按钮",
  "sniffer.showFloatingButtonHint": "在视频元素上显示快速下载按钮",
  "sniffer.showResourcePanel": "资源面板",
  "sniffer.showResourcePanelHint": "页面底部显示检测到的资源列表",
  "sniffer.sniffImages": "图片嗅探",
  "sniffer.sniffImagesHint": "检测网页中的大图片资源（>100KB）",

  // Resource panel (content script)
  "panel.selectAll": "全选",
  "panel.batchDownload": "批量下载",
  "panel.resources": "个资源",
  "panel.empty": "暂未检测到可下载资源",
  "panel.collapse": "收起",
  "panel.more": "其他 {count} 项",
  "panel.hideDot": "隐藏悬浮球",
  "panel.download": "下载",
  "panel.floatDL": "下载",
  "panel.tabAll": "全部",
  "panel.tabVideo": "视频",
  "panel.tabAudio": "音频",
  "panel.tabDocs": "文档",
  "panel.tabArchive": "压缩包",
  "panel.tabStream": "流媒体",
  "panel.tabSubtitle": "字幕",
  "panel.tabMagnet": "磁力",
  "panel.tabOther": "其他",

  // Shortcut toggle
  "shortcut.toggleTitle": "拦截切换",
  "shortcut.interceptOn": "下载拦截已开启",
  "shortcut.interceptOff": "下载拦截已关闭",

  // Context menu
  "contextMenu.sendToFluxDown": "使用 FluxDown 下载此链接",
  "contextMenu.sendImageToFluxDown": "使用 FluxDown 下载此图片",
  "contextMenu.sendVideoToFluxDown": "使用 FluxDown 下载此视频/音频",
  "contextMenu.sendPageToFluxDown": "使用 FluxDown 下载此页面",

  // Manifest
  "manifest.description":
    "拦截浏览器下载，发送到 FluxDown 桌面应用进行高速下载",
} as const;

export type MessageKey = keyof typeof zhCN;
export default zhCN;
