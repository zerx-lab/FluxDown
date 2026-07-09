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

import { browser } from 'wxt/browser';
import { initI18n, applyI18nToDOM, t, getLocale, saveLocale } from '@/utils/i18n';
import { checkFluxDownAvailable } from '@/utils/download-dispatch';
import { loadSettings, saveSettings } from '@/utils/settings';

const $ = <T extends HTMLElement>(sel: string) => document.querySelector<T>(sel)!;

// ===== DOM 元素 =====
const statusBadge = $('#statusBadge')!;
const statusText = statusBadge.querySelector('.status-text')!;
const enableToggle = $<HTMLInputElement>('#enableToggle');
const enableHint = $('#enableHint')!;
const dotVisibleToggle = $<HTMLInputElement>('#dotVisibleToggle');
const interceptModeSelect = $<HTMLSelectElement>('#interceptModeSelect');
const modeHint = $('#modeHint')!;
const minSizeSelect = $<HTMLSelectElement>('#minSizeSelect');

// 远程下载源（url/token/测试连接已迁移到 options 配置页）
const remoteModeSelect = $<HTMLSelectElement>('#remoteModeSelect');
const remoteModeHelp = $('#remoteModeHelp')!;
const remoteServerSummary = $('#remoteServerSummary')!;
const openOptionsBtn = $<HTMLButtonElement>('#openOptionsBtn');
const openSettingsBtn = $<HTMLButtonElement>('#openSettingsBtn');
const themeBtn = $<HTMLButtonElement>('#themeBtn');
const langBtn = $<HTMLButtonElement>('#langBtn');
const langLabel = langBtn.querySelector('.lang-label')!;

// 版本号
const versionLabel = $('#versionLabel')!;

// 统计
const statSent = $('#statSent')!;
const statFailed = $('#statFailed')!;
const resetStatsBtn = $<HTMLButtonElement>('#resetStatsBtn');

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
  await browser.storage.local.set({ theme: next });
}

window.matchMedia('(prefers-color-scheme: light)').addEventListener('change', async () => {
  const result = await browser.storage.local.get('theme') ?? {};
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
  const current = await loadSettings();
  const domains = current.excludeDomains.filter((d) => d !== domain);
  if (domains.length !== current.excludeDomains.length) {
    await saveSettings({ excludeDomains: domains });
    renderDomainList(domains);
    showToast(t('domain.removed', { domain }));
  }
}

async function addDomain(domain: string) {
  domain = domain.trim().toLowerCase();
  if (!domain) return;

  const current = await loadSettings();
  const domains = [...current.excludeDomains];

  if (domains.includes(domain)) {
    showToast(t('domain.exists', { domain }), 'error');
    return;
  }

  domains.push(domain);
  await saveSettings({ excludeDomains: domains });
  renderDomainList(domains);
  showToast(t('domain.excluded', { domain }));
}

// ===== 统计 =====
async function loadStats(preloaded?: { sent?: number; failed?: number; date?: string }) {
  const stats =
    preloaded ??
    ((await browser.storage.local.get('stats'))?.stats as
      | { sent?: number; failed?: number; date?: string }
      | undefined) ??
    { sent: 0, failed: 0, date: '' };

  // 检查是否是今天的统计
  const today = new Date().toDateString();
  if (stats.date !== today) {
    // 新的一天，重置（先渲染再写盘，不阻塞 UI）
    statSent.textContent = '0';
    statFailed.textContent = '0';
    void browser.storage.local.set({
      stats: { sent: 0, failed: 0, date: today },
    });
    return;
  }

  statSent.textContent = String(stats.sent || 0);
  statFailed.textContent = String(stats.failed || 0);
}

// ===== 连接状态 =====
// 并发守卫：设置变更可能连续触发刷新，只让最后一次的结果落到 UI。
let _statusEpoch = 0;

/**
 * 按当前 remoteMode 刷新头部连接徽标（off=NMH / always=远程 / fallback=任一）。
 * 探活逻辑复用 download-dispatch，与实际下载路由判定保持一致。
 */
async function refreshConnectionStatus(): Promise<void> {
  const epoch = ++_statusEpoch;
  statusBadge.className = 'status-badge';
  statusText.textContent = t('header.checking');

  const available = await checkFluxDownAvailable().catch(() => false);
  if (epoch !== _statusEpoch) return; // 已有更新的刷新在途，丢弃本次结果

  if (available) {
    statusBadge.className = 'status-badge connected';
    statusText.textContent = t('header.connected');
    // 探活成功 → 通知 background 解除可用性熔断，让接管状态与这里显示的
    // "已连接"保持一致，避免熔断期内 popup 显示已连接但下载仍被旁路到
    // 浏览器（review 发现 #1/#4/#6）。fire-and-forget：不依赖返回值，
    // 规避 Firefox MV2 下 sendMessage 收到 undefined 的问题。
    browser.runtime
      .sendMessage({ action: 'appConfirmedUp' })
      .catch(() => {});
  } else {
    statusBadge.className = 'status-badge disconnected';
    statusText.textContent = t('header.disconnected');
  }
}

// ===== 初始化 =====
// 性能关键路径：popup 弹出到首次完整渲染。
// 1. 所有 storage 读取合并为一轮并行（i18n / 主题+悬浮球+统计 / 设置）；
// 2. 探活（refreshConnectionStatus）不再阻塞 UI 回显——off 模式下它要
//    connectNative 冷启动 NMH 进程、always 模式下 remotePing 超时可达 4s，
//    此前与 loadSettings 绑在同一个 Promise.all 里是弹出卡顿的主因；
//    徽标静态 HTML 默认即"检测中"，探活结果异步落格。
async function init() {
  const [, localState, settings] = await Promise.all([
    initI18n(),
    browser.storage.local.get(['theme', 'fluxdown_dot_visible', 'stats']),
    loadSettings(),
  ]);

  applyI18nToDOM();
  updateLangButton();
  applyTheme((localState?.theme as ThemeMode) || 'system');

  // 从 manifest 动态读取版本号（CI 构建时由 git tag 写入）
  versionLabel.textContent = `v${browser.runtime.getManifest().version}`;

  // 更新设置 UI
  enableToggle.checked = settings.enabled;
  updateEnableHint(settings.enabled);
  interceptModeSelect.value = settings.interceptMode || 'smart';
  updateModeHint(settings.interceptMode || 'smart');
  minSizeSelect.value = String(settings.minFileSize);
  renderDomainList(settings.excludeDomains || []);

  // 远程下载源设置
  remoteModeSelect.value = settings.remoteMode || 'off';
  refreshRemoteHelpTooltip();
  updateRemoteModeGate(settings.remoteVerified === true, settings.remoteUrl || '');

  // 悬浮球可见状态（未设置时默认显示）
  dotVisibleToggle.checked = localState?.['fluxdown_dot_visible'] !== false;

  // 统计（数据已随批量读取取回）
  await loadStats(localState?.stats);

  // 连接探活：fire-and-forget，结果异步更新徽标，不阻塞弹出渲染。
  // 直接查询而不经过 background sendMessage（Firefox MV2 下 WXT 的 HMR
  // onMessage 监听器会抢答 undefined）；探活走 download-dispatch，
  // 按 remoteMode 适配桌面（NMH）/ 远程 / 两者任一。
  void refreshConnectionStatus().catch(() => {});
}

function updateEnableHint(enabled: boolean) {
  enableHint.textContent = enabled ? t('switch.enabled') : t('switch.disabled');
}

type ModeKey = 'settings.hintSmart' | 'settings.hintAll';

const MODE_HINT_KEYS: Record<string, ModeKey> = {
  smart: 'settings.hintSmart',
  all: 'settings.hintAll',
};

function updateModeHint(mode: string) {
  const key = MODE_HINT_KEYS[mode];
  modeHint.textContent = key ? t(key) : '';
}

type RemoteModeHintKey =
  | 'remote.modeHintOff'
  | 'remote.modeHintFallback'
  | 'remote.modeHintAlways';

const REMOTE_MODE_HINT_KEYS: Record<string, RemoteModeHintKey> = {
  off: 'remote.modeHintOff',
  fallback: 'remote.modeHintFallback',
  always: 'remote.modeHintAlways',
};

// 提示合并到「?」图标的悬浮 tooltip（title），不再占用面板空间。
// 两个来源：当前模式说明 + 未验证时的解锁指引，各自更新后统一拼装。
let _remoteHelpVerified = true;

function refreshRemoteHelpTooltip() {
  const key = REMOTE_MODE_HINT_KEYS[remoteModeSelect.value];
  const parts = [key ? t(key) : ''];
  if (!_remoteHelpVerified) parts.push(t('remote.verifyRequired'));
  remoteModeHelp.setAttribute('title', parts.filter(Boolean).join('\n'));
}

/**
 * 远程模式门禁：仅当远程配置已在配置页通过「测试连接」（含 token 校验）时，
 * 才允许选择 fallback/always；未验证时禁用这两个选项，解锁指引并入 tooltip。
 */
function updateRemoteModeGate(verified: boolean, remoteUrl: string) {
  for (const opt of remoteModeSelect.options) {
    if (opt.value !== 'off') opt.disabled = !verified;
  }
  _remoteHelpVerified = verified;
  refreshRemoteHelpTooltip();
  remoteServerSummary.textContent = remoteUrl
    ? remoteUrl.replace(/^https?:\/\//, '')
    : '';
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

// 悬浮球显示/隐藏
dotVisibleToggle.addEventListener('change', async () => {
  await browser.storage.local.set({ fluxdown_dot_visible: dotVisibleToggle.checked });
});

// 启用/禁用开关
enableToggle.addEventListener('change', async () => {
  const enabled = enableToggle.checked;
  updateEnableHint(enabled);
  await saveSettings({ enabled });
});

// 拦截模式
interceptModeSelect.addEventListener('change', async () => {
  const mode = interceptModeSelect.value;
  updateModeHint(mode);
  await saveSettings({ interceptMode: mode as any });
});

// 最小文件大小
minSizeSelect.addEventListener('change', async () => {
  await saveSettings({ minFileSize: parseInt(minSizeSelect.value, 10) });
});

// 远程下载源 - 模式
remoteModeSelect.addEventListener('change', async () => {
  const mode = remoteModeSelect.value as 'off' | 'fallback' | 'always';
  refreshRemoteHelpTooltip();
  await saveSettings({ remoteMode: mode });
  await refreshConnectionStatus(); // 模式切换影响探活目标，立即刷新徽标
});

// 打开 options 配置页（url/token/测试连接在配置页维护）
// 远程区块入口：直达远程服务器面板（hash 导航）
openOptionsBtn.addEventListener('click', () => {
  browser.tabs.create({ url: browser.runtime.getURL('/options.html#remote') });
  window.close();
});

// 左下角"选项"入口：打开配置页默认面板
openSettingsBtn.addEventListener('click', () => {
  browser.runtime.openOptionsPage();
  window.close();
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
    const [tab] = await browser.tabs.query({ active: true, currentWindow: true });
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
  await browser.storage.local.set({ stats: { sent: 0, failed: 0, date: today } });
  statSent.textContent = '0';
  statFailed.textContent = '0';
  showToast(t('stats.resetDone'));
});

// ===== 启动 =====
// R8-3 修复：init 是顶层 async 调用，加 .catch 防止意外异常成为未捕获 rejection
init().catch((e) => {
  console.error('[FluxDown Popup] Init failed:', e);
});
