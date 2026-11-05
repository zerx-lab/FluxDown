/**
 * 视频浮钮（每帧一个实例，由 sensor.content.ts 创建）。
 *
 * 目标元素只来自本帧已跟踪的 video 元素（矩形命中 / 正在播放），候选只取绑定到该元素的行
 * （getElementCandidates），不再回退到「整页所有媒体」（#336）；显示期间每帧读取元素矩形，
 * 滚动时按钮跟随（#275）；全屏时把 UI 重挂到全屏元素内。
 */
import { render } from "preact";
import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import type { JSX } from "preact";
import type { ContentScriptContext } from "wxt/utils/content-script-context";
import { createShadowRootUi, type ShadowRootContentScriptUi } from "wxt/utils/content-script-ui/shadow-root";
import type { CandidateRow } from "@/background/protocol";
import type { PageConfig } from "@/page/bridge";
import { isMediaTag, minTargetSize, type MediaTracker, type TrackedMedia } from "@/page/media-tracker";
import { loadSiteProfiles, profileFor, saveSiteProfile } from "@/utils/site-profiles";
import { contentCss } from "@/ui/content-css";
import { Icon } from "@/ui/components/Icon";
import { ToastHost, showToast } from "@/ui/components/Toast";
import { useT } from "@/ui/i18n";
import { useThemeAttr } from "@/ui/theme";
import { initI18n } from "@/utils/i18n";
import { clamp, placePopover, type PopRect } from "@/utils/popover-position";
import { sendAction } from "@/utils/messaging";
import { QualityPicker } from "./QualityPicker";
import { variantLabel } from "./labels";
import "@/ui/theme/tokens.css";
import "@/ui/theme/base.css";
import "./float.css";
import { logWarn } from "@/utils/error-log";

export interface FloatButtonController {
  /** 配置变化：决定是否启用、触发方式。 */
  setConfig(config: PageConfig | null): void;
  destroy(): void;
}

export interface FloatButtonDeps {
  ctx: ContentScriptContext;
  tracker: MediaTracker;
  /** 规则匹配用的页面 URL（下载直链时作 referrer 的是 location.href）。 */
  pageUrl(): string;
}

const HIDE_DELAY_MS = 400;
const BTN_MARGIN = 8;
const CANDIDATE_POLL_MS = 2000;
const FLASH_MS = 1500;
/** 触屏（主指针为粗指针）没有 hover：浮钮改为常驻 + 点按视频显示。 */
const COARSE_QUERY = "(pointer: coarse)";
/** 触屏点按视频后浮钮保持显示的时长（无 hover 可依，比指针的 400ms 长得多）。 */
const TOUCH_HIDE_MS = 4000;
const POP_MARGIN = 8;

function fullscreenElementOf(): Element | null {
  const doc = document as Document & { webkitFullscreenElement?: Element | null };
  return document.fullscreenElement ?? doc.webkitFullscreenElement ?? null;
}

function httpSrcOf(el: HTMLMediaElement): string {
  const src = el.currentSrc || "";
  return /^https?:/i.test(src) ? src : "";
}

/** 弹层高度上限（与 float.css 一致）；触屏另限视口一半（横屏矮视口）。 */
const POP_MAX_HEIGHT = 280;
const POP_GAP = 6;

/** 视口可用尺寸（排除滚动条）；怪异模式下 documentElement 的 client 尺寸不是视口，退回 inner*。 */
function viewportSize(): { width: number; height: number } {
  const de = document.documentElement;
  const standards = document.compatMode !== "BackCompat";
  return {
    width: standards && de.clientWidth > 0 ? de.clientWidth : window.innerWidth,
    height: standards && de.clientHeight > 0 ? de.clientHeight : window.innerHeight,
  };
}

/**
 * 把弹层摆进视口。弹层是 `.fd-float-wrap`（position:fixed、左上角在 translate 的视口坐标）的绝对定位子元素，
 * 所以视口坐标减去按钮左上角即相对偏移。内容超出 height 时由弹层自身滚动。
 */
function positionPop(pop: HTMLElement, anchor: PopRect, align: "start" | "end", coarse: boolean): void {
  const viewport = viewportSize();
  const borders = pop.offsetHeight - pop.clientHeight;
  const p = placePopover({
    anchor,
    size: { width: pop.offsetWidth, height: pop.scrollHeight + borders },
    viewport,
    align,
    prefer: "below",
    gap: POP_GAP,
    margin: POP_MARGIN,
    maxHeight: coarse ? Math.min(POP_MAX_HEIGHT, viewport.height / 2) : POP_MAX_HEIGHT,
  });
  pop.style.right = "auto";
  pop.style.bottom = "auto";
  pop.style.left = `${Math.round(p.left - anchor.left)}px`;
  pop.style.top = `${Math.round(p.top - anchor.top)}px`;
  pop.style.maxWidth = `${Math.floor(p.maxWidth)}px`;
  pop.style.maxHeight = `${Math.floor(p.height)}px`;
}

interface FloatRootProps {
  tracker: MediaTracker;
  config: PageConfig;
  host: HTMLElement;
}

function FloatRoot(props: FloatRootProps): JSX.Element {
  const { tracker, config, host } = props;
  const t = useT();
  useThemeAttr(host);

  const wrapRef = useRef<HTMLDivElement>(null);
  const [target, setTarget] = useState<TrackedMedia | null>(null);
  const [rows, setRows] = useState<CandidateRow[]>([]);
  const [menuOpen, setMenuOpen] = useState(false);
  const [state, setState] = useState<"idle" | "busy" | "done" | "error">("idle");
  const [, setTick] = useState(0);
  /** 用户在菜单里选了「本页隐藏」/「本站不再显示」。 */
  const [hidden, setHidden] = useState(false);

  const menuOpenRef = useRef(false);
  menuOpenRef.current = menuOpen;
  const hoverRef = useRef(false);
  const evaluateRef = useRef<(() => void) | null>(null);
  const rowsRef = useRef<CandidateRow[]>([]);
  rowsRef.current = rows;
  const popRef = useRef<HTMLDivElement>(null);
  const [coarse, setCoarse] = useState(() => typeof matchMedia === "function" && matchMedia(COARSE_QUERY).matches);
  useEffect(() => {
    if (typeof matchMedia !== "function") return;
    const mq = matchMedia(COARSE_QUERY);
    const onChange = (): void => setCoarse(mq.matches);
    mq.addEventListener("change", onChange);
    onChange();
    return () => mq.removeEventListener("change", onChange);
  }, []);
  const trigger = config.floatTrigger;

  // ===== 目标元素选择 =====
  useEffect(() => {
    let raf = 0;
    let hideTimer: number | undefined;
    let tapTimer: number | undefined;
    let x = -1;
    let y = -1;
    let current: TrackedMedia | null = null;
    let pending = false;
    /** 触屏：最近一次点按到的媒体元素及其显示截止时间。 */
    let tappedKey: string | null = null;
    let tappedUntil = 0;

    const choose = (next: TrackedMedia | null): void => {
      if ((next?.key ?? null) === (current?.key ?? null)) return;
      current = next;
      setTarget(next);
      if (!next) setMenuOpen(false);
    };

    /** 尺寸达标且未被 page 规则排除。 */
    const usable = (item: TrackedMedia): boolean => {
      const r = item.el.getBoundingClientRect();
      const min = minTargetSize(item.tag);
      return r.width >= min.w && r.height >= min.h && !tracker.isFloatExcluded(item.el);
    };

    const evaluate = (): void => {
      if (current && !tracker.get(current.key)) {
        choose(null);
        return;
      }
      if (trigger === "always" || coarse) {
        if (coarse && tappedKey !== null && Date.now() < tappedUntil) {
          const tapped = tracker.get(tappedKey);
          if (tapped && usable(tapped)) {
            choose(tapped);
            return;
          }
        }
        const active = tracker.activePlaying();
        const ok = active && usable(active);
        // 触屏：菜单展开期间不因点按窗口到期 / 暂停播放而收起
        if (coarse && !ok && current && menuOpenRef.current) return;
        choose(ok ? active : null);
        return;
      }
      const raw = tracker.elementAtPoint(x, y);
      // page 规则排除的背景 / 广告视频视为未命中
      const hit = raw && !tracker.isFloatExcluded(raw.el) ? raw : null;
      if (hit) {
        window.clearTimeout(hideTimer);
        hideTimer = undefined;
        choose(hit);
      } else if (current && hideTimer === undefined && !hoverRef.current && !menuOpenRef.current) {
        hideTimer = window.setTimeout(() => {
          hideTimer = undefined;
          if (!hoverRef.current && !menuOpenRef.current) choose(null);
        }, HIDE_DELAY_MS);
      }
    };

    const schedule = (): void => {
      if (pending) return;
      pending = true;
      raf = requestAnimationFrame(() => {
        pending = false;
        evaluate();
      });
    };

    const onMove = (e: PointerEvent): void => {
      x = e.clientX;
      y = e.clientY;
      schedule();
    };
    const onLeave = (): void => {
      x = -1;
      y = -1;
      schedule();
    };
    // 触屏点按：用 pointerup（滚动会触发 pointercancel，不会误显示）；捕获 + passive，不干扰页面自己的点按处理
    const onTap = (e: PointerEvent): void => {
      const hit = tracker.fromEvent(e);
      if (!hit) return;
      tappedKey = hit.key;
      tappedUntil = Date.now() + TOUCH_HIDE_MS;
      window.clearTimeout(tapTimer);
      tapTimer = window.setTimeout(schedule, TOUCH_HIDE_MS + 50);
      schedule();
    };

    if (coarse) {
      document.addEventListener("pointerup", onTap, { capture: true, passive: true });
    } else if (trigger === "hover") {
      document.addEventListener("pointermove", onMove, { capture: true, passive: true });
      document.documentElement.addEventListener("pointerleave", onLeave, { passive: true });
    }
    const unsubscribe = tracker.subscribe(schedule);
    // 滚动 / 布局变化时指针不动：位置循环每帧调用 evaluateRef 重新评估
    evaluateRef.current = evaluate;
    schedule();

    return () => {
      document.removeEventListener("pointerup", onTap, { capture: true });
      document.removeEventListener("pointermove", onMove, { capture: true });
      document.documentElement.removeEventListener("pointerleave", onLeave);
      unsubscribe();
      cancelAnimationFrame(raf);
      window.clearTimeout(hideTimer);
      window.clearTimeout(tapTimer);
      evaluateRef.current = null;
    };
  }, [tracker, trigger, coarse]);

  // ===== 候选（只取绑定到目标元素的行） =====
  // 按元素缓存上次结果：指针移出再移入同一播放器时立即显示，不必等一次后台往返（MV3
  // service worker 休眠后唤醒可能要数百毫秒）；随后照常刷新。
  const rowsCacheRef = useRef(new Map<string, CandidateRow[]>());
  const targetKey = target?.key ?? null;
  useEffect(() => {
    setRows(targetKey ? rowsCacheRef.current.get(targetKey) ?? [] : []);
    if (!targetKey) return;
    let alive = true;
    let debounce: number | undefined;
    const load = async (): Promise<void> => {
      try {
        const res = await sendAction({ action: "getElementCandidates", elementKey: targetKey });
        if (!alive) return;
        const next = res?.rows ?? [];
        rowsCacheRef.current.set(targetKey, next);
        setRows(next);
      } catch {
        /* 扩展失效 / 后台唤醒中：下一轮重试 */
      }
    };
    // 绑定晚于播放到达：元素状态变化后重取；无行时周期轮询
    const unsubscribe = tracker.subscribe(() => {
      setTick((n) => n + 1);
      window.clearTimeout(debounce);
      debounce = window.setTimeout(() => void load(), 700);
    });
    const poll = window.setInterval(() => {
      if (rowsRef.current.length === 0) void load();
    }, CANDIDATE_POLL_MS);
    return () => {
      alive = false;
      unsubscribe();
      window.clearInterval(poll);
      window.clearTimeout(debounce);
    };
  }, [tracker, targetKey]);

  // ===== 显示判定 =====
  const el = target?.el ?? null;
  const directSrc = rows.length === 0 && el ? httpSrcOf(el) : "";
  const videoFullscreen = !!el && fullscreenElementOf() === el;
  const showBtn = !!target && !hidden && (rows.length > 0 || directSrc !== "") && !videoFullscreen;
  const position = config.floatPosition;

  // ===== 位置跟随：每帧读取元素矩形 =====
  useEffect(() => {
    if (!el || !showBtn) return;
    let raf = 0;
    const loop = (): void => {
      const wrap = wrapRef.current;
      if (wrap) {
        const r = el.getBoundingClientRect();
        const vw = window.innerWidth;
        const vh = window.innerHeight;
        const offscreen =
          !el.isConnected || r.bottom <= 0 || r.top >= vh || r.right <= 0 || r.left >= vw || r.width < 40 || r.height < 40;
        if (offscreen || fullscreenElementOf() === el) {
          wrap.style.visibility = "hidden";
        } else {
          const bw = wrap.offsetWidth || 80;
          const bh = wrap.offsetHeight || 28;
          const atRight = position === "top-right" || position === "bottom-right";
          const atTop = position === "top-right" || position === "top-left";
          const x0 = atRight ? r.right - bw - BTN_MARGIN : r.left + BTN_MARGIN;
          // 音频控件条很矮：按钮放到控件条上方，不遮挡播放控件
          const y0 = target?.tag === "audio"
            ? r.top - bh - 4
            : atTop ? r.top + BTN_MARGIN : r.bottom - bh - BTN_MARGIN;
          const left = clamp(x0, 4, vw - bw - 4);
          const top = clamp(y0, 4, vh - bh - 4);
          wrap.style.transform = `translate(${Math.round(left)}px, ${Math.round(top)}px)`;
          wrap.style.visibility = "visible";
          // 弹层：与按钮对齐，空间不够时翻转 / 夹进视口（打开瞬间由 layout effect 同步一次）
          if (popRef.current) {
            positionPop(popRef.current, { left: Math.round(left), top: Math.round(top), width: bw, height: bh }, atRight ? "end" : "start", coarse);
          }
        }
      }
      evaluateRef.current?.();
      raf = requestAnimationFrame(loop);
    };
    raf = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(raf);
  }, [el, showBtn, position, coarse]);

  // 弹层刚挂载时（下一帧位置循环之前）同步摆好，避免先以默认位置闪一帧
  useLayoutEffect(() => {
    const wrap = wrapRef.current;
    const pop = popRef.current;
    if (!menuOpen || !wrap || !pop) return;
    const r = wrap.getBoundingClientRect();
    const atRight = position === "top-right" || position === "bottom-right";
    positionPop(pop, { left: r.left, top: r.top, width: r.width, height: r.height }, atRight ? "end" : "start", coarse);
  }, [menuOpen, position, coarse]);

  // ===== 弹层：点击外部 / Esc 关闭 =====
  useEffect(() => {
    if (!menuOpen) return;
    const onDown = (e: Event): void => {
      const wrap = wrapRef.current;
      if (wrap && !e.composedPath().includes(wrap)) setMenuOpen(false);
    };
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") setMenuOpen(false);
    };
    document.addEventListener("pointerdown", onDown, true);
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("pointerdown", onDown, true);
      document.removeEventListener("keydown", onKey, true);
    };
  }, [menuOpen]);

  if (!showBtn || !target) return <ToastHost />;

  // 受保护（DRM 加密）或只能在浏览器内取得（SW 合成）的内容：按钮只说明原因，不提供下载。
  const blocked = rows.length > 0 && rows.every((r) => r.candidate.protected === true || r.candidate.browserOnly === true);
  const anyProtected = rows.some((r) => r.candidate.protected === true);

  // ===== 标签：有分辨率的播放档 > 元素实际高度 > 播放档（自动 / 原始）> 默认文案 =====
  const playingRow = rows.find((r) => r.playingVariantId);
  const playingVariant = playingRow?.candidate.variants.find((v) => v.id === playingRow.playingVariantId);
  const height = target.tag === "video" ? (target.el as HTMLVideoElement).videoHeight : 0;
  // 「自动」「原始」不说明画质；此时元素正在播放的实际高度更有用。
  const genericVariant = !playingVariant || playingVariant.label === "auto" || playingVariant.label === "original";
  // 档位标签走与清晰度菜单相同的本地化（auto / original 等内部值不直接外露）
  const label = blocked
    ? t(anyProtected ? "float.protected" : "float.browserOnly")
    : !genericVariant && playingVariant
      ? variantLabel(t, playingVariant)
      : height > 0
        ? `${height}P`
        : playingVariant
          ? variantLabel(t, playingVariant)
          : target.tag === "audio"
            ? t("float.downloadAudio")
            : t("float.download");
  const titleText = blocked
    ? t(anyProtected ? "float.protectedHint" : "float.browserOnlyHint")
    : [playingRow?.candidate.title ?? rows[0]?.candidate.title, label].filter(Boolean).join(" · ");

  const flash = (kind: "done" | "error", message?: string): void => {
    setState(kind);
    if (kind === "error") showToast(message || t("float.failed"), "error");
    window.setTimeout(() => setState("idle"), FLASH_MS);
  };

  const send = async (run: () => Promise<{ success: boolean; message?: string } | undefined>): Promise<void> => {
    setState("busy");
    try {
      const res = await run();
      if (res?.success) flash("done");
      else flash("error", res?.message);
    } catch (e) {
      flash("error", e instanceof Error ? e.message : undefined);
    }
  };

  const onClick = (): void => {
    if (state === "busy") return;
    const only = rows.length === 1 ? rows[0] : null;
    if (!blocked && only && only.candidate.variants.length === 1 && only.candidate.downloadable) {
      void send(() =>
        sendAction({
          action: "downloadCandidate",
          candidateId: only.candidate.id,
          variantId: only.candidate.variants[0].id,
        }),
      );
      return;
    }
    if (rows.length === 0 && directSrc) {
      void send(() => sendAction({ action: "downloadUrls", urls: [directSrc], referrer: location.href }));
      return;
    }
    setMenuOpen((open) => !open);
  };

  /** 当前要复制的链接：正在播放的档，否则第一个可下载档；音视频分离时两条都复制。 */
  const copyLink = async (): Promise<void> => {
    const row = playingRow ?? rows.find((r) => r.candidate.downloadable);
    const variant = playingVariant ?? row?.candidate.variants[0];
    const text = variant ? [variant.videoUrl, variant.audioUrl].filter(Boolean).join("\n") : directSrc;
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      showToast(t("float.linkCopied"));
    } catch {
      showToast(t("float.copyFailed"), "error");
    }
  };

  /** 本站不再显示浮钮：写站点配置（父域已有配置时就地更新），后台随之下发新的页面配置。 */
  const disableForSite = async (): Promise<void> => {
    try {
      const profiles = await loadSiteProfiles();
      const resolved = profileFor(profiles, location.hostname);
      await saveSiteProfile(resolved.key ?? location.hostname, {
        ...resolved.profile,
        floatButton: false,
        updatedAt: Date.now(),
      });
      setHidden(true);
      showToast(t("float.disabledForSite"));
    } catch {
      showToast(t("site.saveFailed"), "error");
    }
  };

  const stop = (e: Event): void => e.stopPropagation();

  return (
    <>
      <div
        class="fd-float-wrap"
        ref={wrapRef}
        onPointerEnter={() => (hoverRef.current = true)}
        onPointerLeave={() => (hoverRef.current = false)}
        onPointerDown={stop}
        onMouseDown={stop}
        onMouseUp={stop}
        onDblClick={stop}
        onContextMenu={stop}
      >
        <button
          type="button"
          class="fd-float-btn"
          data-state={state}
          title={titleText}
          onClick={(e) => {
            e.stopPropagation();
            onClick();
          }}
        >
          <Icon name={state === "done" ? "check" : "download"} size={14} />
          <span class="fd-float-label">{state === "done" ? t("float.done") : label}</span>
        </button>
        {menuOpen ? (
          <div ref={popRef} class="fd-float-pop" onClick={stop}>
            <QualityPicker
              rows={rows}
              onDone={() => {
                setMenuOpen(false);
                flash("done");
              }}
            >
              <button
                type="button"
                class="fd-qp-extra"
                onClick={() => {
                  setMenuOpen(false);
                  void copyLink();
                }}
              >
                {t("float.copyLink")}
              </button>
              <button
                type="button"
                class="fd-qp-extra"
                onClick={() => {
                  setMenuOpen(false);
                  void sendAction({ action: "openPanel" }).catch(() => {});
                }}
              >
                {t("float.openPanel")}
              </button>
              <button
                type="button"
                class="fd-qp-extra"
                onClick={() => {
                  setMenuOpen(false);
                  setHidden(true);
                }}
              >
                {t("float.hideOnPage")}
              </button>
              <button
                type="button"
                class="fd-qp-extra"
                onClick={() => {
                  setMenuOpen(false);
                  void disableForSite();
                }}
              >
                {t("float.disableForSite")}
              </button>
            </QualityPicker>
          </div>
        ) : null}
      </div>
      <ToastHost />
    </>
  );
}

/** 创建本帧浮钮控制器：首次出现可用的 video 元素时才真正挂载 Shadow DOM。 */
export function createFloatButton(deps: FloatButtonDeps): FloatButtonController {
  const { ctx, tracker } = deps;
  let config: PageConfig | null = null;
  let ui: ShadowRootContentScriptUi<HTMLElement> | null = null;
  let creating = false;
  let destroyed = false;
  let mountedContainer: HTMLElement | null = null;
  let mountedHost: HTMLElement | null = null;

  const wanted = (): boolean => !destroyed && config !== null && config.floatButton && config.sniff;
  const hasMedia = (): boolean => tracker.list().length > 0;

  /** 全屏元素包含目标视频（且不是视频本身）时，挂到全屏元素里才可见。 */
  function hostAnchor(): HTMLElement {
    const fs = fullscreenElementOf();
    if (fs && !isMediaTag(fs) && tracker.list().some((i) => fs.contains(i.el))) return fs as HTMLElement;
    return document.documentElement;
  }

  function draw(): void {
    if (mountedContainer && mountedHost && config) {
      render(<FloatRoot tracker={tracker} config={config} host={mountedHost} />, mountedContainer);
    }
  }

  async function ensure(): Promise<void> {
    if (ui || creating || !wanted() || !hasMedia()) return;
    creating = true;
    try {
      await initI18n();
      const created = await createShadowRootUi<HTMLElement>(ctx, {
        name: "fluxdown-float",
        css: contentCss(),
        position: "inline",
        anchor: () => hostAnchor(),
        append: "last",
        onMount(container, _shadow, host) {
          // 不能给宿主写定位样式：WXT 注入的 `:host{all:initial !important}` 属于 shadow 内层上下文，
          // 在 !important 级别压过宿主的内联样式（宿主恒为 static / z-index:auto）。
          // 定位与层级全部放在 shadow 内的 .fd-float-wrap（position:fixed + 最大 z-index）。
          mountedContainer = container;
          mountedHost = host;
          draw();
          return container;
        },
        onRemove(container) {
          if (container) render(null, container);
          mountedContainer = null;
          mountedHost = null;
        },
      });
      if (!wanted()) return;
      ui = created;
      ui.mount();
    } catch (e) {
      logWarn("[FluxDown] float button mount failed:", e);
    } finally {
      creating = false;
    }
  }

  function teardown(): void {
    if (!ui) return;
    ui.remove();
    ui = null;
  }

  const unsubscribe = tracker.subscribe(() => {
    if (!ui) void ensure();
  });

  const onFullscreenChange = (): void => {
    if (!ui) return;
    ui.remove();
    ui.mount();
  };
  document.addEventListener("fullscreenchange", onFullscreenChange);
  document.addEventListener("webkitfullscreenchange", onFullscreenChange);

  return {
    setConfig(next) {
      config = next;
      if (wanted()) {
        void ensure();
        draw();
      } else {
        teardown();
      }
    },
    destroy() {
      destroyed = true;
      unsubscribe();
      document.removeEventListener("fullscreenchange", onFullscreenChange);
      document.removeEventListener("webkitfullscreenchange", onFullscreenChange);
      teardown();
    },
  };
}
