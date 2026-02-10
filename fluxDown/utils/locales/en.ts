/**
 * English translations
 */
import type { MessageKey } from './zh-CN';

const en: Record<MessageKey, string> = {
  // Header
  'header.themeToggle': 'Toggle theme',
  'header.checking': 'Checking...',
  'header.connected': 'Connected',
  'header.disconnected': 'Disconnected',

  // Main switch
  'switch.label': 'Download Intercept',
  'switch.enabled': 'Enabled',
  'switch.disabled': 'Disabled',

  // Stats
  'stats.title': 'Today\'s Stats',
  'stats.sent': 'Intercepted',
  'stats.failed': 'Failed',
  'stats.reset': 'Reset Stats',
  'stats.resetDone': 'Stats reset',

  // Quick settings
  'settings.title': 'Quick Settings',
  'settings.interceptMode': 'Intercept Mode',
  'settings.modeSmart': 'Smart',
  'settings.modeExtension': 'Extension Only',
  'settings.modeAll': 'Intercept All',
  'settings.hintSmart': 'Smart detection based on filename, type, and size',
  'settings.hintExtension': 'Intercept by URL/filename extension only',
  'settings.hintAll': 'Intercept all downloads (except excluded domains)',
  'settings.minFileSize': 'Min File Size',
  'settings.sizeNoLimit': 'No limit',


  // File type management
  'fileType.title': 'Intercept File Types',
  'fileType.addTitle': 'Add extension',
  'fileType.placeholder': 'Enter extension, e.g. .pdf',
  'fileType.add': 'Add',
  'fileType.cancel': 'Cancel',
  'fileType.removed': 'Removed {ext}',
  'fileType.invalidFormat': 'Invalid extension format',
  'fileType.exists': '{ext} already exists',
  'fileType.added': 'Added {ext}',

  // Domain exclusion
  'domain.title': 'Excluded Domains',
  'domain.addTitle': 'Add domain manually',
  'domain.placeholder': 'Enter domain, e.g. example.com',
  'domain.currentSite': 'Current Site',
  'domain.empty': 'No excluded domains',
  'domain.removed': 'Removed {domain}',
  'domain.exists': '{domain} already in exclusion list',
  'domain.excluded': 'Excluded {domain}',
  'domain.cannotGetDomain': 'Cannot get current page domain',

  // Context menus
  'contextMenu.downloadLink': 'Download link with FluxDown',
  'contextMenu.downloadMedia': 'Download media with FluxDown',
  'contextMenu.downloadPage': 'Download all links on this page with FluxDown',

  // Notifications
  'notify.featureInDev': 'Coming Soon',
  'notify.batchDownloadComing': 'Batch download page links coming soon',
  'notify.downloadSent': 'Download Sent',
  'notify.sentToFluxDown': '{name} sent to FluxDown',
  'notify.sendFailed': 'Send Failed',
  'notify.connectionFailed': 'Cannot connect to FluxDown app: {message}',

  // Manifest
  'manifest.description': 'Intercept browser downloads and send to FluxDown app for high-speed downloading',
};

export default en;
