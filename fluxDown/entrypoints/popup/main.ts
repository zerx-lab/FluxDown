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
 */

const $ = <T extends HTMLElement>(sel: string) => document.querySelector<T>(sel)!;

// ===== DOM 元素 =====
const statusBadge = $('#statusBadge')!;
const statusText = statusBadge.querySelector('.status-text')!;
const enableToggle = $<HTMLInputElement>('#enableToggle');
const enableHint = $('#enableHint')!;
const interceptModeSelect = $<HTMLSelectElement>('#interceptModeSelect');
const modeHint = $('#modeHint')!;
const minSizeSelect = $<HTMLSelectElement>('#minSizeSelect');
const notifyToggle = $<HTMLInputElement>('#notifyToggle');
const themeBtn = $<HTMLButtonElement>('#themeBtn');

// 统计
const statIntercepted = $('#statIntercepted')!;
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
const addCurrentDomainBtn = $<HTMLButtonElement>('#addCurrentDomainBtn');
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
    showToast(`已移除 ${ext}`);
  }
}

async function addExtension(ext: string) {
  // 标准化：确保以 . 开头，转小写
  ext = ext.trim().toLowerCase();
  if (!ext.startsWith('.')) ext = '.' + ext;

  // 验证格式
  if (!/^\.\w+$/.test(ext)) {
    showToast('扩展名格式无效', 'error');
    return;
  }

  const result = await chrome.storage.sync.get('settings');
  const settings = result.settings || {};
  const exts: string[] = settings.interceptExtensions || [];

  if (exts.includes(ext)) {
    showToast(`${ext} 已存在`, 'error');
    return;
  }

  exts.push(ext);
  settings.interceptExtensions = exts;
  await chrome.storage.sync.set({ settings });
  renderExtTags(exts);
  showToast(`已添加 ${ext}`);
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
    showToast(`已移除 ${domain}`);
  }
}

async function addDomain(domain: string) {
  domain = domain.trim().toLowerCase();
  if (!domain) return;

  const result = await chrome.storage.sync.get('settings');
  const settings = result.settings || {};
  const domains: string[] = settings.excludeDomains || [];

  if (domains.includes(domain)) {
    showToast(`${domain} 已在排除列表中`, 'error');
    return;
  }

  domains.push(domain);
  settings.excludeDomains = domains;
  await chrome.storage.sync.set({ settings });
  renderDomainList(domains);
  showToast(`已排除 ${domain}`);
}

// ===== 统计 =====
async function loadStats() {
  const result = await chrome.storage.local.get('stats');
  const stats = result.stats || { intercepted: 0, sent: 0, failed: 0, date: '' };

  // 检查是否是今天的统计
  const today = new Date().toDateString();
  if (stats.date !== today) {
    // 新的一天，重置
    const resetStats = { intercepted: 0, sent: 0, failed: 0, date: today };
    await chrome.storage.local.set({ stats: resetStats });
    statIntercepted.textContent = '0';
    statSent.textContent = '0';
    statFailed.textContent = '0';
    return;
  }

  statIntercepted.textContent = String(stats.intercepted || 0);
  statSent.textContent = String(stats.sent || 0);
  statFailed.textContent = String(stats.failed || 0);
}

// ===== 初始化 =====
async function init() {
  await initTheme();

  // 获取连接状态和设置
  const response = await chrome.runtime.sendMessage({ action: 'getStatus' });

  // 更新连接状态
  if (response.connected) {
    statusBadge.className = 'status-badge connected';
    statusText.textContent = '已连接';
  } else {
    statusBadge.className = 'status-badge disconnected';
    statusText.textContent = '未连接';
  }

  // 更新设置 UI
  if (response.settings) {
    const s = response.settings;
    enableToggle.checked = s.enabled;
    updateEnableHint(s.enabled);
    interceptModeSelect.value = s.interceptMode || 'smart';
    updateModeHint(s.interceptMode || 'smart');
    minSizeSelect.value = String(s.minFileSize);
    notifyToggle.checked = s.showNotification;

    // 渲染扩展名标签
    renderExtTags(s.interceptExtensions || []);

    // 渲染排除域名
    renderDomainList(s.excludeDomains || []);
  }

  // 加载统计
  await loadStats();
}

function updateEnableHint(enabled: boolean) {
  enableHint.textContent = enabled ? '已开启' : '已关闭';
}

const MODE_HINTS: Record<string, string> = {
  smart: '综合文件名、类型、大小智能判断',
  extension: '仅按 URL/文件名扩展名拦截',
  all: '拦截所有下载（除排除域名外）',
};

function updateModeHint(mode: string) {
  modeHint.textContent = MODE_HINTS[mode] || '';
}

// ===== 事件绑定 =====

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

// 通知开关
notifyToggle.addEventListener('change', async () => {
  const result = await chrome.storage.sync.get('settings');
  const settings = result.settings || {};
  settings.showNotification = notifyToggle.checked;
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

// 添加当前域名
addCurrentDomainBtn.addEventListener('click', async () => {
  try {
    const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
    if (tab?.url) {
      const hostname = new URL(tab.url).hostname;
      if (hostname) {
        await addDomain(hostname);
      } else {
        showToast('无法获取当前页面域名', 'error');
      }
    }
  } catch {
    showToast('无法获取当前页面域名', 'error');
  }
});

// 重置统计
resetStatsBtn.addEventListener('click', async () => {
  const today = new Date().toDateString();
  await chrome.storage.local.set({ stats: { intercepted: 0, sent: 0, failed: 0, date: today } });
  statIntercepted.textContent = '0';
  statSent.textContent = '0';
  statFailed.textContent = '0';
  showToast('统计已重置');
});

// ===== 启动 =====
init();
