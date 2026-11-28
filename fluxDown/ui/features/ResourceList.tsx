/**
 * 资源列表：按 TabView 分组渲染「当前播放 / 页面其他视频 / 未关联的媒体 / 文件」。
 * popup 摘要、页内面板（compact）与侧边栏（full，带搜索与排序）共用。
 */
import type { JSX } from "preact";
import { useEffect, useMemo, useRef, useState } from "preact/hooks";
import type { CandidateRow, ResourceRow, SendResult, TabView } from "@/background/protocol";
import type { ResourceType } from "@/utils/resource-types";
import { sendAction } from "@/utils/messaging";
import { Badge } from "@/ui/components/Badge";
import { Button } from "@/ui/components/Button";
import { EmptyState } from "@/ui/components/EmptyState";
import { Icon } from "@/ui/components/Icon";
import { IconButton } from "@/ui/components/IconButton";
import { Tabs } from "@/ui/components/Tabs";
import { showToast } from "@/ui/components/Toast";
import { formatBytes } from "@/ui/format";
import { useT } from "@/ui/i18n";
import { isPreviewable, PreviewModal, type PreviewTarget } from "./PreviewModal";
import { QualityPicker } from "./QualityPicker";
import { resourceDisplayName, variantLabel } from "./labels";
import "./features.css";

export interface ListSelection {
  candidateIds: string[];
  resourceIds: string[];
}

type TypeFilter = "all" | ResourceType;
type SortKey = "time" | "size" | "name";

const FILTER_ORDER: TypeFilter[] = [
  "all",
  "video",
  "stream",
  "audio",
  "subtitle",
  "image",
  "document",
  "archive",
  "executable",
  "torrent",
  "magnet",
  "other",
];

const candKey = (id: string) => `c:${id}`;
const resKey = (id: string) => `r:${id}`;

function shownVariant(row: CandidateRow) {
  const { candidate } = row;
  return (
    candidate.variants.find((v) => v.id === row.playingVariantId) ?? candidate.variants[0] ?? null
  );
}

function allRows(view: TabView): CandidateRow[] {
  return [...view.playing, ...view.otherElements, ...view.unbound];
}

/** 一次提交所选资源；候选保留用户看到的播放中/首个清晰度。 */
export function downloadSelection(view: TabView, sel: ListSelection): Promise<SendResult> {
  const byId = new Map(allRows(view).map((r) => [r.candidate.id, r]));
  return sendAction<"downloadSelection">({
    action: "downloadSelection",
    tabId: view.tabId,
    candidates: sel.candidateIds.map((candidateId) => {
      const row = byId.get(candidateId);
      return { candidateId, variantId: row ? shownVariant(row)?.id : undefined };
    }),
    resourceIds: sel.resourceIds,
  });
}

interface Props {
  view: TabView;
  density: "compact" | "full";
  showSearch: boolean;
  onSelectionChange?: (sel: ListSelection) => void;
  /** 内置「已选 N · 下载」栏；侧边栏自己渲染批量栏时传 false。默认 true。 */
  batchBar?: boolean;
}

export function ResourceList(props: Props): JSX.Element {
  const { view, density } = props;
  const t = useT();
  const rootRef = useRef<HTMLDivElement>(null);
  const [filter, setFilter] = useState<TypeFilter>("all");
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<SortKey>("time");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [unboundOpen, setUnboundOpen] = useState(false);
  const [menuKey, setMenuKey] = useState<string | null>(null);
  const [pickerKey, setPickerKey] = useState<string | null>(null);
  const [preview, setPreview] = useState<PreviewTarget | null>(null);
  const [busy, setBusy] = useState(false);

  // 点击列表外 / Esc 关闭菜单（Shadow DOM 下用 composedPath 判断）
  useEffect(() => {
    if (menuKey === null) return;
    const onDown = (e: Event) => {
      const el = rootRef.current;
      if (el && !e.composedPath().some((n) => n instanceof Element && n.hasAttribute("data-menu"))) {
        setMenuKey(null);
      }
    };
    document.addEventListener("pointerdown", onDown, true);
    return () => document.removeEventListener("pointerdown", onDown, true);
  }, [menuKey]);

  // 视图更新后丢弃已不存在的选择项
  useEffect(() => {
    setSelected((prev) => {
      if (prev.size === 0) return prev;
      const alive = new Set<string>();
      for (const r of allRows(view)) alive.add(candKey(r.candidate.id));
      for (const f of view.files) alive.add(resKey(f.resource.id));
      const next = new Set([...prev].filter((k) => alive.has(k)));
      return next.size === prev.size ? prev : next;
    });
  }, [view]);

  const selection = useMemo<ListSelection>(() => {
    const out: ListSelection = { candidateIds: [], resourceIds: [] };
    for (const k of selected) {
      if (k.startsWith("c:")) out.candidateIds.push(k.slice(2));
      else out.resourceIds.push(k.slice(2));
    }
    return out;
  }, [selected]);

  useEffect(() => {
    props.onSelectionChange?.(selection);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selection]);

  const q = query.trim().toLowerCase();
  const matchesCandidate = (row: CandidateRow): boolean => {
    if (filter !== "all" && row.candidate.type !== filter) return false;
    if (!q) return true;
    return (
      row.candidate.title.toLowerCase().includes(q) ||
      row.candidate.variants.some((v) => v.videoUrl.toLowerCase().includes(q))
    );
  };
  const matchesFile = (row: ResourceRow): boolean => {
    const r = row.resource;
    if (filter !== "all" && r.type !== filter) return false;
    if (!q) return true;
    return r.filename.toLowerCase().includes(q) || r.url.toLowerCase().includes(q);
  };

  const files = useMemo(() => {
    const list = view.files.filter(matchesFile);
    if (density === "full") {
      if (sort === "size") list.sort((a, b) => b.resource.size - a.resource.size);
      else if (sort === "name") list.sort((a, b) => a.resource.filename.localeCompare(b.resource.filename));
      else list.sort((a, b) => b.resource.detectedAt - a.resource.detectedAt);
    }
    return list;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [view.files, filter, q, sort, density]);

  const playing = view.playing.filter(matchesCandidate);
  const others = view.otherElements.filter(matchesCandidate);
  const unbound = view.unbound.filter(matchesCandidate);

  const toggle = (key: string, on: boolean) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (on) next.add(key);
      else next.delete(key);
      return next;
    });

  const visibleKeys = [
    ...[...playing, ...others, ...unbound]
      .filter((r) => r.candidate.downloadable && !r.candidate.protected && !r.candidate.browserOnly)
      .map((r) => candKey(r.candidate.id)),
    ...files.map((f) => resKey(f.resource.id)),
  ];
  const allChecked = visibleKeys.length > 0 && visibleKeys.every((k) => selected.has(k));

  const report = (res: SendResult | undefined) => {
    if (res?.cancelled) showToast(t("remote.manualCancelled"));
    else if (res?.success) showToast(t("list.sent"), "success");
    else showToast(res?.message || t("list.sendFailed"), "error");
  };

  const downloadRow = async (row: CandidateRow) => {
    const v = shownVariant(row);
    setBusy(true);
    try {
      report(
        await sendAction<"downloadCandidate">({
          action: "downloadCandidate",
          tabId: view.tabId,
          candidateId: row.candidate.id,
          variantId: v?.id,
        }),
      );
    } catch {
      report(undefined);
    } finally {
      setBusy(false);
    }
  };

  const downloadFile = async (row: ResourceRow) => {
    setBusy(true);
    try {
      report(
        await sendAction<"downloadResources">({
          action: "downloadResources",
          tabId: view.tabId,
          resourceIds: [row.resource.id],
        }),
      );
    } catch {
      report(undefined);
    } finally {
      setBusy(false);
    }
  };

  const copy = async (url: string) => {
    setMenuKey(null);
    try {
      await navigator.clipboard.writeText(url);
      showToast(t("list.copied"), "success");
    } catch {
      showToast(t("list.copyFailed"), "error");
    }
  };

  const batchDownload = async () => {
    setBusy(true);
    try {
      const res = await downloadSelection(view, selection);
      report(res);
      if (res.success) setSelected(new Set());
    } catch {
      report(undefined);
    } finally {
      setBusy(false);
    }
  };

  const subtitleLangs = useMemo(() => {
    const langs = new Set<string>();
    for (const f of view.files) {
      if (f.resource.type === "subtitle") langs.add(f.resource.lang || f.resource.label || "");
    }
    langs.delete("");
    return [...langs];
  }, [view.files]);

  const renderCandidate = (row: CandidateRow, withSubs: boolean): JSX.Element => {
    const c = row.candidate;
    const v = shownVariant(row);
    const key = candKey(c.id);
    const metaParts: string[] = [];
    if (v) {
      metaParts.push(variantLabel(t, v));
      if (v.codec) metaParts.push(v.codec);
      if (v.audioUrl) metaParts.push(t("float.audioVideo"));
      if (v.fileSize && v.fileSize > 0) metaParts.push(formatBytes(v.fileSize));
    }
    if (c.variants.length > 1) metaParts.push(t("list.variantCount", { count: String(c.variants.length) }));
    if (withSubs && subtitleLangs.length) metaParts.push(`${t("list.subtitles")} ${subtitleLangs.join("/")}`);
    const isPlaying = row.playingVariantId !== null && row.binding !== "none";
    const unavailable = !c.downloadable || c.protected === true || c.browserOnly === true;
    const metaText = c.protected
      ? t("float.protectedHint")
      : c.browserOnly
        ? t("float.browserOnlyHint")
        : c.downloadable
          ? metaParts.join(" · ")
          : t("panel.videoNeedsManifest");
    return (
      <div class="fd-row-wrap" key={key}>
        <div class={`fd-row${selected.has(key) ? " fd-selected" : ""}`}>
          <input
            type="checkbox"
            class="fd-check"
            checked={selected.has(key)}
            disabled={unavailable}
            onChange={(e) => toggle(key, (e.currentTarget as HTMLInputElement).checked)}
          />
          <Icon kind="video" size={18} />
          <div class="fd-row-main">
            <div class="fd-row-title" title={c.title}>
              {c.title}
            </div>
            <div class="fd-row-meta" title={metaText}>
              {metaText}
            </div>
          </div>
          {isPlaying ? <Badge tone="accent">{t("list.playing")}</Badge> : null}
          {v && isPreviewable(c.type) ? (
            <IconButton
              icon="play"
              label={t("list.preview")}
              onClick={() => setPreview({ url: v.videoUrl, title: c.title, type: c.type, mimeType: v.mimeType })}
            />
          ) : null}
          <IconButton
            icon="download"
            label={t("panel.download")}
            disabled={busy || unavailable}
            onClick={() => void downloadRow(row)}
          />
          <div class="fd-menu-anchor" data-menu>
            <IconButton icon="more" label={t("list.more")} onClick={() => setMenuKey(menuKey === key ? null : key)} />
            {menuKey === key ? (
              <div class="fd-menu" data-menu>
                <button type="button" onClick={() => v && void copy(v.videoUrl)} disabled={!v}>
                  <Icon name="copy" size={13} /> {t("list.copyLink")}
                </button>
                {c.variants.length > 1 && c.downloadable ? (
                  <button
                    type="button"
                    onClick={() => {
                      setMenuKey(null);
                      setPickerKey(pickerKey === key ? null : key);
                    }}
                  >
                    <Icon name="chevronDown" size={13} /> {t("panel.qualityPickerTitle")}
                  </button>
                ) : null}
              </div>
            ) : null}
          </div>
        </div>
        {pickerKey === key ? (
          <div class="fd-row-picker">
            <QualityPicker rows={[row]} tabId={view.tabId} onDone={() => setPickerKey(null)} />
          </div>
        ) : null}
      </div>
    );
  };

  const renderFile = (row: ResourceRow): JSX.Element => {
    const r = row.resource;
    const key = resKey(r.id);
    const name = resourceDisplayName(r, t("panel.tabOther"));
    const meta: string[] = [t(`list.type.${r.type}`)];
    if (r.lang || r.label) meta.push(r.label || r.lang || "");
    if (r.size > 0) meta.push(formatBytes(r.size));
    return (
      <div class="fd-row-wrap" key={key}>
        <div class={`fd-row${selected.has(key) ? " fd-selected" : ""}`}>
          <input
            type="checkbox"
            class="fd-check"
            checked={selected.has(key)}
            onChange={(e) => toggle(key, (e.currentTarget as HTMLInputElement).checked)}
          />
          <Icon resourceType={r.type} size={18} />
          <div class="fd-row-main">
            <div class="fd-row-title" title={`${name}\n${r.url}`}>
              {name}
            </div>
            <div class="fd-row-meta" title={meta.join(" · ")}>{meta.join(" · ")}</div>
          </div>
          {r.confidence === "low" ? <Badge tone="warning">{t("list.lowConfidence")}</Badge> : null}
          {isPreviewable(r.type) ? (
            <IconButton
              icon="play"
              label={t("list.preview")}
              onClick={() => setPreview({ url: r.url, title: name, type: r.type, mimeType: r.mimeType })}
            />
          ) : null}
          <IconButton
            icon="download"
            label={t("panel.download")}
            disabled={busy}
            onClick={() => void downloadFile(row)}
          />
          <div class="fd-menu-anchor" data-menu>
            <IconButton icon="more" label={t("list.more")} onClick={() => setMenuKey(menuKey === key ? null : key)} />
            {menuKey === key ? (
              <div class="fd-menu" data-menu>
                <button type="button" onClick={() => void copy(r.url)}>
                  <Icon name="copy" size={13} /> {t("list.copyLink")}
                </button>
              </div>
            ) : null}
          </div>
        </div>
      </div>
    );
  };

  const filterItems = FILTER_ORDER.filter((k) => k === "all" || (view.counts[k] ?? 0) > 0).map((k) => ({
    key: k,
    label: t(`list.type.${k}`),
    count: view.counts[k],
  }));

  const total = playing.length + others.length + unbound.length + files.length;
  // 页面探测状态：恢复中 / 需刷新总是提示；不可注入只在确有媒体时提示（否则只是普通受限页）
  const probeText =
    view.probe === "recovering"
      ? t("list.probe.recovering")
      : view.probe === "degraded"
        ? t("list.probe.degraded")
        : view.probe === "unavailable" && view.unbound.length > 0
          ? t("list.probe.unavailable")
          : null;
  const reloadPage = async () => {
    const res = await sendAction<"reloadTab">({ action: "reloadTab", tabId: view.tabId }).catch(() => null);
    if (!res?.ok) showToast(t("list.probe.reloadFailed"), "error");
  };

  return (
    <div class={`fd-list fd-list-${density}`} ref={rootRef}>
      {filterItems.length > 2 ? (
        <Tabs items={filterItems} active={filter} onChange={(k) => setFilter(k as TypeFilter)} />
      ) : null}

      {density === "full" ? (
        <div class="fd-list-toolbar">
          <label class="fd-check-all">
            <input
              type="checkbox"
              checked={allChecked}
              onChange={(e) => {
                const on = (e.currentTarget as HTMLInputElement).checked;
                setSelected(on ? new Set(visibleKeys) : new Set());
              }}
            />
            {t("panel.selectAll")}
          </label>
          {props.showSearch ? (
            <input
              class="fd-input fd-search"
              type="search"
              placeholder={t("list.search")}
              value={query}
              onInput={(e) => setQuery((e.currentTarget as HTMLInputElement).value)}
            />
          ) : null}
          <select
            class="fd-select"
            value={sort}
            aria-label={t("list.sort")}
            onChange={(e) => setSort((e.currentTarget as HTMLSelectElement).value as SortKey)}
          >
            <option value="time">{t("list.sortTime")}</option>
            <option value="size">{t("list.sortSize")}</option>
            <option value="name">{t("list.sortName")}</option>
          </select>
        </div>
      ) : null}

      {probeText ? (
        <div class={`fd-probe fd-probe-${view.probe}`} role="status">
          <span class="fd-probe-text">{probeText}</span>
          {view.probe === "degraded" ? (
            <Button size="sm" variant="ghost" onClick={() => void reloadPage()}>
              {t("list.probe.reload")}
            </Button>
          ) : null}
        </div>
      ) : null}

      {total === 0 ? (
        <EmptyState
          icon="search"
          title={t("popup.resources.empty")}
          hint={view.hiddenLowConfidence > 0 ? t("list.hiddenLow", { count: String(view.hiddenLowConfidence) }) : undefined}
        />
      ) : null}

      {playing.length > 0 ? (
        <section class="fd-group">
          <h4 class="fd-group-title">{t("list.group.playing")}</h4>
          {playing.map((r) => renderCandidate(r, true))}
        </section>
      ) : null}

      {others.length > 0 ? (
        <section class="fd-group">
          <h4 class="fd-group-title">{t("list.group.others")}</h4>
          {others.map((r) => renderCandidate(r, false))}
        </section>
      ) : null}

      {unbound.length > 0 ? (
        <section class="fd-group">
          <button type="button" class="fd-group-title fd-group-toggle" onClick={() => setUnboundOpen(!unboundOpen)}>
            <Icon name={unboundOpen ? "chevronDown" : "chevronRight"} size={12} />
            {t("list.group.unbound")} ({unbound.length})
          </button>
          {unboundOpen ? unbound.map((r) => renderCandidate(r, false)) : null}
        </section>
      ) : null}

      {files.length > 0 ? (
        <section class="fd-group">
          <h4 class="fd-group-title">{t("list.group.files")}</h4>
          {files.map(renderFile)}
        </section>
      ) : null}

      {props.batchBar !== false && selected.size > 0 ? (
        <div class="fd-batchbar">
          <span>{t("list.selected", { count: String(selected.size) })}</span>
          <Button size="sm" variant="ghost" onClick={() => setSelected(new Set())}>
            {t("list.clearSelection")}
          </Button>
          <Button size="sm" variant="primary" disabled={busy} onClick={() => void batchDownload()}>
            {t("panel.batchDownload")}
          </Button>
        </div>
      ) : null}

      {preview ? <PreviewModal target={preview} onClose={() => setPreview(null)} /> : null}
    </div>
  );
}
