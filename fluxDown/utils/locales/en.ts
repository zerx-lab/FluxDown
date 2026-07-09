/**
 * English translations
 */
import type { MessageKey } from "./zh-CN";

const en: Record<MessageKey, string> = {
  // Header
  "header.themeToggle": "Toggle theme",
  "header.checking": "Checking...",
  "header.connected": "Connected",
  "header.disconnected": "Disconnected",

  // Main switch
  "switch.label": "Download Intercept",
  "switch.enabled": "Enabled",
  "switch.disabled": "Disabled",

  // Stats
  "stats.title": "Today's Stats",
  "stats.sent": "Intercepted",
  "stats.failed": "Failed",
  "stats.reset": "Reset Stats",
  "stats.resetDone": "Stats reset",

  // Quick settings
  "settings.title": "Quick Settings",
  "settings.interceptMode": "Intercept Mode",
  "settings.modeSmart": "Smart",
  "settings.modeAll": "Intercept All",
  "settings.hintSmart": "Smart detection based on filename, type, and size",
  "settings.hintAll": "Intercept all downloads (except excluded domains)",
  "settings.minFileSize": "Min File Size",
  "settings.sizeNoLimit": "No limit",
  "settings.altClickHint":
    "Press Alt+Shift+D to quickly toggle download interception",
  "settings.dotVisible": "Floating Ball",


  // Domain exclusion
  "domain.title": "Excluded Domains",
  "domain.addTitle": "Add domain manually",
  "domain.add": "Add",
  "domain.cancel": "Cancel",
  "domain.placeholder": "Enter domain, e.g. example.com",
  "domain.currentSite": "Current Site",
  "domain.empty": "No excluded domains",
  "domain.removed": "Removed {domain}",
  "domain.exists": "{domain} already in exclusion list",
  "domain.excluded": "Excluded {domain}",
  "domain.cannotGetDomain": "Cannot get current page domain",

  // Notifications
  "notify.batchNoLinks": "No Links Found",
  "notify.batchNoLinksDetail": "No links found on this page",
  "notify.batchNoDownloadableLinks":
    "No downloadable file links found on this page",
  "notify.batchComplete": "Batch Download Complete",
  "notify.batchResult": "{total} files total, {sent} sent, {failed} failed",
  "notify.batchExtractFailed":
    "Failed to extract page links, check page permissions",
  "notify.downloadSent": "Download Sent",
  "notify.sentToFluxDown": "{name} sent to FluxDown",
  "notify.sendFailed": "Send Failed",
  "notify.connectionFailed": "Cannot connect to FluxDown app: {message}",
  "notify.fallbackBrowser": "Fell back to browser download",
  "notify.fallbackBrowserDetail":
    "Could not send to FluxDown, fell back to browser: {url}",
  "notify.appUnavailable": "FluxDown app not detected",
  "notify.appUnavailableDetail":
    "Temporarily using the browser's built-in download. Make sure the FluxDown desktop app is running; interception will resume automatically.",

  // Resource sniffer & panel
  "sniffer.title": "Resource Sniffer",
  "sniffer.resourceSniffing": "Resource Sniffing",
  "sniffer.resourceSniffingHint":
    "Auto-detect downloadable resources on web pages",
  "sniffer.showFloatingButton": "Video Float Button",
  "sniffer.showFloatingButtonHint":
    "Show quick download button on video elements",
  "sniffer.showResourcePanel": "Resource Panel",
  "sniffer.showResourcePanelHint":
    "Show detected resources panel at page bottom",
  "sniffer.sniffImages": "Image Sniffing",
  "sniffer.sniffImagesHint": "Detect large images on web pages (>100KB)",

  // Resource panel (content script)
  "panel.selectAll": "Select All",
  "panel.batchDownload": "Download",
  "panel.resources": "resources",
  "panel.empty": "No downloadable resources detected",
  "panel.collapse": "Collapse",
  "panel.more": "{count} more",
  "panel.hideDot": "Hide dot",
  "panel.download": "Download",
  "panel.floatDL": "DL",
  "panel.tabAll": "All",
  "panel.tabVideo": "Video",
  "panel.tabAudio": "Audio",
  "panel.tabDocs": "Docs",
  "panel.tabArchive": "Archive",
  "panel.tabStream": "Stream",
  "panel.tabSubtitle": "Subtitle",
  "panel.tabMagnet": "Magnet",
  "panel.tabOther": "Other",
  "panel.qualityPickerTitle": "Select Quality",
  "panel.trackVideo": "Video Track",
  "panel.trackAudio": "Audio Track",
  "panel.qualityUnknown": "Unknown Quality",
  "panel.previewTitle": "Preview",
  "panel.previewClose": "Close preview",
  "panel.previewFailed": "Preview failed to load, may require login or be blocked by CORS",
  "panel.previewUnsupported": "Preview not supported for this type",
  "panel.previewFragmentUnsupported": "This fragment can't be previewed on its own; merge it first",
  "panel.previewHlsUnsupported": "This browser can't play HLS directly. View it on the source page or open it with a player after downloading",
  "panel.previewDashUnsupported": "This browser can't play this DASH stream directly. View it on the source page or open it with a player after downloading",
  "panel.previewLimited": "Preview limited",
  "panel.previewLimitedHint": "Browser preview failed due to CORS/login limits, but download may still succeed (the engine sends your session)",
  "panel.clearFailed": "Clear failed previews",
  "panel.clearFailedHint": "Hide resources that failed to preview (doesn't affect others, and doesn't mean they can't be downloaded)",

  // Shortcut toggle
  "shortcut.toggleTitle": "Intercept Toggle",
  "shortcut.interceptOn": "Download interception enabled",
  "shortcut.interceptOff": "Download interception disabled",

  // Context menu
  "contextMenu.sendToFluxDown": "Download this link with FluxDown",
  "contextMenu.sendImageToFluxDown": "Download this image with FluxDown",
  "contextMenu.sendVideoToFluxDown": "Download this video/audio with FluxDown",
  "contextMenu.sendPageToFluxDown": "Download this page with FluxDown",

  // Manifest
  "manifest.description":
    "Intercept browser downloads and send to FluxDown app for high-speed downloading",
};

export default en;
