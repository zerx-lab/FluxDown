/**
 * 社区页(反馈 / 路线图 / 投票)共用的小部件与格式化工具。
 * 样式类(`cm-*`)定义在 `CommunityStyles.astro`。
 */
import { useRef, type CSSProperties, type ReactNode } from "react";
import { CircleAlert, CircleCheck, CircleDot, CircleSlash, Copy, Loader2, type LucideIcon } from "lucide-react";
import { INTL_LOCALE, type Lang } from "@/i18n/config";
import { cn } from "@/lib/utils";

export const intlLocale = (lang: Lang) => INTL_LOCALE[lang];

/** "3d ago" / "3 天前"。 */
export function timeAgo(dateStr: string, lang: Lang): string {
  const diff = (new Date(dateStr).getTime() - Date.now()) / 1000;
  if (Number.isNaN(diff)) return "";
  const rtf = new Intl.RelativeTimeFormat(intlLocale(lang), { numeric: "auto", style: "short" });
  const abs = Math.abs(diff);
  if (abs < 60) return rtf.format(0, "second");
  if (abs < 3600) return rtf.format(Math.round(diff / 60), "minute");
  if (abs < 86400) return rtf.format(Math.round(diff / 3600), "hour");
  if (abs < 86400 * 30) return rtf.format(Math.round(diff / 86400), "day");
  if (abs < 86400 * 365) return rtf.format(Math.round(diff / (86400 * 30)), "month");
  return rtf.format(Math.round(diff / (86400 * 365)), "year");
}

/** "Jul 16, 2026"(可带时间,统一按北京时间展示,与反馈元数据一致)。 */
export function formatDate(dateStr: string, lang: Lang, withTime = false): string {
  const d = new Date(dateStr);
  if (Number.isNaN(d.getTime())) return "";
  return new Intl.DateTimeFormat(intlLocale(lang), {
    year: "numeric",
    month: "short",
    day: "numeric",
    ...(withTime ? { hour: "2-digit", minute: "2-digit", timeZone: "Asia/Shanghai" } : {}),
  }).format(d);
}

export function Spinner({ className }: { className?: string }) {
  return <Loader2 aria-hidden className={cn("size-4 animate-spin", className)} />;
}

/** 加载 / 错误 / 空状态占位块。 */
export function Placeholder({
  tone = "muted",
  children,
  className,
}: {
  tone?: "muted" | "loading" | "error";
  children: ReactNode;
  className?: string;
}) {
  return (
    <div
      role={tone === "error" ? "alert" : "status"}
      className={cn(
        "flex min-h-40 items-center justify-center gap-2.5 px-6 py-12 text-center text-sm",
        tone === "error" ? "text-danger" : "text-muted",
        className,
      )}
    >
      {tone === "loading" && <Spinner />}
      {tone === "error" && <CircleAlert aria-hidden className="size-4 shrink-0" />}
      <span>{children}</span>
    </div>
  );
}

/** 行内提交结果提示(成功 / 失败),读屏器可感知。 */
export function Notice({ tone, children, className }: { tone: "ok" | "error"; children: ReactNode; className?: string }) {
  const Icon = tone === "ok" ? CircleCheck : CircleAlert;
  return (
    <p
      role={tone === "error" ? "alert" : "status"}
      className={cn(
        "cm-notice inline-flex items-center gap-1.5 text-sm",
        tone === "ok" ? "text-ok" : "text-danger",
        className,
      )}
    >
      <Icon aria-hidden className="size-4 shrink-0" />
      {children}
    </p>
  );
}

export interface TabDef<K extends string> {
  key: K;
  label: string;
  icon?: LucideIcon;
}

/** 分段式标签切换(WAI-ARIA tabs:方向键 / Home / End 切换)。 */
export function Tabs<K extends string>({
  label,
  tabs,
  active,
  onChange,
  idPrefix,
  className,
}: {
  label: string;
  tabs: TabDef<K>[];
  active: K;
  onChange: (key: K) => void;
  idPrefix: string;
  className?: string;
}) {
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  const move = (index: number) => {
    const next = (index + tabs.length) % tabs.length;
    onChange(tabs[next].key);
    refs.current[next]?.focus();
  };
  return (
    <div role="tablist" aria-label={label} className={cn("cm-tabs", className)}>
      {tabs.map((tab, i) => {
        const Icon = tab.icon;
        const selected = tab.key === active;
        return (
          <button
            key={tab.key}
            ref={(el) => {
              refs.current[i] = el;
            }}
            type="button"
            role="tab"
            id={`${idPrefix}-tab-${tab.key}`}
            aria-controls={`${idPrefix}-panel-${tab.key}`}
            aria-selected={selected}
            tabIndex={selected ? 0 : -1}
            onClick={() => onChange(tab.key)}
            onKeyDown={(e) => {
              if (e.key === "ArrowRight") move(i + 1);
              else if (e.key === "ArrowLeft") move(i - 1);
              else if (e.key === "Home") move(0);
              else if (e.key === "End") move(tabs.length - 1);
              else return;
              e.preventDefault();
            }}
          >
            {Icon && <Icon aria-hidden className="size-3.5" />}
            {tab.label}
          </button>
        );
      })}
    </div>
  );
}

/** GitHub 标签:原色通过 color-mix 适配明暗主题(不依赖 JS 主题探测)。 */
export function LabelChip({ name, color }: { name: string; color?: string | null }) {
  const hex = color ? color.replace(/^#/, "") : "";
  const valid = /^[0-9a-fA-F]{6}$/.test(hex);
  return (
    <span className="cm-label" style={valid ? ({ "--c": `#${hex}` } as CSSProperties) : undefined}>
      {name}
    </span>
  );
}

export type IssueState = "open" | "completed" | "not_planned" | "duplicate";

const STATE_META: Record<IssueState, { icon: LucideIcon; tone: string }> = {
  open: { icon: CircleDot, tone: "text-ok" },
  completed: { icon: CircleCheck, tone: "text-accent-ink" },
  not_planned: { icon: CircleSlash, tone: "text-subtle" },
  duplicate: { icon: Copy, tone: "text-subtle" },
};

export function StateIcon({ state, className }: { state: IssueState; className?: string }) {
  const { icon: Icon, tone } = STATE_META[state];
  return <Icon aria-hidden className={cn("size-4 shrink-0", tone, className)} />;
}

/** 状态徽章(图标 + 文案)。 */
export function StateBadge({ state, label }: { state: IssueState; label: string }) {
  return (
    <span className={cn("cm-state", `cm-state-${state}`)}>
      <StateIcon state={state} className="size-3.5 text-current" />
      {label}
    </span>
  );
}
