/**
 * FluxDown Popup Script — 任务面板
 *
 * 功能：
 * - 连接状态显示（头部徽标，逻辑不变）
 * - 高频开关（下载拦截 / 悬浮球）
 * - 任务面板：轮询 NMH 任务列表并增量渲染（下载中 / 最近完成）
 * - App 未运行时的空态引导（一键启动）
 * - 空闲态下的快捷 URL 下载
 * - 主题切换 / 多语言支持（中/英）
 *
 * 与 background 的通信严格遵循共享 Contract（type 字段）：
 *   - {type:'nmh-tasks'} → {ok, connected, tasks}
 *   - {type:'nmh-task-op', op, taskId} → {ok, message?}
 *   - {type:'nmh-open-file'|'nmh-reveal-file', taskId} → {ok, message?}
 *   - {type:'nmh-warmup'} → {ok}
 * popup 绝不直连 NMH，一律经 chrome.runtime.sendMessage 与 background 通信。
 *
 * 拦截模式 / 最小文件大小 / 远程下载源 / 排除域名 / 重置统计等低频设置
 * 已迁移到 options 页（entrypoints/options/），popup 仅保留高频操作。
 */

import { browser } from 'wxt/browser';
import { initI18n, applyI18nToDOM, t, getLocale, saveLocale } from '@/utils/i18n';
import { checkFluxDownAvailable } from '@/utils/download-dispatch';
import { loadSettings, saveSettings } from '@/utils/settings';
import type { RemoteMode } from '@/utils/settings';
import type { DetectedResource, ResourceType } from '@/utils/resource-types';
import { formatFileSize } from '@/utils/resource-types';
import {
  fileIconKind,
  resourceIconKind,
  fileIconSvg,
  ICON_CHECK_CIRCLE,
} from '@/utils/file-icons';

const $ = <T extends HTMLElement>(sel: string) => document.querySelector<T>(sel)!;

// ===== DOM 元素 =====
const statusBadge = $('#statusBadge')!;
const statusText = statusBadge.querySelector('.status-text')!;
const enableToggle = $<HTMLInputElement>('#enableToggle');
const enableHint = $('#enableHint')!;
const protocolToggle = $<HTMLInputElement>('#protocolToggle');
const protocolHint = $('#protocolHint')!;
const dotVisibleToggle = $<HTMLInputElement>('#dotVisibleToggle');
const notifyLocalToggle = $<HTMLInputElement>('#notifyLocalToggle');
const notifyRemoteToggle = $<HTMLInputElement>('#notifyRemoteToggle');
const remoteModeSelect = $<HTMLSelectElement>('#remoteModeSelect');
const remoteModeHint = $('#remoteModeHint')!;
const openSettingsBtn = $<HTMLButtonElement>('#openSettingsBtn');
const themeBtn = $<HTMLButtonElement>('#themeBtn');
const langBtn = $<HTMLButtonElement>('#langBtn');
const langLabel = langBtn.querySelector('.lang-label')!;

// 版本号
const versionLabel = $('#versionLabel')!;

// 统计（压缩单行，点击跳转 options）
const statsLine = $<HTMLButtonElement>('#statsLine');
const statSent = $('#statSent')!;
const statFailed = $('#statFailed')!;

// 任务面板
const taskDisconnectedEl = $('#taskDisconnected')!;
const taskEmptyEl = $('#taskEmpty')!;
const taskGroupsEl = $('#taskGroups')!;
const downloadingGroupEl = $('#downloadingGroup')!;
const completedGroupEl = $('#completedGroup')!;
const downloadingListEl = $('#downloadingList')!;
const completedListEl = $('#completedList')!;
const startAppBtn = $<HTMLButtonElement>('#startAppBtn');
const quickDownloadInput = $<HTMLInputElement>('#quickDownloadInput');
const quickDownloadBtn = $<HTMLButtonElement>('#quickDownloadBtn');

// 顶部 tab
const topTabs = $('#topTabs')!;
const paneTasks = $('#paneTasks')!;
const paneResources = $('#paneResources')!;
const paneSettings = $('#paneSettings')!;
const resourceBadge = $('#resourceBadge')!;

// 资源面板
const resTypeTabsEl = $('#resTypeTabs')!;
const resEmptyEl = $('#resEmpty')!;
const resListEl = $('#resList')!;
const resFooterEl = $('#resFooter')!;
const resSelectAll = $<HTMLInputElement>('#resSelectAll');
const resBatchBtn = $<HTMLButtonElement>('#resBatchBtn');
const resBatchCount = $('#resBatchCount')!;

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

// ===== 连接状态（头部徽标，逻辑不变） =====
// 并发守卫：设置变更可能连续触发刷新，只让最后一次的结果落到 UI。
let _statusEpoch = 0;

/**
 * 刷新头部连接徽标。探活逻辑复用 download-dispatch，与实际下载路由判定保持一致。
 * 与任务面板的 connected 状态是两个独立信号：头部徽标反映"能否投递下载"，
 * 任务面板的空态取决于 nmh-tasks 应答里的 connected（App 是否在运行）。
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
    browser.runtime
      .sendMessage({ action: 'appConfirmedUp' })
      .catch(() => {});
  } else {
    statusBadge.className = 'status-badge disconnected';
    statusText.textContent = t('header.disconnected');
  }
}

// ===== 任务面板：数据类型 & NMH 消息封装 =====

/** 与共享 Contract 逐字一致：popup ↔ background 的任务对象。 */
interface TaskBrief {
  taskId: string;
  fileName: string;
  status: number;
  downloadedBytes: number;
  totalBytes: number;
  speed: number;
  errorMessage?: string;
  createdAt: string;
}

interface NmhTasksResponse {
  ok: boolean;
  connected: boolean;
  tasks: TaskBrief[];
}

interface NmhOpResponse {
  ok: boolean;
  message?: string;
}

const TASK_STATUS = {
  PENDING: 0,
  DOWNLOADING: 1,
  PAUSED: 2,
  COMPLETED: 3,
  ERROR: 4,
  PREPARING: 5,
} as const;

async function nmhTasks(): Promise<NmhTasksResponse> {
  try {
    const res = (await browser.runtime.sendMessage({ type: 'nmh-tasks' })) as
      | NmhTasksResponse
      | undefined;
    return res ?? { ok: false, connected: false, tasks: [] };
  } catch {
    return { ok: false, connected: false, tasks: [] };
  }
}

async function nmhTaskOp(
  op: 'pause' | 'resume' | 'remove',
  taskId: string,
): Promise<NmhOpResponse> {
  try {
    const res = (await browser.runtime.sendMessage({
      type: 'nmh-task-op',
      op,
      taskId,
    })) as NmhOpResponse | undefined;
    return res ?? { ok: false };
  } catch (e) {
    return { ok: false, message: String(e) };
  }
}

async function nmhOpenFile(taskId: string): Promise<NmhOpResponse> {
  try {
    const res = (await browser.runtime.sendMessage({
      type: 'nmh-open-file',
      taskId,
    })) as NmhOpResponse | undefined;
    return res ?? { ok: false };
  } catch (e) {
    return { ok: false, message: String(e) };
  }
}

async function nmhRevealFile(taskId: string): Promise<NmhOpResponse> {
  try {
    const res = (await browser.runtime.sendMessage({
      type: 'nmh-reveal-file',
      taskId,
    })) as NmhOpResponse | undefined;
    return res ?? { ok: false };
  } catch (e) {
    return { ok: false, message: String(e) };
  }
}

async function nmhWarmup(): Promise<void> {
  try {
    await browser.runtime.sendMessage({ type: 'nmh-warmup' });
  } catch {
    // fire-and-forget：预热失败不影响持续轮询
  }
}

// ===== 任务面板：格式化辅助 =====

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex++;
  }
  const decimals = unitIndex === 0 ? 0 : value >= 100 ? 0 : value >= 10 ? 1 : 2;
  return `${value.toFixed(decimals)} ${units[unitIndex]}`;
}

function formatSpeed(bytesPerSec: number): string {
  if (!Number.isFinite(bytesPerSec) || bytesPerSec <= 0) return '0 B/s';
  return `${formatBytes(bytesPerSec)}/s`;
}

function progressPercent(downloaded: number, total: number): number {
  if (!Number.isFinite(total) || total <= 0) return 0;
  return Math.max(0, Math.min(100, (downloaded / total) * 100));
}

// 文件/资源图标：lucide 线性 SVG（@/utils/file-icons），按 kind 上色（icon-<kind> class）

const ICON_PAUSE =
  '<svg xmlns="http://www.w3.org/2000/svg" width="11" height="11" viewBox="0 0 24 24" fill="currentColor"><rect x="6" y="4" width="4" height="16" rx="1"/><rect x="14" y="4" width="4" height="16" rx="1"/></svg>';
const ICON_PLAY =
  '<svg xmlns="http://www.w3.org/2000/svg" width="11" height="11" viewBox="0 0 24 24" fill="currentColor"><path d="M8 5v14l11-7z"/></svg>';
const ICON_TRASH =
  '<svg xmlns="http://www.w3.org/2000/svg" width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="3 6 5 6 21 6"/><path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/><path d="M10 11v6"/><path d="M14 11v6"/></svg>';

/** 操作失败时的短暂红色抖动反馈 + toast 提示。 */
function flashError(btn: HTMLButtonElement, message?: string): void {
  btn.classList.remove('shake');
  void btn.offsetWidth; // 强制 reflow，允许连续触发同一动画
  btn.classList.add('shake');
  window.setTimeout(() => btn.classList.remove('shake'), 400);
  showToast(message || t('popup.task.opFailed'), 'error');
}

// ===== 任务面板：增量 DOM 渲染（按 taskId 复用节点，避免 1s 全量重建闪烁） =====

function reconcileTaskList<T extends { taskId: string }>(
  container: HTMLElement,
  items: T[],
  create: (item: T) => HTMLElement,
  update: (el: HTMLElement, item: T) => void,
): void {
  const existing = new Map<string, HTMLElement>();
  Array.from(container.children).forEach((child) => {
    const el = child as HTMLElement;
    const id = el.dataset.taskId;
    if (id) existing.set(id, el);
  });

  let anchor: HTMLElement | null = null;
  for (const item of items) {
    let el = existing.get(item.taskId);
    if (el) {
      update(el, item);
      existing.delete(item.taskId);
    } else {
      el = create(item);
    }
    const nextSibling: Element | null = anchor
      ? anchor.nextElementSibling
      : container.firstElementChild;
    if (nextSibling !== el) {
      container.insertBefore(el, nextSibling);
    }
    anchor = el;
  }

  for (const leftover of existing.values()) {
    leftover.remove();
  }
}

// --- 下载中卡片 ---

function bindActiveCardActions(card: HTMLElement): void {
  card.querySelectorAll<HTMLButtonElement>('.task-action-btn').forEach((btn) => {
    btn.addEventListener('click', async () => {
      if (btn.disabled) return;
      const op = btn.dataset.op as 'pause' | 'resume' | 'remove' | undefined;
      const taskId = card.dataset.taskId;
      if (!op || !taskId) return;
      btn.disabled = true;
      try {
        const res = await nmhTaskOp(op, taskId);
        if (!res.ok) {
          flashError(btn, res.message);
        } else {
          void pollTasksOnce();
        }
      } finally {
        btn.disabled = false;
      }
    });
  });
}

function createActiveTaskCard(task: TaskBrief): HTMLElement {
  const card = document.createElement('div');
  card.className = 'task-card';
  card.dataset.taskId = task.taskId;
  card.innerHTML = `
    <span class="task-icon"></span>
    <div class="task-info">
      <div class="task-name"></div>
      <div class="task-progress-track"><div class="task-progress-fill"></div></div>
      <div class="task-meta-row">
        <span class="task-meta-left"></span>
        <span class="task-meta-right"></span>
      </div>
      <div class="task-error-msg hidden"></div>
    </div>
    <div class="task-actions">
      <button class="task-action-btn" type="button" data-op="pause"></button>
      <button class="task-action-btn danger" type="button" data-op="remove">${ICON_TRASH}</button>
    </div>
  `;
  bindActiveCardActions(card);
  updateActiveTaskCard(card, task);
  return card;
}

function updateActiveTaskCard(card: HTMLElement, task: TaskBrief): void {
  const iconEl = card.querySelector<HTMLElement>('.task-icon')!;
  const nameEl = card.querySelector<HTMLElement>('.task-name')!;
  const fillEl = card.querySelector<HTMLElement>('.task-progress-fill')!;
  const leftEl = card.querySelector<HTMLElement>('.task-meta-left')!;
  const rightEl = card.querySelector<HTMLElement>('.task-meta-right')!;
  const errEl = card.querySelector<HTMLElement>('.task-error-msg')!;
  const toggleBtn = card.querySelector<HTMLButtonElement>(
    '[data-op="pause"], [data-op="resume"]',
  )!;
  const removeBtn = card.querySelector<HTMLButtonElement>('[data-op="remove"]')!;

  const kind = fileIconKind(task.fileName);
  if (iconEl.dataset.kind !== kind) {
    iconEl.dataset.kind = kind;
    iconEl.className = `task-icon icon-${kind}`;
    iconEl.innerHTML = fileIconSvg(kind);
  }
  if (nameEl.textContent !== task.fileName) {
    nameEl.textContent = task.fileName;
    nameEl.title = task.fileName;
  }

  const pct = progressPercent(task.downloadedBytes, task.totalBytes);
  const isPaused = task.status === TASK_STATUS.PAUSED;
  const isError = task.status === TASK_STATUS.ERROR;
  const isPreparing =
    task.status === TASK_STATUS.PENDING || task.status === TASK_STATUS.PREPARING;

  card.classList.toggle('paused', isPaused);
  card.classList.toggle('error', isError);
  card.classList.toggle('preparing', isPreparing);

  fillEl.style.width = isPreparing ? '100%' : `${pct}%`;

  leftEl.textContent = `${formatBytes(task.downloadedBytes)} / ${
    task.totalBytes > 0 ? formatBytes(task.totalBytes) : '--'
  }`;

  if (isError) {
    const msg = task.errorMessage || t('popup.task.errorGeneric');
    errEl.textContent = msg;
    errEl.title = msg;
    errEl.classList.remove('hidden');
    rightEl.textContent = '';
  } else {
    errEl.classList.add('hidden');
    if (isPaused) {
      rightEl.textContent = t('popup.task.paused');
    } else if (task.status === TASK_STATUS.PENDING) {
      rightEl.textContent = t('popup.task.pending');
    } else if (task.status === TASK_STATUS.PREPARING) {
      rightEl.textContent = t('popup.task.preparing');
    } else {
      rightEl.textContent = `${formatSpeed(task.speed)} · ${pct.toFixed(0)}%`;
    }
  }

  toggleBtn.dataset.op = isPaused ? 'resume' : 'pause';
  toggleBtn.innerHTML = isPaused ? ICON_PLAY : ICON_PAUSE;
  const toggleTitle = isPaused ? t('popup.task.resume') : t('popup.task.pause');
  toggleBtn.title = toggleTitle;
  toggleBtn.setAttribute('aria-label', toggleTitle);
  toggleBtn.disabled = isError;
  removeBtn.title = t('popup.task.remove');
  removeBtn.setAttribute('aria-label', t('popup.task.remove'));
}

// --- 最近完成卡片 ---

function bindCompletedCardActions(card: HTMLElement): void {
  card.querySelectorAll<HTMLButtonElement>('.ghost-btn').forEach((btn) => {
    btn.addEventListener('click', async () => {
      if (btn.disabled) return;
      const op = btn.dataset.op as 'open' | 'reveal' | undefined;
      const taskId = card.dataset.taskId;
      if (!op || !taskId) return;
      btn.disabled = true;
      try {
        const res = op === 'open' ? await nmhOpenFile(taskId) : await nmhRevealFile(taskId);
        if (!res.ok) {
          flashError(btn, res.message);
        }
      } finally {
        btn.disabled = false;
      }
    });
  });
}

function createCompletedTaskCard(task: TaskBrief): HTMLElement {
  const card = document.createElement('div');
  card.className = 'task-card completed';
  card.dataset.taskId = task.taskId;
  card.innerHTML = `
    <span class="task-icon icon-success">${ICON_CHECK_CIRCLE}</span>
    <div class="task-info">
      <div class="task-name"></div>
      <div class="task-meta-row"><span class="task-meta-left"></span></div>
    </div>
    <div class="task-actions">
      <button class="ghost-btn" type="button" data-op="open"></button>
      <button class="ghost-btn" type="button" data-op="reveal"></button>
    </div>
  `;
  bindCompletedCardActions(card);
  updateCompletedTaskCard(card, task);
  return card;
}

function updateCompletedTaskCard(card: HTMLElement, task: TaskBrief): void {
  const nameEl = card.querySelector<HTMLElement>('.task-name')!;
  const leftEl = card.querySelector<HTMLElement>('.task-meta-left')!;
  const openBtn = card.querySelector<HTMLButtonElement>('[data-op="open"]')!;
  const revealBtn = card.querySelector<HTMLButtonElement>('[data-op="reveal"]')!;

  if (nameEl.textContent !== task.fileName) {
    nameEl.textContent = task.fileName;
    nameEl.title = task.fileName;
  }
  leftEl.textContent =
    task.totalBytes > 0 ? formatBytes(task.totalBytes) : formatBytes(task.downloadedBytes);
  openBtn.textContent = t('popup.task.open');
  revealBtn.textContent = t('popup.task.reveal');
}

// ===== 任务面板：状态切换 + 轮询 =====

let appStarting = false;
let appStartingTimeoutId: number | undefined;

function setStartAppLoading(loading: boolean): void {
  startAppBtn.disabled = loading;
  startAppBtn.classList.toggle('loading', loading);
  startAppBtn.querySelector('.btn-label')!.textContent = loading
    ? t('popup.tasks.starting')
    : t('popup.tasks.startApp');
}

function resetStartAppState(): void {
  if (!appStarting) return;
  appStarting = false;
  if (appStartingTimeoutId !== undefined) {
    window.clearTimeout(appStartingTimeoutId);
    appStartingTimeoutId = undefined;
  }
  setStartAppLoading(false);
}

startAppBtn.addEventListener('click', () => {
  if (appStarting) return;
  appStarting = true;
  setStartAppLoading(true);
  void nmhWarmup();
  // 安全兜底：8s 内仍未连接（App 启动失败/超时）则恢复按钮，允许用户重试。
  appStartingTimeoutId = window.setTimeout(() => {
    appStarting = false;
    setStartAppLoading(false);
  }, 8000);
});

function renderTaskPanel(connected: boolean, tasks: TaskBrief[]): void {
  if (!connected) {
    taskDisconnectedEl.classList.remove('hidden');
    taskEmptyEl.classList.add('hidden');
    taskGroupsEl.classList.add('hidden');
    return;
  }
  resetStartAppState();

  const active = tasks.filter((it) => it.status !== TASK_STATUS.COMPLETED);
  const completed = tasks.filter((it) => it.status === TASK_STATUS.COMPLETED).slice(0, 5);

  if (active.length === 0 && completed.length === 0) {
    taskDisconnectedEl.classList.add('hidden');
    taskEmptyEl.classList.remove('hidden');
    taskGroupsEl.classList.add('hidden');
    return;
  }

  taskDisconnectedEl.classList.add('hidden');
  taskEmptyEl.classList.add('hidden');
  taskGroupsEl.classList.remove('hidden');

  downloadingGroupEl.classList.toggle('hidden', active.length === 0);
  completedGroupEl.classList.toggle('hidden', completed.length === 0);

  reconcileTaskList(downloadingListEl, active, createActiveTaskCard, updateActiveTaskCard);
  reconcileTaskList(completedListEl, completed, createCompletedTaskCard, updateCompletedTaskCard);
}

let pollTimerId: number | undefined;
let pollInFlight = false;

async function pollTasksOnce(): Promise<void> {
  if (pollInFlight) return;
  pollInFlight = true;
  try {
    const res = await nmhTasks();
    renderTaskPanel(res.connected, res.tasks);
  } finally {
    pollInFlight = false;
  }
}

/** popup 打开时立即轮询一次，随后每 1000ms 轮询；隐藏（document.hidden）即停。 */
function startPolling(): void {
  if (pollTimerId !== undefined) return;
  void pollTasksOnce();
  pollTimerId = window.setInterval(() => {
    void pollTasksOnce();
  }, 1000);
}

function stopPolling(): void {
  if (pollTimerId !== undefined) {
    window.clearInterval(pollTimerId);
    pollTimerId = undefined;
  }
}

document.addEventListener('visibilitychange', () => {
  if (document.hidden) {
    stopPolling();
  } else if (activePane === 'tasks') {
    startPolling();
  }
});

// ===== 空态下的快捷 URL 下载 =====
// 复用 background 现有的手动下载入口（downloadResource），与资源面板/嗅探触发下载走同一条路径。
async function submitQuickDownload(): Promise<void> {
  const url = quickDownloadInput.value.trim();
  if (!url) return;

  try {
    const parsed = new URL(url);
    if (!['http:', 'https:', 'ftp:'].includes(parsed.protocol)) {
      throw new Error('unsupported protocol');
    }
  } catch {
    showToast(t('popup.quickDownload.invalidUrl'), 'error');
    return;
  }

  quickDownloadBtn.disabled = true;
  try {
    const res = (await browser.runtime.sendMessage({
      action: 'downloadResource',
      url,
    })) as { success?: boolean; message?: string } | undefined;
    if (res?.success) {
      showToast(t('popup.quickDownload.sent'));
      quickDownloadInput.value = '';
      void pollTasksOnce();
    } else {
      showToast(res?.message || t('popup.quickDownload.failed'), 'error');
    }
  } catch {
    showToast(t('popup.quickDownload.failed'), 'error');
  } finally {
    quickDownloadBtn.disabled = false;
  }
}

quickDownloadBtn.addEventListener('click', () => {
  void submitQuickDownload();
});
quickDownloadInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') void submitQuickDownload();
});

// ===== 顶部 Tab 切换（任务 / 资源 / 设置） =====

type PaneKey = 'tasks' | 'resources' | 'settings';
let activePane: PaneKey = 'tasks';

const PANES: Record<PaneKey, HTMLElement> = {
  tasks: paneTasks,
  resources: paneResources,
  settings: paneSettings,
};

function switchPane(pane: PaneKey): void {
  if (pane === activePane) return;
  activePane = pane;
  for (const btn of topTabs.querySelectorAll<HTMLButtonElement>('.top-tab')) {
    btn.classList.toggle('active', btn.dataset.pane === pane);
  }
  for (const [key, el] of Object.entries(PANES)) {
    el.classList.toggle('hidden', key !== pane);
  }
  // 任务轮询只在任务 pane 可见时跑；资源 pane 每次进入取一次新快照
  if (pane === 'tasks') {
    startPolling();
  } else {
    stopPolling();
  }
  if (pane === 'resources') {
    void refreshResources();
  }
}

topTabs.addEventListener('click', (e) => {
  const btn = (e.target as HTMLElement).closest<HTMLButtonElement>('.top-tab');
  const pane = btn?.dataset.pane as PaneKey | undefined;
  if (pane) switchPane(pane);
});

// ===== 资源面板（当前活跃 tab 的嗅探结果） =====
// 与页内浮动面板同一数据源（background resource-store），popup 侧做
// 轻量列表：类型筛选 + 预览 + 单个/批量下载，选轨等重交互留在页内面板。

/** 与页内面板一致的类型 tab 顺序（无资源的类型不渲染）。 */
const RES_TABS: Array<{ key: ResourceType | 'all'; i18nKey: string }> = [
  { key: 'all', i18nKey: 'panel.tabAll' },
  { key: 'video', i18nKey: 'panel.tabVideo' },
  { key: 'audio', i18nKey: 'panel.tabAudio' },
  { key: 'document', i18nKey: 'panel.tabDocs' },
  { key: 'archive', i18nKey: 'panel.tabArchive' },
  { key: 'stream', i18nKey: 'panel.tabStream' },
  { key: 'subtitle', i18nKey: 'panel.tabSubtitle' },
  { key: 'magnet', i18nKey: 'panel.tabMagnet' },
  { key: 'other', i18nKey: 'panel.tabOther' },
];

let resources: DetectedResource[] = [];
let resActiveType: ResourceType | 'all' = 'all';
const resSelectedIds = new Set<string>();

async function refreshResources(): Promise<void> {
  try {
    const [activeTab] = await browser.tabs.query({ active: true, currentWindow: true });
    if (!activeTab?.id) {
      resources = [];
    } else {
      const res = (await browser.runtime.sendMessage({
        action: 'getResources',
        tabId: activeTab.id,
      })) as { resources?: DetectedResource[] } | undefined;
      resources = res?.resources ?? [];
    }
  } catch {
    resources = [];
  }
  // 快照刷新后清掉已消失资源的选中态
  const alive = new Set(resources.map((r) => r.id));
  for (const id of resSelectedIds) {
    if (!alive.has(id)) resSelectedIds.delete(id);
  }
  if (resources.every((r) => r.type !== resActiveType) && resActiveType !== 'all') {
    resActiveType = 'all';
  }
  updateResourceBadge();
  renderResTabs();
  renderResList();
}

function updateResourceBadge(): void {
  resourceBadge.textContent = resources.length > 99 ? '99+' : String(resources.length);
  resourceBadge.classList.toggle('hidden', resources.length === 0);
}

function filteredResources(): DetectedResource[] {
  return resActiveType === 'all'
    ? resources
    : resources.filter((r) => r.type === resActiveType);
}

function renderResTabs(): void {
  resTypeTabsEl.textContent = '';
  for (const tab of RES_TABS) {
    const count =
      tab.key === 'all'
        ? resources.length
        : resources.filter((r) => r.type === tab.key).length;
    if (tab.key !== 'all' && count === 0) continue;
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = `res-tab${resActiveType === tab.key ? ' active' : ''}`;
    btn.textContent = `${t(tab.i18nKey)} ${count}`;
    btn.addEventListener('click', () => {
      resActiveType = tab.key;
      renderResTabs();
      renderResList();
    });
    resTypeTabsEl.appendChild(btn);
  }
}

function resDownloadPayload(r: DetectedResource) {
  return {
    url: r.url,
    referrer: r.pageUrl || undefined,
    filename: r.filename,
    fileSize: r.size > 0 ? r.size : undefined,
    mimeType: r.mimeType,
  };
}

// ===== 资源预览（与页内浮动面板同规则：图片/音频/视频直链/流分片按类型分发，
// 原生播放失败诚实降级提示，禁止引入 hls.js） =====

const SVG_PREVIEW =
  '<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8Z"/><circle cx="12" cy="12" r="3"/></svg>';
const SVG_PREVIEW_CLOSE =
  '<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>';

let previewModalEl: HTMLElement | null = null;

/** 仅这些类型显示预览按钮；document/archive/其他不可预览。 */
function isPreviewable(r: DetectedResource): boolean {
  return (
    r.type === 'image' || r.type === 'video' || r.type === 'audio' || r.type === 'stream'
  );
}

type PreviewKind = 'image' | 'audio' | 'direct-video' | 'hls' | 'dash' | 'fragment' | 'unsupported';

/** 按结构特征（type + URL 后缀）分发预览渲染方式，不做站点特判。 */
function previewKind(r: DetectedResource): PreviewKind {
  const mime = r.mimeType?.toLowerCase() || '';
  const url = r.url.toLowerCase();
  if (r.type === 'image' || mime.startsWith('image/')) return 'image';
  if (r.type === 'stream') {
    if (url.includes('.m3u8')) return 'hls';
    if (url.includes('.mpd')) return 'dash';
    return 'fragment'; // m4s 等分片：单文件常缺 moov/init，原生播放大概率失败
  }
  if (r.type === 'audio') return 'audio';
  if (r.type === 'video') return 'direct-video';
  return 'unsupported';
}

function ensurePreviewModal(): HTMLElement {
  if (previewModalEl) return previewModalEl;
  const modal = document.createElement('div');
  modal.className = 'res-preview-modal';
  modal.innerHTML = `
    <div class="res-preview-card">
      <div class="preview-header">
        <span class="preview-title"></span>
        <button type="button" class="preview-close" title="${t('panel.previewClose')}">${SVG_PREVIEW_CLOSE}</button>
      </div>
      <div class="preview-body"></div>
    </div>
  `;
  // 点遮罩关闭；点卡片内部（含控件交互）不关闭
  modal.addEventListener('click', (e) => {
    if (e.target === modal) closePreview();
  });
  modal.querySelector('.preview-close')?.addEventListener('click', closePreview);
  document.body.appendChild(modal);
  previewModalEl = modal;
  return modal;
}

/** 用降级提示替换预览区内容（不黑屏、不假装能播放）。 */
function showPreviewFallback(bodyEl: HTMLElement, message: string): void {
  bodyEl.textContent = '';
  const fb = document.createElement('div');
  fb.className = 'preview-fallback';
  fb.textContent = message;
  bodyEl.appendChild(fb);
}

/** 打开预览弹层：按资源类型分发渲染，原生播放失败诚实降级。 */
function openPreview(r: DetectedResource): void {
  const modal = ensurePreviewModal();
  const titleEl = modal.querySelector('.preview-title') as HTMLElement;
  const bodyEl = modal.querySelector('.preview-body') as HTMLElement;
  titleEl.textContent = r.filename || r.url;
  bodyEl.textContent = '';

  const kind = previewKind(r);
  if (kind === 'image') {
    const img = document.createElement('img');
    img.className = 'preview-media';
    img.addEventListener('load', () => {
      const hint = document.createElement('div');
      hint.className = 'preview-hint';
      hint.textContent = `${img.naturalWidth} × ${img.naturalHeight}`;
      bodyEl.appendChild(hint);
    });
    img.addEventListener('error', () => showPreviewFallback(bodyEl, t('panel.previewFailed')));
    img.src = r.url;
    bodyEl.appendChild(img);
  } else if (kind === 'audio') {
    const audio = document.createElement('audio');
    audio.className = 'preview-media';
    audio.controls = true;
    audio.addEventListener('error', () => showPreviewFallback(bodyEl, t('panel.previewFailed')));
    audio.src = r.url;
    bodyEl.appendChild(audio);
  } else if (kind === 'direct-video') {
    const video = document.createElement('video');
    video.className = 'preview-media';
    video.controls = true;
    video.autoplay = true;
    video.muted = true;
    video.addEventListener('error', () => showPreviewFallback(bodyEl, t('panel.previewFailed')));
    video.src = r.url;
    bodyEl.appendChild(video);
  } else if (kind === 'fragment' || kind === 'hls' || kind === 'dash') {
    // m4s 分片 / hls / dash：浏览器原生尝试，失败诚实降级（禁止引入 hls.js）
    const video = document.createElement('video');
    video.className = 'preview-media';
    video.controls = true;
    const fallbackMsg =
      kind === 'hls' ? t('panel.previewHlsUnsupported')
      : kind === 'dash' ? t('panel.previewDashUnsupported')
      : t('panel.previewFragmentUnsupported');
    video.addEventListener('error', () => showPreviewFallback(bodyEl, fallbackMsg));
    video.src = r.url;
    bodyEl.appendChild(video);
  } else {
    showPreviewFallback(bodyEl, t('panel.previewUnsupported'));
  }

  modal.classList.add('visible');
}

/** 关闭预览弹层，销毁 video/audio 元素释放资源（暂停 + 清空 src）。 */
function closePreview(): void {
  if (!previewModalEl) return;
  previewModalEl.classList.remove('visible');
  for (const el of previewModalEl.querySelectorAll('video, audio')) {
    const m = el as HTMLMediaElement;
    m.pause();
    m.src = '';
    m.load();
  }
}

// Esc 关闭预览弹层
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && previewModalEl?.classList.contains('visible')) {
    closePreview();
  }
});

function buildResRow(r: DetectedResource): HTMLElement {
  const row = document.createElement('div');
  row.className = 'res-row';

  const check = document.createElement('input');
  check.type = 'checkbox';
  check.className = 'res-check';
  check.checked = resSelectedIds.has(r.id);
  check.addEventListener('change', () => {
    if (check.checked) resSelectedIds.add(r.id);
    else resSelectedIds.delete(r.id);
    updateResBatchBar();
  });
  row.appendChild(check);

  const icon = document.createElement('span');
  const resKind = resourceIconKind(r.type);
  icon.className = `res-icon icon-${resKind}`;
  icon.innerHTML = fileIconSvg(resKind, 14);
  row.appendChild(icon);

  const info = document.createElement('div');
  info.className = 'res-info';
  const name = document.createElement('div');
  name.className = 'res-name';
  name.textContent = r.filename;
  name.title = r.url;
  info.appendChild(name);
  const size = formatFileSize(r.size);
  if (size) {
    const meta = document.createElement('div');
    meta.className = 'res-meta';
    meta.textContent = size;
    info.appendChild(meta);
  }
  row.appendChild(info);

  if (isPreviewable(r)) {
    const pv = document.createElement('button');
    pv.type = 'button';
    pv.className = 'res-preview-btn';
    pv.title = t('panel.previewTitle');
    pv.innerHTML = SVG_PREVIEW;
    pv.addEventListener('click', () => openPreview(r));
    row.appendChild(pv);
  }

  const dl = document.createElement('button');
  dl.type = 'button';
  dl.className = 'res-dl-btn';
  dl.title = t('popup.quickDownload.button');
  dl.innerHTML =
    '<svg xmlns="http://www.w3.org/2000/svg" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>';
  dl.addEventListener('click', async () => {
    dl.disabled = true;
    try {
      const res = (await browser.runtime.sendMessage({
        action: 'downloadResource',
        ...resDownloadPayload(r),
      })) as { success?: boolean; message?: string } | undefined;
      if (res?.success) {
        showToast(t('popup.quickDownload.sent'));
      } else {
        showToast(res?.message || t('popup.quickDownload.failed'), 'error');
        dl.disabled = false;
      }
    } catch {
      showToast(t('popup.quickDownload.failed'), 'error');
      dl.disabled = false;
    }
  });
  row.appendChild(dl);

  return row;
}

function renderResList(): void {
  const items = filteredResources();
  resListEl.textContent = '';
  for (const r of items) resListEl.appendChild(buildResRow(r));

  const empty = resources.length === 0;
  resEmptyEl.classList.toggle('hidden', !empty);
  resTypeTabsEl.classList.toggle('hidden', empty);
  resFooterEl.classList.toggle('hidden', empty);
  updateResBatchBar();
}

function updateResBatchBar(): void {
  const visible = filteredResources();
  const selectedVisible = visible.filter((r) => resSelectedIds.has(r.id)).length;
  resBatchCount.textContent = String(resSelectedIds.size);
  resBatchBtn.disabled = resSelectedIds.size === 0;
  resSelectAll.checked = visible.length > 0 && selectedVisible === visible.length;
}

resSelectAll.addEventListener('change', () => {
  const visible = filteredResources();
  if (resSelectAll.checked) {
    for (const r of visible) resSelectedIds.add(r.id);
  } else {
    for (const r of visible) resSelectedIds.delete(r.id);
  }
  renderResList();
});

resBatchBtn.addEventListener('click', async () => {
  const items = resources.filter((r) => resSelectedIds.has(r.id));
  if (items.length === 0) return;
  resBatchBtn.disabled = true;
  try {
    await browser.runtime.sendMessage({
      action: 'batchDownload',
      items: items.map(resDownloadPayload),
    });
    showToast(t('popup.quickDownload.sent'));
    resSelectedIds.clear();
    renderResList();
  } catch {
    showToast(t('popup.quickDownload.failed'), 'error');
    resBatchBtn.disabled = false;
  }
});

// ===== 初始化 =====
// 性能关键路径：popup 弹出到首次完整渲染。
// 1. 所有 storage 读取合并为一轮并行（i18n / 主题+悬浮球+统计 / 设置）；
// 2. 探活（refreshConnectionStatus）与任务轮询（startPolling）都不阻塞 UI 回显——
//    off 模式下 refreshConnectionStatus 要 connectNative 冷启动 NMH 进程、
//    always 模式下 remotePing 超时可达 4s；任务区在收到首次轮询结果前保持
//    disconnected 空态（静态 HTML 默认隐藏，首次渲染由 pollTasksOnce 落地）。
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

  // 高频开关
  enableToggle.checked = settings.enabled;
  updateEnableHint(settings.enabled);
  protocolToggle.checked = settings.enableFluxdownProtocol === true;
  updateProtocolHint(settings.enableFluxdownProtocol === true);
  dotVisibleToggle.checked = localState?.['fluxdown_dot_visible'] !== false;

  // 任务发送通知开关 + 远程投递模式（与 options 页同一 settings 源，双入口镜像）
  notifyLocalToggle.checked = settings.notifyLocalTask !== false;
  notifyRemoteToggle.checked = settings.notifyRemoteTask !== false;
  remoteModeSelect.value = settings.remoteMode || 'off';
  updateRemoteModeGate(settings.remoteVerified === true);

  // 统计（数据已随批量读取取回）
  await loadStats(
    localState?.stats as { sent?: number; failed?: number; date?: string } | undefined,
  );

  // 连接探活：fire-and-forget，结果异步更新头部徽标，不阻塞弹出渲染。
  void refreshConnectionStatus().catch(() => {});

  // 任务面板轮询：立即一次 + 1s 间隔，隐藏/切走 tab 即停（见 visibilitychange 与 switchPane）。
  startPolling();

  // 资源徽标：取一次当前活跃 tab 的嗅探快照（fire-and-forget，不阻塞弹出渲染）。
  void refreshResources().catch(() => {});
}

function updateEnableHint(enabled: boolean) {
  enableHint.textContent = enabled ? t('switch.enabled') : t('switch.disabled');
}

function updateProtocolHint(enabled: boolean) {
  protocolHint.textContent = enabled
    ? t('options.protocol.enabled')
    : t('options.protocol.disabled');
}

/** 远程模式提示 + 未验证连接时禁用非 off 选项（与 options 页同规则） */
let _remoteVerified = false;

const REMOTE_MODE_HINT_KEYS: Record<
  string,
  'remote.modeHintOff' | 'remote.modeHintFallback' | 'remote.modeHintAlways'
> = {
  off: 'remote.modeHintOff',
  fallback: 'remote.modeHintFallback',
  always: 'remote.modeHintAlways',
};

function refreshRemoteModeHint(): void {
  const key = REMOTE_MODE_HINT_KEYS[remoteModeSelect.value];
  const parts = [key ? t(key) : ''];
  if (!_remoteVerified) parts.push(t('remote.verifyRequired'));
  remoteModeHint.textContent = parts.filter(Boolean).join(' ');
}

function updateRemoteModeGate(verified: boolean): void {
  for (const opt of remoteModeSelect.options) {
    if (opt.value !== 'off') opt.disabled = !verified;
  }
  _remoteVerified = verified;
  refreshRemoteModeHint();
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
  updateEnableHint(enableToggle.checked);
  refreshRemoteModeHint();
  // applyI18nToDOM 会把 startAppBtn 的 .btn-label 重置为默认文案，
  // loading 态需要重新套用翻译后的"启动中…"。
  if (appStarting) setStartAppLoading(true);
  // 任务卡片的文件名/进度/状态文案不经 data-i18n（JS 动态渲染），
  // 清空后触发一次立即轮询以翻译后的语言重建。
  downloadingListEl.replaceChildren();
  completedListEl.replaceChildren();
  void pollTasksOnce();
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

// fluxdown:// 自定义协议开关
protocolToggle.addEventListener('change', async () => {
  await saveSettings({ enableFluxdownProtocol: protocolToggle.checked });
  updateProtocolHint(protocolToggle.checked);
});

// 任务发送通知开关（本地 / 远程分开控制）
notifyLocalToggle.addEventListener('change', async () => {
  await saveSettings({ notifyLocalTask: notifyLocalToggle.checked });
});
notifyRemoteToggle.addEventListener('change', async () => {
  await saveSettings({ notifyRemoteTask: notifyRemoteToggle.checked });
});

// 远程下载源 - 投递模式
remoteModeSelect.addEventListener('change', async () => {
  await saveSettings({ remoteMode: remoteModeSelect.value as RemoteMode });
  refreshRemoteModeHint();
});

// 启用/禁用开关
enableToggle.addEventListener('change', async () => {
  const enabled = enableToggle.checked;
  updateEnableHint(enabled);
  await saveSettings({ enabled });
});

// 统计单行 → 跳转 options
statsLine.addEventListener('click', () => {
  browser.runtime.openOptionsPage();
  window.close();
});

// 左下角"全部设置"入口 → 打开配置页
openSettingsBtn.addEventListener('click', () => {
  browser.runtime.openOptionsPage();
  window.close();
});

// ===== 启动 =====
// R8-3 修复：init 是顶层 async 调用，加 .catch 防止意外异常成为未捕获 rejection
init().catch((e) => {
  console.error('[FluxDown Popup] Init failed:', e);
});
