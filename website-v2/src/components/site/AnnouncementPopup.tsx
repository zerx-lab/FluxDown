/**
 * 强提示公告弹窗(如安全警告):取第一条进行中、带 `popup` 且未被关闭的公告,
 * 用原生 <dialog>.showModal() 展示(top layer、Esc 关闭、焦点陷阱)。关闭即写入关闭记录。
 */
import { useEffect, useRef, useState } from "react";
import { ArrowUpRight, ShieldAlert } from "lucide-react";
import { toBaselineLang, type Lang } from "@/i18n/config";
import { DISMISSED_KEY, OFFICIAL_SITES, popupAnnouncements, type Announcement } from "@/lib/announcements";

const COPY = {
  en: {
    blocked: "Fake site — do not visit",
    official: "Official sites — trust only these",
    confirm: "I understand",
    goOfficial: "Go to the official site",
  },
  zh: {
    blocked: "仿冒网站(请勿访问)",
    official: "官方网址(认准以下域名)",
    confirm: "我已知晓",
    goOfficial: "前往官网",
  },
} as const;

function readDismissed(): string[] {
  try {
    const list: unknown = JSON.parse(localStorage.getItem(DISMISSED_KEY) ?? "[]");
    return Array.isArray(list) ? list.filter((id): id is string => typeof id === "string") : [];
  } catch {
    return [];
  }
}

export default function AnnouncementPopup({ lang }: { lang: Lang }) {
  // 弹窗文案与公告数据只有 en / zh 手写基线,机器翻译语言沿用 en
  const copyLang = toBaselineLang(lang);
  const t = COPY[copyLang];
  const dialogRef = useRef<HTMLDialogElement>(null);
  const [current, setCurrent] = useState<Announcement | null>(null);

  useEffect(() => {
    const dismissed = readDismissed();
    setCurrent(popupAnnouncements().find((a) => !dismissed.includes(a.id)) ?? null);
  }, []);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (current && dialog && !dialog.open) dialog.showModal();
  }, [current]);

  const dismiss = () => {
    if (!current) return;
    const dismissed = readDismissed();
    if (!dismissed.includes(current.id)) {
      try {
        localStorage.setItem(DISMISSED_KEY, JSON.stringify([...dismissed, current.id]));
      } catch {
        /* localStorage 不可用:本次会话内仍关闭 */
      }
    }
  };

  if (!current?.popup) return null;
  const popup = current.popup;

  return (
    <dialog
      ref={dialogRef}
      aria-labelledby="announcement-popup-title"
      onClose={() => {
        dismiss();
        setCurrent(null);
      }}
      onClick={(event) => {
        if (event.target === event.currentTarget) dialogRef.current?.close();
      }}
      className="m-auto w-[min(520px,calc(100vw-32px))] max-w-none overflow-visible border-0 bg-transparent p-0 text-fg opacity-100 transition-[opacity,translate] duration-200 backdrop:bg-bg/70 backdrop:backdrop-blur-[6px] starting:open:translate-y-2 starting:open:opacity-0"
    >
      <div className="relative overflow-hidden rounded-[14px] bg-elev shadow-[0_0_0_1px_var(--line-strong),var(--shadow-window)]">
        <div className="h-[2px] bg-danger" aria-hidden="true" />
        <div className="flex flex-col gap-5 p-6 sm:p-7">
          <div className="flex items-center gap-3">
            <span className="grid size-10 shrink-0 place-items-center rounded-lg text-danger shadow-[0_0_0_1px_color-mix(in_oklch,var(--danger)_35%,transparent)_inset]">
              <ShieldAlert size={20} strokeWidth={1.75} aria-hidden="true" />
            </span>
            <div className="flex flex-col gap-0.5">
              <span className="font-mono text-[11px] tracking-[0.12em] text-subtle uppercase">{current.date}</span>
              <h2 id="announcement-popup-title" className="text-xl font-[560] tracking-[-0.02em]">
                {popup.title[copyLang]}
              </h2>
            </div>
          </div>
          <p className="text-[14.5px] leading-relaxed text-muted">{popup.body[copyLang]}</p>

          <div className="grid gap-px overflow-hidden rounded-[10px] bg-line shadow-[0_0_0_1px_var(--line)]">
            <div className="flex flex-col gap-1 bg-bg px-4 py-3">
              <span className="font-mono text-[10.5px] tracking-[0.12em] text-danger uppercase">{t.blocked}</span>
              {popup.blocked.map((domain) => (
                <span key={domain} className="font-mono text-sm line-through decoration-danger/70">
                  {domain}
                </span>
              ))}
            </div>
            <div className="flex flex-col gap-1 bg-bg px-4 py-3">
              <span className="font-mono text-[10.5px] tracking-[0.12em] text-ok uppercase">{t.official}</span>
              {OFFICIAL_SITES.map((site) => (
                <a key={site} href={site} className="link inline-flex w-fit items-center gap-1 font-mono text-sm">
                  {site.replace(/^https:\/\//, "")}
                  <ArrowUpRight size={13} strokeWidth={2} aria-hidden="true" />
                </a>
              ))}
            </div>
          </div>

          <div className="flex flex-col-reverse gap-2 sm:flex-row sm:justify-end">
            <button type="button" className="btn btn-secondary" onClick={() => dialogRef.current?.close()} autoFocus>
              {t.confirm}
            </button>
            <a href={OFFICIAL_SITES[0]} className="btn btn-primary" onClick={dismiss}>
              {t.goOfficial}
            </a>
          </div>
        </div>
      </div>
    </dialog>
  );
}
