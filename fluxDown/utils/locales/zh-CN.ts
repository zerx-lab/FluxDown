/**
 * 简体中文翻译
 */
const zhCN = {
  // Header
  'header.themeToggle': '切换主题',
  'header.checking': '检测中...',
  'header.connected': '已连接',
  'header.disconnected': '未连接',

  // Main switch
  'switch.label': '下载拦截',
  'switch.enabled': '已开启',
  'switch.disabled': '已关闭',

  // Stats
  'stats.title': '今日统计',
  'stats.sent': '已接管',
  'stats.failed': '失败',
  'stats.reset': '重置统计',
  'stats.resetDone': '统计已重置',

  // Quick settings
  'settings.title': '快捷设置',
  'settings.interceptMode': '拦截模式',
  'settings.modeSmart': '智能模式',
  'settings.modeExtension': '仅扩展名',
  'settings.modeAll': '拦截所有',
  'settings.hintSmart': '综合文件名、类型、大小智能判断',
  'settings.hintExtension': '仅按 URL/文件名扩展名拦截',
  'settings.hintAll': '拦截所有下载（除排除域名外）',
  'settings.minFileSize': '最小文件大小',
  'settings.sizeNoLimit': '不限',


  // File type management
  'fileType.title': '拦截文件类型',
  'fileType.addTitle': '添加扩展名',
  'fileType.placeholder': '输入扩展名，如 .pdf',
  'fileType.add': '添加',
  'fileType.cancel': '取消',
  'fileType.removed': '已移除 {ext}',
  'fileType.invalidFormat': '扩展名格式无效',
  'fileType.exists': '{ext} 已存在',
  'fileType.added': '已添加 {ext}',

  // Domain exclusion
  'domain.title': '排除域名',
  'domain.addTitle': '手动添加域名',
  'domain.placeholder': '输入域名，如 example.com',
  'domain.currentSite': '当前站点',
  'domain.empty': '暂无排除域名',
  'domain.removed': '已移除 {domain}',
  'domain.exists': '{domain} 已在排除列表中',
  'domain.excluded': '已排除 {domain}',
  'domain.cannotGetDomain': '无法获取当前页面域名',

  // Context menus
  'contextMenu.downloadLink': '使用 FluxDown 下载链接',
  'contextMenu.downloadMedia': '使用 FluxDown 下载媒体',
  'contextMenu.downloadPage': '使用 FluxDown 下载此页面所有链接',

  // Notifications
  'notify.featureInDev': '功能开发中',
  'notify.batchDownloadComing': '批量下载页面链接功能即将推出',
  'notify.downloadSent': '下载已发送',
  'notify.sentToFluxDown': '{name} 已发送到 FluxDown',
  'notify.sendFailed': '发送失败',
  'notify.connectionFailed': '无法连接到 FluxDown 应用: {message}',

  // Manifest
  'manifest.description': '拦截浏览器下载，发送到 FluxDown 桌面应用进行高速下载',
} as const;

export type MessageKey = keyof typeof zhCN;
export default zhCN;
