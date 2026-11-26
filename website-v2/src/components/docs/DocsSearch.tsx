/**
 * 文档内联搜索(combobox):懒加载 `/docs/search-{lang}.json`,经 docs-search.ts 做模糊 + 全文匹配。
 *
 * - `compact`:侧栏 / 抽屉中的输入框,结果以浮层列出;
 * - `hero`:文档首页大输入框,结果内联展开,并与 `?q=` 查询参数双向同步(SearchAction 入口)。
 *
 * 键盘:↑↓ 选择,Enter 打开,Esc 清空 / 收起。
 */
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { CornerDownLeft, Search, X } from "lucide-react";
import { searchDocs, type SearchDoc, type SearchResult } from "@/lib/docs-search";
import { SECTIONS } from "@/lib/docs-nav";
import type { Lang } from "@/i18n/config";
import { docs as messages } from "@/i18n/messages/docs";
import { cn } from "@/lib/utils";
import { withBase } from "@/lib/base";

interface Props {
  lang: Lang;
  variant?: "compact" | "hero";
}

type Status = "idle" | "loading" | "ready" | "error";

// 同一页面上多个搜索框共用一份索引请求。
const indexCache = new Map<Lang, Promise<SearchDoc[]>>();

function loadIndex(lang: Lang): Promise<SearchDoc[]> {
  let pending = indexCache.get(lang);
  if (!pending) {
    pending = fetch(withBase(`/docs/search-${lang}.json`)).then((res) => {
      if (!res.ok) throw new Error(String(res.status));
      return res.json() as Promise<SearchDoc[]>;
    });
    pending.catch(() => indexCache.delete(lang));
    indexCache.set(lang, pending);
  }
  return pending;
}

export default function DocsSearch({ lang, variant = "compact" }: Props) {
  const t = messages[lang];
  const hero = variant === "hero";
  const id = useId();
  const listId = `${id}-list`;
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState<SearchDoc[] | null>(null);
  const [status, setStatus] = useState<Status>("idle");
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);

  const ensureIndex = useCallback(() => {
    if (index || status === "loading") return;
    setStatus("loading");
    loadIndex(lang)
      .then((data) => {
        setIndex(data);
        setStatus("ready");
      })
      .catch(() => setStatus("error"));
  }, [index, status, lang]);

  // hero:从 ?q= 恢复查询(SearchAction / 分享链接)
  useEffect(() => {
    if (!hero) return;
    const q = new URLSearchParams(window.location.search).get("q");
    if (q) {
      setQuery(q);
      setOpen(true);
      ensureIndex();
    }
    // 仅挂载时读取一次
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // hero:查询写回 URL(replaceState,不产生历史记录)
  useEffect(() => {
    if (!hero) return;
    const url = new URL(window.location.href);
    const q = query.trim();
    if (q) url.searchParams.set("q", q);
    else url.searchParams.delete("q");
    if (url.href !== window.location.href) history.replaceState(history.state, "", url);
  }, [hero, query]);

  const q = query.trim();
  const results: SearchResult[] = useMemo(
    () => (index && q ? searchDocs(index, q, hero ? 20 : 8) : []),
    [index, q, hero],
  );

  useEffect(() => setActive(0), [q]);
  useEffect(() => {
    listRef.current
      ?.querySelector<HTMLElement>(`[data-idx="${active}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [active]);

  const expanded = open && q.length > 0;

  const onKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setOpen(true);
      setActive((i) => Math.min(results.length - 1, i + 1));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive((i) => Math.max(0, i - 1));
    } else if (event.key === "Enter") {
      const target = results[active];
      if (expanded && target) {
        event.preventDefault();
        window.location.href = target.doc.href;
      }
    } else if (event.key === "Escape") {
      if (query) {
        event.preventDefault();
        setQuery("");
      } else if (!hero) {
        inputRef.current?.blur();
      }
      setOpen(false);
    }
  };

  const sectionLabel = (section: string) => {
    const known = SECTIONS.find((id) => id === section);
    return known ? t.sectionLabels[known] : section;
  };

  const panel = (() => {
    if (status === "error") {
      return (
        <p className="flex items-center justify-between gap-3 px-4 py-4 text-sm text-danger" role="alert">
          {t.search.error}
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            onMouseDown={(e) => e.preventDefault()}
            onClick={ensureIndex}
          >
            {t.search.retry}
          </button>
        </p>
      );
    }
    if (!index) {
      return <p className="px-4 py-5 text-sm text-subtle">{t.search.loading}</p>;
    }
    if (results.length === 0) {
      return <p className="px-4 py-5 text-sm text-subtle">{t.search.empty(q)}</p>;
    }
    return (
      <>
        <div className="eyebrow-plain flex items-center justify-between border-b border-line px-4 py-2">
          <span className="num">{t.search.results(results.length)}</span>
          <span className="hidden items-center gap-1 sm:flex" aria-hidden="true">
            <span className="kbd">↑</span>
            <span className="kbd">↓</span>
            <span className="kbd">
              <CornerDownLeft size={11} />
            </span>
          </span>
        </div>
        <ul
          ref={listRef}
          id={listId}
          role="listbox"
          aria-label={t.search.label}
          className={cn("overflow-y-auto py-1", hero ? "max-h-[min(560px,65vh)]" : "max-h-[min(440px,60vh)]")}
          onMouseDown={(e) => e.preventDefault()}
        >
          {results.map((r, i) => (
            <li key={r.doc.slug} role="presentation">
              <a
                id={`${id}-opt-${i}`}
                data-idx={i}
                role="option"
                aria-selected={i === active}
                href={r.doc.href}
                tabIndex={-1}
                onMouseEnter={() => setActive(i)}
                className={cn(
                  "block border-l-2 px-4 py-2.5 transition-colors",
                  i === active ? "border-accent bg-accent-soft" : "border-transparent",
                )}
              >
                <span className="flex min-w-0 items-baseline gap-2">
                  <span className="shrink-0 text-sm font-medium text-fg">{r.doc.title}</span>
                  {r.matchedHeading && (
                    <span className="truncate text-xs text-muted">› {r.matchedHeading}</span>
                  )}
                  <span className="eyebrow-plain ml-auto shrink-0 !text-[10px]">
                    {sectionLabel(r.doc.section)}
                  </span>
                </span>
                {r.snippet.length > 0 && (
                  <span className="mt-1 line-clamp-2 block text-[13px] leading-relaxed text-muted">
                    {r.snippet.map((p, k) =>
                      p.hit ? (
                        <mark key={k} className="rounded-[3px] bg-accent-soft px-0.5 text-accent-ink">
                          {p.text}
                        </mark>
                      ) : (
                        <span key={k}>{p.text}</span>
                      ),
                    )}
                  </span>
                )}
              </a>
            </li>
          ))}
        </ul>
      </>
    );
  })();

  return (
    <div
      className="relative"
      role="search"
      onBlur={(e) => {
        if (!hero && !e.currentTarget.contains(e.relatedTarget as Node | null)) setOpen(false);
      }}
    >
      <label htmlFor={`${id}-input`} className="sr-only">
        {t.search.label}
      </label>
      <div className="relative">
        <Search
          size={hero ? 18 : 15}
          strokeWidth={1.75}
          aria-hidden="true"
          className={cn("pointer-events-none absolute top-1/2 -translate-y-1/2 text-subtle", hero ? "left-4" : "left-3")}
        />
        <input
          ref={inputRef}
          id={`${id}-input`}
          type="search"
          role="combobox"
          aria-expanded={expanded}
          aria-controls={listId}
          aria-autocomplete="list"
          aria-activedescendant={expanded && results[active] ? `${id}-opt-${active}` : undefined}
          value={query}
          placeholder={t.search.placeholder}
          autoComplete="off"
          spellCheck={false}
          enterKeyHint="go"
          onFocus={() => {
            ensureIndex();
            setOpen(true);
          }}
          onChange={(e) => {
            setQuery(e.target.value);
            setOpen(true);
            ensureIndex();
          }}
          onKeyDown={onKeyDown}
          className={cn(
            "field docs-search-input",
            hero ? "!h-12 !pl-11 !pr-11 !text-base" : "!h-9 !pl-9 !pr-8 !text-[13.5px]",
          )}
        />
        {query && (
          <button
            type="button"
            aria-label={t.search.clear}
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => {
              setQuery("");
              inputRef.current?.focus();
            }}
            className={cn(
              "absolute top-1/2 grid -translate-y-1/2 place-items-center rounded-md text-subtle hover:bg-inset hover:text-fg",
              hero ? "right-3 size-7" : "right-1.5 size-6",
            )}
          >
            <X size={hero ? 15 : 13} />
          </button>
        )}
      </div>
      {hero && !q && <p className="mt-3 text-[13px] text-subtle">{t.search.hint}</p>}
      {expanded && (
        <div
          className={cn(
            "overflow-hidden rounded-[10px] border border-line bg-elev",
            hero
              ? "mt-3"
              : "absolute left-0 top-full z-40 mt-1.5 w-[min(440px,calc(100vw-40px))] shadow-[var(--shadow-float)]",
          )}
          aria-live="polite"
        >
          {panel}
        </div>
      )}
    </div>
  );
}
