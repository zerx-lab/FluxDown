/**
 * FluxDown Popup Script
 *
 * 功能：
 * - 连接状态显示
 * - 下载拦截开关
 * - 今日拦截统计
 * - 快捷设置（最小文件大小、通知）
 * - 文件扩展名管理（Tag 增删）
 * - 排除域名管理（快捷添加当前站点）
 * - 主题切换
 * - 多语言支持（中/英）
 */

import { initI18n, applyI18nToDOM, t, getLocale, saveLocale } from '@/utils/i18n';

const $ = <T extends HTMLElement>(sel: string) => document.querySelector<T>(sel)!;

// ===== DOM 元素 =====
const statusBadge = $('#statusBadge')!;
const statusText = statusBadge.querySelector('.status-text')!;
const enableToggle = $<HTMLInputElement>('#enableToggle');
const enableHint = $('#enableHint')!;
const interceptModeSelect = $<HTMLSelectElement>('#interceptModeSelect');
const modeHint = $('#modeHint')!;
const minSizeSelect = $<HTMLSelectElement>('#minSizeSelect');
const themeBtn = $<HTMLButtonElement>('#themeBtn');
const langBtn = $<HTMLButtonElement>('#langBtn');
const langLabel = langBtn.querySelector('.lang-label')!;

// 统计
const statSent = $('#statSent')!;
const statFailed = $('#statFailed')!;
const resetStatsBtn = $<HTMLButtonElement>('#resetStatsBtn');

// 扩展名管理
const addExtBtn = $<HTMLButtonElement>('#addExtBtn');
const extInputRow = $('#extInputRow')!;
const extInput = $<HTMLInputElement>('#extInput');
const extConfirmBtn = $<HTMLButtonElement>('#extConfirmBtn');
const extCancelBtn = $<HTMLButtonElement>('#extCancelBtn');
const extTagsContainer = $('#extTagsContainer')!;

// 域名管理
const addDomainManualBtn = $<HTMLButtonElement>('#addDomainManualBtn');
const addCurrentDomainBtn = $<HTMLButtonElement>('#addCurrentDomainBtn');
const domainInputRow = $('#domainInputRow')!;
const domainInput = $<HTMLInputElement>('#domainInput');
const domainConfirmBtn = $<HTMLButtonElement>('#domainConfirmBtn');
const domainCancelBtn = $<HTMLButtonElement>('#domainCancelBtn');
const domainList = $('#domainList')!;
const domainEmptyHint = $('#domainEmptyHint')!;

// ===== 主题管理 =====
type ThemeMode = 'light' | 'dark' | 'system';

function getSystemTheme(): 'light' | 'dark' {
  return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
}

function applyTheme(mode: ThemeMode) {
  const root = document.documentElement;
  if (mode === 'system') {
    root.removeAttribute('data-theme');
  } else {
    root.setAttribute('data-theme', mode);
  }
}

async function initTheme() {
  const result = await chrome.storage.local.get('theme');
  const saved: ThemeMode = result.theme || 'system';
  applyTheme(saved);
}

async function toggleTheme() {
  const root = document.documentElement;
  const currentAttr = root.getAttribute('data-theme');
  let next: 'light' | 'dark';
  if (!currentAttr) {
    next = getSystemTheme() === 'dark' ? 'light' : 'dark';
  } else {
    next = currentAttr === 'dark' ? 'light' : 'dark';
  }
  applyTheme(next);
  await chrome.storage.local.set({ theme: next });
}

window.matchMedia('(prefers-color-scheme: light)').addEventListener('change', async () => {
  const result = await chrome.storage.local.get('theme');
  if (!result.theme || result.theme === 'system') {
    applyTheme('system');
  }
});

// ===== Toast =====
function showToast(message: string, type: 'success' | 'error' = 'success') {
  let toast = document.querySelector('.toast');
  if (!toast) {
    toast = document.createElement('div');
    toast.className = 'toast';
    document.body.appendChild(toast);
  }
  toast.textContent = message;
  toast.className = `toast ${type} show`;
  setTimeout(() => toast!.classList.remove('show'), 2000);
}

// ===== 扩展名 Tag 渲染 =====
function renderExtTags(extensions: string[]) {
  extTagsContainer.innerHTML = '';
  for (const ext of extensions) {
    const tag = document.createElement('span');
    tag.className = 'tag';
    tag.innerHTML = `${ext}<button class="tag-remove" data-ext="${ext}">&times;</button>`;
    extTagsContainer.appendChild(tag);
  }

  // 绑定删除事件
  extTagsContainer.querySelectorAll<HTMLButtonElement>('.tag-remove').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const ext = btn.dataset.ext!;
      await removeExtension(ext);
    });
  });
}

async function removeExtension(ext: string) {
  const result = await chrome.storage.sync.get('settings');
  const settings = result.settings || {};
  const exts: string[] = settings.interceptExtensions || [];
  const idx = exts.indexOf(ext);
  if (idx !== -1) {
    exts.splice(idx, 1);
    settings.interceptExtensions = exts;
    await chrome.storage.sync.set({ settings });
    renderExtTags(exts);
    showToast(t('fileType.removed', { ext }));
  }
}

async function addExtension(ext: string) {
  // 标准化：确保以 . 开头，转小写
  ext = ext.trim().toLowerCase();
  if (!ext.startsWith('.')) ext = '.' + ext;

  // 验证格式
  if (!/^\.\w+$/.test(ext)) {
    showToast(t('fileType.invalidFormat'), 'error');
    return;
  }

  const result = await chrome.storage.sync.get('settings');
  const settings = result.settings || {};
  const exts: string[] = settings.interceptExtensions || [];

  if (exts.includes(ext)) {
    showToast(t('fileType.exists', { ext }), 'error');
    return;
  }

  exts.push(ext);
  settings.interceptExtensions = exts;
  await chrome.storage.sync.set({ settings });
  renderExtTags(exts);
  showToast(t('fileType.added', { ext }));
}

// ===== 域名管理 =====
function renderDomainList(domains: string[]) {
  // 清除非 empty-hint 的元素
  domainList.querySelectorAll('.domain-item').forEach((el) => el.remove());

  if (domains.length === 0) {
    domainEmptyHint.style.display = '';
    return;
  }

  domainEmptyHint.style.display = 'none';

  for (const domain of domains) {
    const item = document.createElement('div');
    item.className = 'domain-item';
    item.innerHTML = `
      <span class="domain-text">${domain}</span>
      <button class="domain-remove" data-domain="${domain}">&times;</button>
    `;
    domainList.appendChild(item);
  }

  // 绑定删除事件
  domainList.querySelectorAll<HTMLButtonElement>('.domain-remove').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const domain = btn.dataset.domain!;
      await removeDomain(domain);
    });
  });
}

async function removeDomain(domain: string) {
  const result = await chrome.storage.sync.get('settings');
  const settings = result.settings || {};
  const domains: string[] = settings.excludeDomains || [];
  const idx = domains.indexOf(domain);
  if (idx !== -1) {
    domains.splice(idx, 1);
    settings.excludeDomains = domains;
    await chrome.storage.sync.set({ settings });
    renderDomainList(domains);
    showToast(t('domain.removed', { domain }));
  }
}

async function addDomain(domain: string) {
  domain = domain.trim().toLowerCase();
  if (!domain) return;

  const result = await chrome.storage.sync.get('settings');
  const settings = result.settings || {};
  const domains: string[] = settings.excludeDomains || [];

  if (domains.includes(domain)) {
    showToast(t('domain.exists', { domain }), 'error');
    return;
  }

  domains.push(domain);
  settings.excludeDomains = domains;
  await chrome.storage.sync.set({ settings });
  renderDomainList(domains);
  showToast(t('domain.excluded', { domain }));
}

// ===== 统计 =====
async function loadStats() {
  const result = await chrome.storage.local.get('stats');
  const stats = result.stats || { sent: 0, failed: 0, date: '' };

  // 检查是否是今天的统计
  const today = new Date().toDateString();
  if (stats.date !== today) {
    // 新的一天，重置
    const resetStats = { sent: 0, failed: 0, date: today };
    await chrome.storage.local.set({ stats: resetStats });
    statSent.textContent = '0';
    statFailed.textContent = '0';
    return;
  }

  statSent.textContent = String(stats.sent || 0);
  statFailed.textContent = String(stats.failed || 0);
}

// ===== 初始化 =====
async function init() {
  // 初始化 i18n（必须先于 UI 渲染）
  await initI18n();
  applyI18nToDOM();
  updateLangButton();

  await initTheme();

  // 获取连接状态和设置
  const response = await chrome.runtime.sendMessage({ action: 'getStatus' });

  // 更新连接状态
  if (response.connected) {
    statusBadge.className = 'status-badge connected';
    statusText.textContent = t('header.connected');
  } else {
    statusBadge.className = 'status-badge disconnected';
    statusText.textContent = t('header.disconnected');
  }

  // 更新设置 UI
  if (response.settings) {
    const s = response.settings;
    enableToggle.checked = s.enabled;
    updateEnableHint(s.enabled);
    interceptModeSelect.value = s.interceptMode || 'smart';
    updateModeHint(s.interceptMode || 'smart');
    minSizeSelect.value = String(s.minFileSize);

    // 渲染扩展名标签
    renderExtTags(s.interceptExtensions || []);

    // 渲染排除域名
    renderDomainList(s.excludeDomains || []);
  }

  // 加载统计
  await loadStats();
}

function updateEnableHint(enabled: boolean) {
  enableHint.textContent = enabled ? t('switch.enabled') : t('switch.disabled');
}

type ModeKey = 'settings.hintSmart' | 'settings.hintExtension' | 'settings.hintAll';

const MODE_HINT_KEYS: Record<string, ModeKey> = {
  smart: 'settings.hintSmart',
  extension: 'settings.hintExtension',
  all: 'settings.hintAll',
};

function updateModeHint(mode: string) {
  const key = MODE_HINT_KEYS[mode];
  modeHint.textContent = key ? t(key) : '';
}

// ===== 语言切换 =====
function isZh(): boolean {
  return getLocale().startsWith('zh');
}

function updateLangButton() {
  langLabel.textContent = isZh() ? '中' : 'EN';
  langBtn.title = isZh() ? 'Switch to English' : '切换到中文';
}

async function toggleLang() {
  const next = isZh() ? 'en' : 'zh-CN';
  await saveLocale(next);
  applyI18nToDOM();
  updateLangButton();
  // 刷新动态文本
  updateEnableHint(enableToggle.checked);
  updateModeHint(interceptModeSelect.value);
}

// ===== 事件绑定 =====

// 语言切换
langBtn.addEventListener('click', toggleLang);

// 主题切换
themeBtn.addEventListener('click', toggleTheme);

// 启用/禁用开关
enableToggle.addEventListener('change', async () => {
  const res = await chrome.runtime.sendMessage({ action: 'toggleEnabled' });
  updateEnableHint(res.enabled);
});

// 拦截模式
interceptModeSelect.addEventListener('change', async () => {
  const mode = interceptModeSelect.value;
  updateModeHint(mode);
  await chrome.runtime.sendMessage({ action: 'updateSettings', settings: { interceptMode: mode } });
});

// 最小文件大小
minSizeSelect.addEventListener('change', async () => {
  const result = await chrome.storage.sync.get('settings');
  const settings = result.settings || {};
  settings.minFileSize = parseInt(minSizeSelect.value, 10);
  await chrome.storage.sync.set({ settings });
});

// 扩展名 - 显示输入框
addExtBtn.addEventListener('click', () => {
  extInputRow.classList.remove('hidden');
  extInput.focus();
});

// 扩展名 - 确认添加
extConfirmBtn.addEventListener('click', async () => {
  const val = extInput.value.trim();
  if (val) {
    await addExtension(val);
    extInput.value = '';
  }
  extInputRow.classList.add('hidden');
});

// 扩展名 - 取消
extCancelBtn.addEventListener('click', () => {
  extInput.value = '';
  extInputRow.classList.add('hidden');
});

// 扩展名 - Enter 确认
extInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') extConfirmBtn.click();
  if (e.key === 'Escape') extCancelBtn.click();
});

// 域名 - 显示手动输入框
addDomainManualBtn.addEventListener('click', () => {
  domainInputRow.classList.remove('hidden');
  domainInput.focus();
});

// 域名 - 确认手动添加
domainConfirmBtn.addEventListener('click', async () => {
  const val = domainInput.value.trim();
  if (val) {
    await addDomain(val);
    domainInput.value = '';
  }
  domainInputRow.classList.add('hidden');
});

// 域名 - 取消
domainCancelBtn.addEventListener('click', () => {
  domainInput.value = '';
  domainInputRow.classList.add('hidden');
});

// 域名 - Enter 确认
domainInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') domainConfirmBtn.click();
  if (e.key === 'Escape') domainCancelBtn.click();
});

// 添加当前域名
addCurrentDomainBtn.addEventListener('click', async () => {
  try {
    const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
    if (tab?.url) {
      const hostname = new URL(tab.url).hostname;
      if (hostname) {
        await addDomain(hostname);
      } else {
        showToast(t('domain.cannotGetDomain'), 'error');
      }
    }
  } catch {
    showToast(t('domain.cannotGetDomain'), 'error');
  }
});

// 重置统计
resetStatsBtn.addEventListener('click', async () => {
  const today = new Date().toDateString();
  await chrome.storage.local.set({ stats: { sent: 0, failed: 0, date: today } });
  statSent.textContent = '0';
  statFailed.textContent = '0';
  showToast(t('stats.resetDone'));
});

// ===== 启动 =====
init();
