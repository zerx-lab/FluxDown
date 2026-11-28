import { lazy, Suspense, useCallback, useEffect, useRef, useState, useSyncExternalStore, type CSSProperties, type KeyboardEvent, type MouseEvent, type PointerEvent } from "react";
import { Bot, Sparkles } from "lucide-react";
import type { Lang } from "@/i18n/config";
import { assistant } from "@/i18n/messages/assistant";
import { Spinner } from "@/components/account/dashboard/ui";
import { isAiEntryVisible, subscribeAiEntry } from "@/lib/account/ai-entry";
import {
  applyDock, applyDockWidth, clampDockWidth, clampPos, readDockWidth, readMode, readOpen, readPos,
  writeDockWidth, writeMode, writeOpen, writePos, DOCK_WIDTH_DEFAULT, type Point, type WidgetMode,
} from "./storage";

/** 与 `global.css` 的断点一致：≥640 才是悬浮窗（更窄铺满整屏），≥1024 停靠才生效。 */
const FLOAT_MQ = "(min-width: 640px)";
const DOCK_MQ = "(min-width: 1024px)";
const RESIZE_STEP = 16;
/** 拖动 / 调宽期间 `<html>` 上的标记：禁选中文字、锁定光标（`global.css`）。 */
const DRAGGING_ATTR = "data-ai-dragging";

/** 跟随指针直到松开；松开时回调一次（用于落盘）。 */
function trackPointer(event: PointerEvent<HTMLElement>, kind: "move" | "resize", onMove: (e: globalThis.PointerEvent) => void, onEnd: () => void) {
  const el = event.currentTarget;
  el.setPointerCapture(event.pointerId);
  document.documentElement.setAttribute(DRAGGING_ATTR, kind);
  const end = () => {
    el.removeEventListener("pointermove", onMove);
    el.removeEventListener("pointerup", end);
    el.removeEventListener("pointercancel", end);
    document.documentElement.removeAttribute(DRAGGING_ATTR);
    onEnd();
  };
  el.addEventListener("pointermove", onMove);
  el.addEventListener("pointerup", end);
  el.addEventListener("pointercancel", end);
}

/** 元素沿给定方向还能否滚动（只看 overflow 为 auto / scroll 的滚动容器）。 */
function canScroll(el: Element, horizontal: boolean, delta: number): boolean {
  const style = getComputedStyle(el);
  const overflow = horizontal ? style.overflowX : style.overflowY;
  if (overflow !== "auto" && overflow !== "scroll") return false;
  if (horizontal) {
    const max = el.scrollWidth - el.clientWidth;
    return max > 0 && (delta < 0 ? el.scrollLeft > 0 : el.scrollLeft < max - 1);
  }
  const max = el.scrollHeight - el.clientHeight;
  return max > 0 && (delta < 0 ? el.scrollTop > 0 : el.scrollTop < max - 1);
}

/** 面板（对话、Markdown、SSE）只在第一次展开时加载，收起状态的页面只多一个按钮。 */
const WidgetPanel = lazy(() => import("./WidgetPanel"));

/**
 * 全站右下角 AI 助手：悬浮按钮 -> 悬浮窗（拖标题栏移动，双击复位），宽屏可停靠到右侧
 * （拖左缘调宽，双击复位；页面让出同样宽度）。形态、位置与宽度跨访问记忆；展开状态在本标签页内
 * 跨页面保留（登录回跳后自动重新展开）。
 */
function Widget({ lang }: { lang: Lang }) {
  const t = assistant[lang];
  const [open, setOpen] = useState(false);
  /** 展开过一次后面板常驻（收起只隐藏不卸载）：进行中的回答不会因为关窗被掐断。 */
  const [everOpen, setEverOpen] = useState(false);
  if (open && !everOpen) setEverOpen(true);
  const [mode, setMode] = useState<WidgetMode>("float");
  const launcherRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<Point | null>(null);
  const posRef = useRef<Point | null>(null);
  posRef.current = pos;
  // 惰性初值：首个 effect 就写入记忆的宽度，不会先把首帧预设的 `--ai-dock-w` 覆盖回缺省值
  const [dockWidth, setDockWidth] = useState(() =>
    typeof window === "undefined" ? DOCK_WIDTH_DEFAULT : clampDockWidth(readDockWidth(), window.innerWidth),
  );
  const dockWidthRef = useRef(dockWidth);
  dockWidthRef.current = dockWidth;

  useEffect(() => {
    setMode(readMode());
    setOpen(readOpen());
    setPos(readPos());
  }, []);

  useEffect(() => {
    applyDock(open && mode === "dock");
  }, [open, mode]);

  useEffect(() => {
    applyDockWidth(dockWidth);
  }, [dockWidth]);

  /** 视口变化后把悬浮窗拉回可见区、停靠宽度收回上限（只改显示，不改记忆的值）。 */
  const fitViewport = useCallback(() => {
    setDockWidth((w) => clampDockWidth(w, window.innerWidth));
    const panel = panelRef.current;
    setPos((p) => (p && panel ? clampPos(p, panel.offsetWidth, panel.offsetHeight, window.innerWidth, window.innerHeight) : p));
  }, []);

  useEffect(() => {
    if (!open) return;
    fitViewport();
    window.addEventListener("resize", fitViewport);
    return () => window.removeEventListener("resize", fitViewport);
  }, [open, fitViewport]);

  // 面板内滚轮不外溢到页面：沿目标向上找面板内还能朝该方向滚动的元素，找不到就吞掉这次滚动。
  // 能滚的元素自己滚，到边界后由 CSS `overscroll-behavior: contain` 截住滚动链。
  useEffect(() => {
    const panel = panelRef.current;
    if (!open || !panel) return;
    const onWheel = (event: WheelEvent) => {
      if (event.ctrlKey) return; // 触控板缩放
      const horizontal = Math.abs(event.deltaX) > Math.abs(event.deltaY);
      const delta = horizontal ? event.deltaX : event.deltaY;
      if (delta === 0) return;
      for (let el = event.target instanceof Element ? event.target : null; el && el !== panel; el = el.parentElement) {
        if (canScroll(el, horizontal, delta)) return;
      }
      event.preventDefault();
    };
    panel.addEventListener("wheel", onWheel, { passive: false });
    return () => panel.removeEventListener("wheel", onWheel);
  }, [open]);

  /** 当前是否以悬浮窗呈现（窄屏的「停靠」也按悬浮窗显示，同样可拖）。 */
  const floating = () => matchMedia(FLOAT_MQ).matches && (mode === "float" || !matchMedia(DOCK_MQ).matches);
  const docked = () => mode === "dock" && matchMedia(DOCK_MQ).matches;

  const onPanelPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    const target = event.target instanceof Element ? event.target : null;
    if (event.button !== 0 || !target?.closest("[data-drag-handle]") || target.closest("button, a, input, textarea, select")) return;
    const panel = panelRef.current;
    if (!panel || !floating()) return;
    event.preventDefault();
    const rect = panel.getBoundingClientRect();
    const dx = event.clientX - rect.left;
    const dy = event.clientY - rect.top;
    trackPointer(
      event,
      "move",
      (e) => setPos(clampPos({ x: e.clientX - dx, y: e.clientY - dy }, rect.width, rect.height, window.innerWidth, window.innerHeight)),
      () => writePos(posRef.current),
    );
  };

  const onPanelDoubleClick = (event: MouseEvent<HTMLDivElement>) => {
    const target = event.target instanceof Element ? event.target : null;
    if (!target?.closest("[data-drag-handle]") || target.closest("button, a") || !floating()) return;
    setPos(null);
    writePos(null);
  };

  const resizeDock = (width: number) => {
    const next = clampDockWidth(width, window.innerWidth);
    setDockWidth(next);
    writeDockWidth(next);
  };

  const onResizePointerDown = (event: PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0 || !docked()) return;
    event.preventDefault();
    trackPointer(
      event,
      "resize",
      (e) => setDockWidth(clampDockWidth(window.innerWidth - e.clientX, window.innerWidth)),
      () => writeDockWidth(dockWidthRef.current),
    );
  };

  const onResizeKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "ArrowLeft") resizeDock(dockWidth + RESIZE_STEP);
    else if (event.key === "ArrowRight") resizeDock(dockWidth - RESIZE_STEP);
    else if (event.key === "Home") resizeDock(DOCK_WIDTH_DEFAULT);
    else return;
    event.preventDefault();
  };

  /** 用户亲手展开 / 收起过：只有这时才搬焦点（跨页面恢复展开不抢页面焦点）。 */
  const toggled = useRef(false);

  const show = useCallback(() => {
    toggled.current = true;
    writeOpen(true);
    setOpen(true);
  }, []);

  const close = useCallback(() => {
    writeOpen(false);
    setOpen(false);
    toggled.current = true;
  }, []);

  const changeMode = useCallback((next: WidgetMode) => {
    writeMode(next);
    setMode(next);
  }, []);

  // 展开后把焦点移进面板，键盘用户不必从页面顶部 Tab 过来；收起后还给悬浮按钮
  useEffect(() => {
    if (!toggled.current) return;
    if (open) panelRef.current?.focus();
    else launcherRef.current?.focus();
  }, [open]);

  return (
    <>
      {!open && (
        <button ref={launcherRef} type="button" onClick={show} aria-label={t.widget.launcher} aria-haspopup="dialog" className="ai-launcher">
          <span className="ai-launcher-hint" aria-hidden>
            {t.widget.launcherHint}
          </span>
          <span className="ai-launcher-orb" aria-hidden>
            <Bot size={22} />
            <Sparkles size={11} className="ai-launcher-spark" />
          </span>
        </button>
      )}
      {everOpen && (
        <div
          ref={panelRef}
          role="dialog"
          aria-modal="false"
          aria-labelledby="ai-widget-title"
          tabIndex={-1}
          data-mode={mode}
          data-positioned={pos ? "" : undefined}
          style={{
            ...(pos ? ({ "--ai-x": `${pos.x}px`, "--ai-y": `${pos.y}px` } as CSSProperties) : null),
            ...(open ? null : { display: "none" }),
          }}
          className="ai-widget"
          data-ai-root=""
          onPointerDown={onPanelPointerDown}
          onDoubleClick={onPanelDoubleClick}
          onKeyDown={(event) => {
            if (event.key === "Escape" && mode === "float" && !event.nativeEvent.isComposing) close();
          }}
        >
          {mode === "dock" && (
            <div
              role="separator"
              aria-orientation="vertical"
              aria-label={t.widget.resize}
              aria-valuenow={dockWidth}
              tabIndex={0}
              title={t.widget.resize}
              className="ai-dock-resize"
              onPointerDown={onResizePointerDown}
              onKeyDown={onResizeKeyDown}
              onDoubleClick={() => resizeDock(DOCK_WIDTH_DEFAULT)}
            />
          )}
          <Suspense
            fallback={
              <div role="status" aria-label={t.loading} className="flex flex-1 items-center justify-center text-muted">
                <Spinner size={18} />
              </div>
            }
          >
            <WidgetPanel lang={lang} mode={mode} onMode={changeMode} onClose={close} />
          </Suspense>
        </div>
      )}
    </>
  );
}

/**
 * 入口显隐门：是否挂出悬浮助手由 FluxCloud 的判定决定（`<html data-ai-visible>`，来源见 `lib/account/ai-entry.ts`）。
 * 服务端渲染与水合首遍一律不渲染，水合后按标记挂载 / 卸载；不可见时撤掉首帧预设的停靠让位，页面不留空。
 */
export default function AssistantWidget({ lang }: { lang: Lang }) {
  const visible = useSyncExternalStore(subscribeAiEntry, isAiEntryVisible, () => false);

  // 以标记本身为准而不是上面的快照（水合首遍的快照恒为 false，不能据此撤掉已放行的预设）
  useEffect(() => {
    const release = () => {
      if (!isAiEntryVisible()) applyDock(false);
    };
    release();
    return subscribeAiEntry(release);
  }, []);

  return visible ? <Widget lang={lang} /> : null;
}
