/**
 * FluxDown Options 配置页
 *
 * 布局：左侧分类导航 + 右侧设置面板（参考沉浸式翻译设置页），
 * 便于后续持续追加设置分类/设置项。
 *
 * 存放"持久化、低频修改"的配置：
 *   - 通用：界面语言（自动/中文/英文）、外观主题
 *   - 远程服务器：地址 / 访问令牌 / 连接验证
 * popup 只保留高频开关（拦截开关、模式选择等）。
 *
 * 「测试连接」使用 remoteVerify（/ping 探活 + /api/v1/info 鉴权校验），
 * 通过后写入 settings.remoteVerified=true —— popup 的 fallback/always
 * 模式选项以此解锁；url/token 任何变更由 saveSettings 自动复位为未验证。
 *
 * 支持 hash 直达面板：options.html#remote → 打开「远程服务器」。
 */

import { browser } from 'wxt/browser';
import {
  initI18n,
  applyI18nToDOM,
  t,
  saveLocale,
  clearLocale,
  getSavedLocale,
} from '@/utils/i18n';
import {
  loadSettings,
  saveSettings,
  BUILTIN_EXTENSIONS,
  normalizeExtension,
  DEFAULT_SETTINGS,
} from '@/utils/settings';
import { remoteVerify } from '@/utils/remote-server';

const $ = <T extends HTMLElement>(sel: string) => document.querySelector<T>(sel)!;

const navItems = document.querySelectorAll<HTMLButtonElement>('.opt-nav-item');
const panels = document.querySelectorAll<HTMLElement>('.opt-panel');
const languageSelect = $<HTMLSelectElement>('#languageSelect');
const themeSelect = $<HTMLSelectElement>('#themeSelect');
const remoteUrlInput = $<HTMLInputElement>('#remoteUrlInput');
const remoteTokenInput = $<HTMLInputElement>('#remoteTokenInput');
const remoteTestBtn = $<HTMLButtonElement>('#remoteTestBtn');
const remoteTestResult = $('#remoteTestResult')!;
const verifyStateHint = $('#verifyStateHint')!;
const versionLabel = $('#versionLabel')!;
const notifyLocalToggle = $<HTMLInputElement>('#notifyLocalToggle');
const notifyRemoteToggle = $<HTMLInputElement>('#notifyRemoteToggle');

// 拦截规则
const extInput = $<HTMLInputElement>('#extInput');
const extAddBtn = $<HTMLButtonElement>('#extAddBtn');
const customExtList = $('#customExtList')!;
const builtinExtList = $('#builtinExtList')!;
const mimeInput = $<HTMLInputElement>('#mimeInput');
const mimeAddBtn = $<HTMLButtonElement>('#mimeAddBtn');
const mimeList = $('#mimeList')!;
const mimeResetBtn = $<HTMLButtonElement>('#mimeResetBtn');

// 今日统计（仅重置按钮，数值本身显示在 popup）
const resetStatsBtn = $<HTMLButtonElement>('#resetStatsBtn');

// 远程下载源 - 投递模式（地址/token/测试连接见上方 DOM 引用）
const remoteModeSelect = $<HTMLSelectElement>('#remoteModeSelect');
const remoteModeDesc = $('#remoteModeDesc')!;

// 拦截模式 / 最小文件大小（原 popup 快捷设置，迁移至此）
const interceptModeSelect = $<HTMLSelectElement>('#interceptModeSelect');
const modeHint = $('#modeHint')!;
const minSizeSelect = $<HTMLSelectElement>('#minSizeSelect');

// 排除域名管理
const domainInput = $<HTMLInputElement>('#domainInput');
const domainAddBtn = $<HTMLButtonElement>('#domainAddBtn');
const domainList = $('#domainList')!;
const domainEmptyHint = $('#domainEmptyHint')!;

// ===== 面板导航 =====
function activatePanel(name: string) {
  let found = false;
  for (const panel of panels) {
    const match = panel.id === `panel-${name}`;
    panel.classList.toggle('hidden', !match);
    if (match) found = true;
  }
  if (!found) return activatePanel('general');
  for (const item of navItems) {
    item.classList.toggle('active', item.dataset.panel === name);
  }
}

for (const item of navItems) {
  item.addEventListener('click', () => {
    const name = item.dataset.panel!;
    activatePanel(name);
    history.replaceState(null, '', `#${name}`);
  });
}

window.addEventListener('hashchange', () => {
  activatePanel(location.hash.replace(/^#/, '') || 'general');
});

// hash 直达面板（popup 远程区块的「配置服务器」→ #remote）。
// 放在模块顶层同步执行，不依赖 init 的异步存储读取。
activatePanel(location.hash.replace(/^#/, '') || 'general');

// ===== 主题（与 popup 共用 storage.local.theme） =====
type ThemeMode = 'light' | 'dark' | 'system';

function applyTheme(mode: ThemeMode) {
  const root = document.documentElement;
  if (mode === 'system') {
    root.removeAttribute('data-theme');
  } else {
    root.setAttribute('data-theme', mode);
  }
}

themeSelect.addEventListener('change', async () => {
  const mode = themeSelect.value as ThemeMode;
  applyTheme(mode);
  await browser.storage.local.set({ theme: mode });
});

// ===== 界面语言 =====
languageSelect.addEventListener('change', async () => {
  const value = languageSelect.value;
  if (value === 'auto') {
    await clearLocale();
  } else {
    await saveLocale(value);
  }
  applyI18nToDOM();
  updateModeHint(interceptModeSelect.value);
  await renderVerifyState();
});

// ===== Toast（复用 popup style.css 的 .toast 样式） =====
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

// ===== 拦截模式 / 最小文件大小（原 popup 快捷设置迁移） =====
type ModeKey = 'settings.hintSmart' | 'settings.hintAll';

const MODE_HINT_KEYS: Record<string, ModeKey> = {
  smart: 'settings.hintSmart',
  all: 'settings.hintAll',
};

function updateModeHint(mode: string): void {
  const key = MODE_HINT_KEYS[mode];
  modeHint.textContent = key ? t(key) : '';
}

interceptModeSelect.addEventListener('change', async () => {
  const mode = interceptModeSelect.value;
  updateModeHint(mode);
  await saveSettings({ interceptMode: mode as 'smart' | 'all' });
});

minSizeSelect.addEventListener('change', async () => {
  await saveSettings({ minFileSize: parseInt(minSizeSelect.value, 10) });
});

// ===== 远程配置 =====

/** 把 remote-server.ts 返回的稳定 message 前缀映射为本地化错误文案 */
function remoteTestErrorMessage(message?: string): string {
  if (message === 'remote_auth_failed') return t('remote.testAuthFailed');
  if (message === 'remote_not_configured') return t('remote.testNotConfigured');
  if (message && message.startsWith('remote_unreachable')) return t('remote.testUnreachable');
  return t('remote.testFailed', { message: message || 'unknown' });
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

// 远程模式门禁：仅当远程配置已通过「测试连接」验证时，才允许选择 fallback/always；
// 未验证时禁用这两个选项，解锁指引并入模式说明文案。
let _remoteVerified = false;

function refreshRemoteModeDesc(): void {
  const key = REMOTE_MODE_HINT_KEYS[remoteModeSelect.value];
  const parts = [key ? t(key) : ''];
  if (!_remoteVerified) parts.push(t('remote.verifyRequired'));
  remoteModeDesc.textContent = parts.filter(Boolean).join(' ');
}

function updateRemoteModeGate(verified: boolean): void {
  for (const opt of remoteModeSelect.options) {
    if (opt.value !== 'off') opt.disabled = !verified;
  }
  _remoteVerified = verified;
  refreshRemoteModeDesc();
}

async function renderVerifyState() {
  const settings = await loadSettings();
  verifyStateHint.textContent = settings.remoteVerified
    ? t('options.verifiedState')
    : t('options.unverifiedState');
  updateRemoteModeGate(settings.remoteVerified === true);
}

// 服务器地址（失焦保存；saveSettings 内部去除尾部斜杠并复位 remoteVerified）
remoteUrlInput.addEventListener('change', async () => {
  await saveSettings({ remoteUrl: remoteUrlInput.value.trim() });
  const current = await loadSettings();
  remoteUrlInput.value = current.remoteUrl;
  await renderVerifyState();
});

// Token（失焦保存；变更同样复位 remoteVerified）
remoteTokenInput.addEventListener('change', async () => {
  await saveSettings({ remoteToken: remoteTokenInput.value });
  await renderVerifyState();
});

// 远程下载源 - 投递模式
remoteModeSelect.addEventListener('change', async () => {
  await saveSettings({
    remoteMode: remoteModeSelect.value as 'off' | 'fallback' | 'always',
  });
  refreshRemoteModeDesc();
});

// 测试连接：探活 + 鉴权校验，结果写入 remoteVerified
remoteTestBtn.addEventListener('click', async () => {
  const remoteUrl = remoteUrlInput.value.trim().replace(/\/+$/, '');
  const remoteToken = remoteTokenInput.value;
  if (!remoteUrl) {
    remoteTestResult.textContent = t('remote.testNotConfigured');
    showToast(t('remote.testNotConfigured'), 'error');
    return;
  }
  remoteTestBtn.disabled = true;
  remoteTestResult.textContent = t('remote.testing');
  try {
    // 先落盘输入值（可能尚未触发 change），再验证
    await saveSettings({ remoteUrl, remoteToken });
    const result = await remoteVerify({ remoteUrl, remoteToken });
    if (result.success) {
      const msg = t('remote.testSuccess', {
        app: result.app || 'FluxDown',
        version: result.version || '',
      });
      remoteTestResult.textContent = msg;
      showToast(msg, 'success');
      await saveSettings({ remoteVerified: true });
    } else {
      const msg = remoteTestErrorMessage(result.message);
      remoteTestResult.textContent = msg;
      showToast(msg, 'error');
      await saveSettings({ remoteVerified: false });
    }
  } catch (e) {
    const msg = t('remote.testFailed', { message: String(e) });
    remoteTestResult.textContent = msg;
    showToast(msg, 'error');
    await saveSettings({ remoteVerified: false });
  } finally {
    remoteTestBtn.disabled = false;
  }
  await renderVerifyState();
});

// ===== 任务发送通知开关（本地 / 远程分开控制） =====
notifyLocalToggle.addEventListener('change', async () => {
  await saveSettings({ notifyLocalTask: notifyLocalToggle.checked });
});
notifyRemoteToggle.addEventListener('change', async () => {
  await saveSettings({ notifyRemoteTask: notifyRemoteToggle.checked });
});

// ===== 今日统计重置 =====
resetStatsBtn.addEventListener('click', async () => {
  const today = new Date().toDateString();
  await browser.storage.local.set({ stats: { sent: 0, failed: 0, date: today } });
  showToast(t('stats.resetDone'));
});

// ===== 拦截规则 =====

/** 生成一个标签 chip；onRemove 为空则只读（内置项） */
function makeTag(text: string, onRemove?: () => void): HTMLElement {
  const tag = document.createElement('span');
  tag.className = 'tag';
  const label = document.createElement('span');
  label.textContent = text;
  tag.appendChild(label);
  if (onRemove) {
    const btn = document.createElement('button');
    btn.className = 'tag-remove';
    btn.textContent = '\u00d7';
    btn.addEventListener('click', onRemove);
    tag.appendChild(btn);
  }
  return tag;
}

function renderCustomExtensions(exts: string[]) {
  customExtList.replaceChildren(
    ...exts.map((ext) =>
      makeTag(ext, async () => {
        const current = await loadSettings();
        await saveSettings({
          customExtensions: current.customExtensions.filter((e) => e !== ext),
        });
        renderCustomExtensions(
          current.customExtensions.filter((e) => e !== ext),
        );
      }),
    ),
  );
  customExtList.setAttribute('data-empty', t('options.rules.extEmpty'));
}

function renderMimeTypes(mimes: string[]) {
  mimeList.replaceChildren(
    ...mimes.map((mime) =>
      makeTag(mime, async () => {
        const current = await loadSettings();
        const next = current.interceptMimeTypes.filter((m) => m !== mime);
        await saveSettings({ interceptMimeTypes: next });
        renderMimeTypes(next);
      }),
    ),
  );
}

async function addCustomExtension() {
  const normalized = normalizeExtension(extInput.value);
  if (!normalized) {
    showToast(t('options.rules.extInvalid'), 'error');
    return;
  }
  const current = await loadSettings();
  if (
    BUILTIN_EXTENSIONS.includes(normalized) ||
    current.customExtensions.includes(normalized)
  ) {
    showToast(t('options.rules.extExists', { ext: normalized }), 'error');
    return;
  }
  const next = [...current.customExtensions, normalized];
  await saveSettings({ customExtensions: next });
  extInput.value = '';
  renderCustomExtensions(next);
  showToast(t('options.rules.extAdded', { ext: normalized }));
}

/** MIME 归一化：小写；`type/` 前缀匹配整族，`type/subtype` 精确匹配 */
function normalizeMime(input: string): string | null {
  const s = input.trim().toLowerCase();
  return /^[a-z0-9][a-z0-9.+-]*\/([a-z0-9][a-z0-9.+-]*)?$/.test(s) ? s : null;
}

async function addMimeType() {
  const normalized = normalizeMime(mimeInput.value);
  if (!normalized) {
    showToast(t('options.rules.mimeInvalid'), 'error');
    return;
  }
  const current = await loadSettings();
  if (current.interceptMimeTypes.includes(normalized)) {
    showToast(t('options.rules.extExists', { ext: normalized }), 'error');
    return;
  }
  const next = [...current.interceptMimeTypes, normalized];
  await saveSettings({ interceptMimeTypes: next });
  mimeInput.value = '';
  renderMimeTypes(next);
  showToast(t('options.rules.extAdded', { ext: normalized }));
}

extAddBtn.addEventListener('click', addCustomExtension);
extInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') addCustomExtension();
});
mimeAddBtn.addEventListener('click', addMimeType);
mimeInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') addMimeType();
});

// 恢复默认 MIME 列表（自定义扩展名不受影响）
mimeResetBtn.addEventListener('click', async () => {
  await saveSettings({
    interceptMimeTypes: [...DEFAULT_SETTINGS.interceptMimeTypes],
  });
  renderMimeTypes(DEFAULT_SETTINGS.interceptMimeTypes);
  showToast(t('options.rules.mimeResetDone'));
});

// ===== 排除域名管理（原 popup 逻辑迁移） =====
function renderDomainList(domains: string[]): void {
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

  domainList.querySelectorAll<HTMLButtonElement>('.domain-remove').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const domain = btn.dataset.domain!;
      await removeDomain(domain);
    });
  });
}

async function removeDomain(domain: string): Promise<void> {
  const current = await loadSettings();
  const domains = current.excludeDomains.filter((d) => d !== domain);
  if (domains.length !== current.excludeDomains.length) {
    await saveSettings({ excludeDomains: domains });
    renderDomainList(domains);
    showToast(t('domain.removed', { domain }));
  }
}

async function addDomain(domain: string): Promise<void> {
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

domainAddBtn.addEventListener('click', async () => {
  const val = domainInput.value.trim();
  if (val) {
    await addDomain(val);
    domainInput.value = '';
  }
});
domainInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') domainAddBtn.click();
});

// ===== 初始化 =====
async function init() {
  await initI18n();
  applyI18nToDOM();

  // 主题初始化
  const themeResult = (await browser.storage.local.get('theme')) ?? {};
  const theme = (themeResult.theme as ThemeMode) || 'system';
  applyTheme(theme);
  themeSelect.value = theme;

  // 语言选择器回显（未手动选择过 → auto）
  languageSelect.value = (await getSavedLocale()) || 'auto';

  versionLabel.textContent = `v${browser.runtime.getManifest().version}`;

  const settings = await loadSettings();
  remoteUrlInput.value = settings.remoteUrl || '';
  remoteTokenInput.value = settings.remoteToken || '';
  remoteModeSelect.value = settings.remoteMode || 'off';
  await renderVerifyState();

  // 任务发送通知开关
  notifyLocalToggle.checked = settings.notifyLocalTask !== false;
  notifyRemoteToggle.checked = settings.notifyRemoteTask !== false;

  // 拦截模式 / 最小文件大小
  interceptModeSelect.value = settings.interceptMode || 'smart';
  updateModeHint(settings.interceptMode || 'smart');
  minSizeSelect.value = String(settings.minFileSize);

  // 排除域名
  renderDomainList(settings.excludeDomains || []);

  // 拦截规则
  builtinExtList.replaceChildren(
    ...BUILTIN_EXTENSIONS.map((ext) => makeTag(ext)),
  );
  renderCustomExtensions(settings.customExtensions || []);
  renderMimeTypes(settings.interceptMimeTypes || []);
}

init().catch((e) => {
  console.error('[FluxDown Options] Init failed:', e);
});
