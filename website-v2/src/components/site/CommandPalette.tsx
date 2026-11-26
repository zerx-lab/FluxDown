/**
 * ⌘K 命令面板:页面导航 + 文档全文搜索 + 快捷操作。
 * 基于原生 <dialog>(top layer、Esc 关闭、焦点陷阱);文档索引首次打开时按语言懒加载。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ArrowRight, BookOpen, CornerDownLeft, FileText, Languages, Search, SunMoon } from "lucide-react";
import { BrandIcon } from "../icons/brand";
import { common } from "@/i18n/messages/common";
import { LANG_LABEL, type Lang } from "@/i18n/config";
import { searchDocs, type SearchDoc } from "@/lib/docs-search";
import { GITHUB_URL } from "@/lib/site-nav";
import { withBase } from "@/lib/base";
import { rememberLocale } from "@/lib/locale-pref";

interface PaletteLink {
  label: string;
  href: string;
}

interface Item {
  id: string;
  group: "pages" | "docs" | "actions";
  label: string;
  detail?: string;
  icon: React.ReactNode;
  run: () => void;
}

interface Props {
  lang: Lang;
  links: PaletteLink[];
  /** 其余语言下的当前页地址(语言切换动作,每种语言一条)。 */
  langLinks: { lang: Lang; href: string }[];
}

export default function CommandPalette({ lang, links, langLinks }: Props) {
  const t = common[lang].search;
  const dialogRef = useRef<HTMLDialogElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const [docs, setDocs] = useState<SearchDoc[] | null>(null);

  const open = useCallback(() => {
    const dialog = dialogRef.current;
    if (!dialog || dialog.open) return;
    setQuery("");
    setActive(0);
    dialog.showModal();
    requestAnimationFrame(() => inputRef.current?.focus());
    if (!docs) {
      fetch(withBase(`/docs/search-${lang}.json`))
        .then((res) => (res.ok ? res.json() : []))
        .then((data: SearchDoc[]) => setDocs(data))
        .catch(() => setDocs([]));
    }
  }, [docs, lang]);

  useEffect(() => {
    window.addEventListener("palette:open", open);
    return () => window.removeEventListener("palette:open", open);
  }, [open]);

  const go = (url: string) => {
    dialogRef.current?.close();
    window.location.href = url;
  };

  const items = useMemo<Item[]>(() => {
    const q = query.trim().toLowerCase();
    const pageItems: Item[] = links
      .filter((l) => !q || l.label.toLowerCase().includes(q) || l.href.includes(q))
      .map((l) => ({
        id: `page:${l.href}`,
        group: "pages",
        label: l.label,
        detail: l.href,
        icon: <FileText size={15} strokeWidth={1.75} />,
        run: () => go(l.href),
      }));
    const docItems: Item[] =
      q && docs
        ? searchDocs(docs, q, 8).map((r) => ({
            id: `doc:${r.doc.href}`,
            group: "docs",
            label: r.doc.title,
            detail: r.matchedHeading ?? r.doc.description,
            icon: <BookOpen size={15} strokeWidth={1.75} />,
            run: () => go(r.doc.href),
          }))
        : [];
    const actions: Item[] = [
      {
        id: "action:theme",
        group: "actions",
        label: t.toggleTheme,
        icon: <SunMoon size={15} strokeWidth={1.75} />,
        run: () => {
          dialogRef.current?.close();
          document.querySelector<HTMLElement>("[data-theme-toggle]")?.click();
        },
      },
      ...langLinks.map(
        (l): Item => ({
          id: `action:lang:${l.lang}`,
          group: "actions",
          label: LANG_LABEL[l.lang],
          icon: <Languages size={15} strokeWidth={1.75} />,
          run: () => {
            rememberLocale(l.lang);
            go(l.href);
          },
        }),
      ),
      {
        id: "action:github",
        group: "actions",
        label: t.openGithub,
        icon: <BrandIcon name="github" size={15} />,
        run: () => {
          dialogRef.current?.close();
          window.open(GITHUB_URL, "_blank", "noopener");
        },
      },
    ].filter((a) => !q || a.label.toLowerCase().includes(q)) as Item[];
    return [...pageItems, ...docItems, ...actions];
  }, [query, docs, links, langLinks, t]);

  useEffect(() => setActive(0), [query]);
  useEffect(() => {
    listRef.current
      ?.querySelector<HTMLElement>(`[data-index="${active}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [active]);

  const onKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActive((i) => Math.min(items.length - 1, i + 1));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive((i) => Math.max(0, i - 1));
    } else if (event.key === "Enter") {
      event.preventDefault();
      items[active]?.run();
    }
  };

  const groups: { id: Item["group"]; label: string }[] = [
    { id: "pages", label: t.pages },
    { id: "docs", label: t.docs },
    { id: "actions", label: t.actions },
  ];

  return (
    <dialog
      ref={dialogRef}
      className="palette"
      aria-label={t.open}
      onClick={(event) => {
        if (event.target === dialogRef.current) dialogRef.current?.close();
      }}
    >
      <div className="palette-panel" onKeyDown={onKeyDown}>
        <label className="flex items-center gap-3 border-b border-line px-4">
          <Search size={16} strokeWidth={1.75} className="text-subtle" />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t.placeholder}
            className="h-13 w-full bg-transparent text-[15px] text-fg outline-none placeholder:text-subtle"
            role="combobox"
            aria-expanded="true"
            aria-controls="palette-list"
            aria-activedescendant={items[active] ? `palette-${active}` : undefined}
          />
          <kbd className="kbd">esc</kbd>
        </label>
        <div ref={listRef} id="palette-list" role="listbox" className="max-h-[min(60vh,440px)] overflow-y-auto p-2">
          {items.length === 0 && <div className="px-3 py-10 text-center text-sm text-subtle">{t.empty}</div>}
          {groups.map((group) => {
            const groupItems = items.filter((i) => i.group === group.id);
            if (groupItems.length === 0) return null;
            return (
              <div key={group.id} className="mb-1">
                <div className="eyebrow-plain px-3 pb-1.5 pt-2.5">{group.label}</div>
                {groupItems.map((item) => {
                  const index = items.indexOf(item);
                  const selected = index === active;
                  return (
                    <button
                      type="button"
                      key={item.id}
                      id={`palette-${index}`}
                      data-index={index}
                      role="option"
                      aria-selected={selected}
                      onMouseMove={() => setActive(index)}
                      onClick={item.run}
                      className={`flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left transition-colors ${
                        selected ? "bg-inset text-fg" : "text-muted"
                      }`}
                    >
                      <span className={selected ? "text-accent-ink" : "text-subtle"}>{item.icon}</span>
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-[14px]">{item.label}</span>
                        {item.detail && <span className="block truncate text-[12px] text-subtle">{item.detail}</span>}
                      </span>
                      {selected && <ArrowRight size={14} strokeWidth={1.75} className="text-subtle" />}
                    </button>
                  );
                })}
              </div>
            );
          })}
        </div>
        <div className="flex items-center gap-4 border-t border-line px-4 py-2.5 text-[12px] text-subtle">
          <span className="flex items-center gap-1.5">
            <kbd className="kbd">↑</kbd>
            <kbd className="kbd">↓</kbd>
            {t.hint}
          </span>
          <span className="flex items-center gap-1.5">
            <kbd className="kbd">
              <CornerDownLeft size={11} strokeWidth={2} />
            </kbd>
            {t.select}
          </span>
        </div>
      </div>
    </dialog>
  );
}
