/**
 * 更新日志时间线:按渠道分页拉取 /api/changelog,宽屏左侧为吸顶版本索引(随滚动高亮)。
 * 带 `#v1-2-3` 锚点进入时:rc 锚点自动切到预览渠道,目标不在已加载页时继续翻页直到找到。
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowUpRight, ChevronDown, Loader2 } from "lucide-react";
import type { Lang } from "@/i18n/config";
import { changelog } from "@/i18n/messages/changelog";
import { versionAnchor, type ChangelogRelease } from "@/lib/release-format";
import { GITHUB_URL } from "@/lib/site-nav";
import ReleaseEntry from "./ReleaseEntry";
import { withBase } from "@/lib/base";

type Channel = "stable" | "frontier";
const PER_PAGE = 15;
/** 锚点追页上限,避免错误锚点把全部历史拉完。 */
const MAX_HASH_PAGES = 8;

interface PageData {
  releases: ChangelogRelease[];
  has_more: boolean;
}

export default function ChangelogTimeline({ lang }: { lang: Lang }) {
  const t = changelog[lang];
  const [channel, setChannel] = useState<Channel>("stable");
  const [releases, setReleases] = useState<ChangelogRelease[]>([]);
  const [page, setPage] = useState(0);
  const [hasMore, setHasMore] = useState(false);
  const [status, setStatus] = useState<"loading" | "more" | "ready" | "error">("loading");
  const [active, setActive] = useState<string | null>(null);
  const requestRef = useRef(0);
  const hashRef = useRef<string | null>(null);
  const hashPagesRef = useRef(0);

  const locale = lang === "zh" ? "zh-CN" : "en-US";
  const dateFormat = new Intl.DateTimeFormat(locale, { year: "numeric", month: "long", day: "numeric", timeZone: "Asia/Shanghai" });
  const relative = new Intl.RelativeTimeFormat(locale, { numeric: "auto" });
  // 按上海时区的日历日相减，与 dateFormat 同口径；按毫秒差四舍五入会把「昨天晚上」算成「今天」。
  const dayKey = new Intl.DateTimeFormat("en-CA", { year: "numeric", month: "2-digit", day: "2-digit", timeZone: "Asia/Shanghai" });
  const calendarDay = (d: Date) => Date.parse(`${dayKey.format(d)}T00:00:00Z`) / 86_400_000;
  const relativeLabel = (iso: string) => {
    const days = calendarDay(new Date(iso)) - calendarDay(new Date());
    if (Math.abs(days) < 30) return relative.format(days, "day");
    if (Math.abs(days) < 365) return relative.format(Math.round(days / 30), "month");
    return relative.format(Math.round(days / 365), "year");
  };

  const fetchPage = useCallback(async (ch: Channel, p: number) => {
    const id = ++requestRef.current;
    setStatus(p === 1 ? "loading" : "more");
    try {
      const res = await fetch(withBase(`/api/changelog?page=${p}&per_page=${PER_PAGE}&channel=${ch}`));
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = (await res.json()) as PageData;
      if (id !== requestRef.current) return;
      setReleases((prev) => (p === 1 ? data.releases : [...prev, ...data.releases]));
      setHasMore(Boolean(data.has_more));
      setPage(p);
      setStatus("ready");
    } catch {
      if (id === requestRef.current) setStatus("error");
    }
  }, []);

  useEffect(() => {
    const hash = decodeURIComponent(window.location.hash.slice(1));
    const initial: Channel = /^v[\w-]*-rc-/.test(hash) ? "frontier" : "stable";
    hashRef.current = hash || null;
    setChannel(initial);
    void fetchPage(initial, 1);
  }, [fetchPage]);

  // 锚点目标:已渲染则滚动过去,否则继续翻页(有上限)
  useEffect(() => {
    const hash = hashRef.current;
    if (status !== "ready" || !hash) return;
    const target = document.getElementById(hash);
    if (target) {
      hashRef.current = null;
      requestAnimationFrame(() => target.scrollIntoView({ block: "start" }));
    } else if (hasMore && hashPagesRef.current < MAX_HASH_PAGES) {
      hashPagesRef.current += 1;
      void fetchPage(channel, page + 1);
    } else {
      hashRef.current = null;
    }
  }, [status, releases, hasMore, page, channel, fetchPage]);

  // 版本索引高亮:取视口上部最近进入的条目
  useEffect(() => {
    const entries = document.querySelectorAll<HTMLElement>("[data-entry]");
    if (!entries.length) return;
    const observer = new IntersectionObserver(
      (records) => {
        const visible = records.filter((r) => r.isIntersecting).sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top);
        if (visible[0]) setActive((visible[0].target as HTMLElement).dataset.entry ?? null);
      },
      { rootMargin: "-80px 0px -65% 0px" },
    );
    entries.forEach((el) => observer.observe(el));
    return () => observer.disconnect();
  }, [releases]);

  const switchChannel = (next: Channel) => {
    if (next === channel) return;
    setChannel(next);
    setReleases([]);
    setHasMore(false);
    hashRef.current = null;
    void fetchPage(next, 1);
  };

  return (
    <div className="cl-layout">
      <aside className="cl-aside">
        <div className="cl-aside-inner">
          <div className="segmented" role="group" aria-label={t.channel}>
            {(["stable", "frontier"] as const).map((ch) => (
              <button key={ch} type="button" aria-pressed={channel === ch} data-channel={ch} onClick={() => switchChannel(ch)}>
                {ch === "stable" ? t.stable : t.preview}
              </button>
            ))}
          </div>
          {channel === "frontier" && <p className="mt-3 text-[12.5px] leading-relaxed text-muted">{t.previewHint}</p>}
          {releases.length > 0 && (
            <nav aria-label={t.versions} className="mt-6 hidden lg:block">
              <p className="eyebrow-plain mb-2">{t.versions}</p>
              <ol className="cl-index">
                {releases.map((r) => {
                  const anchor = versionAnchor(r.tag);
                  return (
                    <li key={r.tag}>
                      <a href={`#${anchor}`} aria-current={active === anchor ? "location" : undefined}>
                        {r.tag}
                      </a>
                    </li>
                  );
                })}
              </ol>
            </nav>
          )}
        </div>
      </aside>

      <div className="cl-main" aria-busy={status === "loading"}>
        {status === "loading" && (
          <div className="flex flex-col gap-10" role="status" aria-label={t.loading}>
            {[0, 1, 2].map((i) => (
              <div key={i} className="cl-skeleton" aria-hidden="true">
                <span style={{ width: "30%" }} />
                <span style={{ width: "90%" }} />
                <span style={{ width: "75%" }} />
                <span style={{ width: "60%" }} />
              </div>
            ))}
          </div>
        )}

        {status === "error" && releases.length === 0 && (
          <div className="cl-error" role="alert">
            <p>{t.error}</p>
            <div className="flex flex-wrap gap-2">
              <button type="button" className="btn btn-secondary btn-sm" onClick={() => void fetchPage(channel, 1)}>
                {t.retry}
              </button>
              <a className="btn btn-ghost btn-sm" href={`${GITHUB_URL}/releases`} target="_blank" rel="noopener">
                {t.github}
                <ArrowUpRight size={13} strokeWidth={2} aria-hidden="true" />
              </a>
            </div>
          </div>
        )}

        {status !== "loading" && status !== "error" && releases.length === 0 && (
          <p className="py-16 text-center text-[14px] text-muted">{t.empty}</p>
        )}

        {releases.length > 0 && (
          <div className="cl-timeline">
            {releases.map((release) => (
              <ReleaseEntry
                key={release.tag}
                release={release}
                lang={lang}
                dateLabel={dateFormat.format(new Date(release.published_at))}
                relativeLabel={relativeLabel(release.published_at)}
              />
            ))}
          </div>
        )}

        {releases.length > 0 && (hasMore || status === "error") && (
          <div className="mt-10 flex flex-col items-center gap-2">
            {status === "error" && <p className="text-[13px] text-danger" role="alert">{t.error}</p>}
            <button
              type="button"
              className="btn btn-secondary"
              disabled={status === "more"}
              onClick={() => void fetchPage(channel, page + 1)}
            >
              {status === "more" ? (
                <Loader2 size={15} strokeWidth={2} className="animate-spin" aria-hidden="true" />
              ) : (
                <ChevronDown size={15} strokeWidth={2} aria-hidden="true" />
              )}
              {status === "more" ? t.loading : status === "error" ? t.retry : t.loadMore}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
