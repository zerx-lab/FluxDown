/**
 * 悬浮助手的本地状态：形态（悬浮窗 / 停靠右侧）跨访问保留在 localStorage；
 * 是否展开与当前会话只在本标签页内跨页面保留（sessionStorage），关掉标签页即回到收起。
 *
 * 键名与 `src/layouts/Layout.astro` 首帧前的停靠预设脚本同值（那段内联脚本不能 import）。
 */
export type WidgetMode = "float" | "dock";

export const MODE_KEY = "fd_ai_widget_mode";
export const OPEN_KEY = "fd_ai_widget_open";
export const SESSION_KEY = "fd_ai_widget_session";
/** 悬浮窗位置（左上角 px，JSON `{x, y}`）；缺省贴右下角。 */
export const POS_KEY = "fd_ai_widget_pos";
/** 停靠宽度（px）。 */
export const DOCK_WIDTH_KEY = "fd_ai_widget_dock_w";
/** `<html>` 上的停靠标记：宽屏下 body 让出右侧 `--ai-dock-w`（`global.css`）。 */
export const DOCK_ATTR = "data-ai-dock";

export const DOCK_WIDTH_DEFAULT = 420;
const DOCK_WIDTH_MIN = 320;
const DOCK_WIDTH_MAX = 960;
/** 停靠时页面至少保留的宽度。 */
const PAGE_MIN_WIDTH = 560;

const SESSION_ID_RE = /^[A-Za-z0-9-]{1,64}$/;

/** 隐私模式 / 禁用存储时读写会抛错：读回退缺省值，写直接放弃（只影响跨页面记忆）。 */
function read(storage: () => Storage, key: string): string | null {
  try {
    return storage().getItem(key);
  } catch {
    return null;
  }
}

function write(storage: () => Storage, key: string, value: string | null): void {
  try {
    if (value === null) storage().removeItem(key);
    else storage().setItem(key, value);
  } catch {
    // 存储不可用：本页照常工作，只是换页后不记得
  }
}

const local = () => localStorage;
const session = () => sessionStorage;

export const readMode = (): WidgetMode => (read(local, MODE_KEY) === "dock" ? "dock" : "float");
export const writeMode = (mode: WidgetMode) => write(local, MODE_KEY, mode);
export const readOpen = (): boolean => read(session, OPEN_KEY) === "1";
export const writeOpen = (open: boolean) => write(session, OPEN_KEY, open ? "1" : null);

export function readSession(): string | null {
  const id = read(session, SESSION_KEY);
  return id !== null && SESSION_ID_RE.test(id) ? id : null;
}
export const writeSession = (id: string | null) => write(session, SESSION_KEY, id);

/** 停靠且展开时让页面让出右侧；其余情况撤掉。 */
export function applyDock(docked: boolean): void {
  document.documentElement.toggleAttribute(DOCK_ATTR, docked);
}

export interface Point {
  x: number;
  y: number;
}

/** 停靠宽度夹到 [320, min(960, 视口 - 560)]（视口太窄时以下限为准）。 */
export function clampDockWidth(width: number, viewport: number): number {
  const max = Math.max(DOCK_WIDTH_MIN, Math.min(DOCK_WIDTH_MAX, viewport - PAGE_MIN_WIDTH));
  return Math.round(Math.min(max, Math.max(DOCK_WIDTH_MIN, width)));
}

export function readDockWidth(): number {
  const value = Number(read(local, DOCK_WIDTH_KEY));
  return Number.isFinite(value) && value > 0 ? value : DOCK_WIDTH_DEFAULT;
}
export const writeDockWidth = (width: number) => write(local, DOCK_WIDTH_KEY, String(width));

/** 让 body 让出的宽度与停靠面板宽度一致（`--ai-dock-w`）。 */
export function applyDockWidth(width: number): void {
  document.documentElement.style.setProperty("--ai-dock-w", `${width}px`);
}

/** 悬浮窗左上角夹在视口内（面板比视口大时贴左上）。 */
export function clampPos(pos: Point, width: number, height: number, viewW: number, viewH: number): Point {
  return {
    x: Math.round(Math.min(Math.max(0, viewW - width), Math.max(0, pos.x))),
    y: Math.round(Math.min(Math.max(0, viewH - height), Math.max(0, pos.y))),
  };
}

export function readPos(): Point | null {
  const raw = read(local, POS_KEY);
  if (raw === null) return null;
  try {
    const value: unknown = JSON.parse(raw);
    if (typeof value !== "object" || value === null) return null;
    const { x, y } = value as Record<string, unknown>;
    return typeof x === "number" && typeof y === "number" && Number.isFinite(x) && Number.isFinite(y) ? { x, y } : null;
  } catch {
    return null;
  }
}
export const writePos = (pos: Point | null) => write(local, POS_KEY, pos === null ? null : JSON.stringify(pos));