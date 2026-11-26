import { useState, type InputHTMLAttributes, type ReactNode } from "react";
import { Eye, EyeOff, LoaderCircle, RefreshCw, TriangleAlert, type LucideIcon } from "lucide-react";

/** 分区标题 id:切换分区后把焦点移到这里。 */
export const HEADING_ID = "section-heading";

export function Spinner({ size = 15, className = "" }: { size?: number; className?: string }) {
  return <LoaderCircle size={size} className={`animate-spin motion-reduce:animate-none ${className}`} aria-hidden />;
}

/** 占位块:与最终内容同高同宽,避免加载完成时的布局位移。 */
export function Skeleton({ className = "" }: { className?: string }) {
  return <div aria-hidden className={`animate-pulse rounded-sm bg-inset motion-reduce:animate-none ${className}`} />;
}

/** 行内错误(`role="alert"`:出现即播报)。 */
export function ErrorLine({ text, id }: { text: string | null | undefined; id?: string }) {
  if (!text) return null;
  return (
    <p id={id} role="alert" className="mt-3 text-[13px] text-danger">
      {text}
    </p>
  );
}

/** 成功 / 进度提示:容器常驻,文字变化时才被读屏播报。 */
export function StatusLine({ text }: { text: string | null }) {
  return (
    <p role="status" className={text ? "mt-3 text-[13px] text-ok" : "sr-only"}>
      {text}
    </p>
  );
}

export function Panel({ children, className = "" }: { children: ReactNode; className?: string }) {
  return <div className={`border border-line bg-elev ${className}`}>{children}</div>;
}

export function SectionHead({
  title,
  lede,
  actions,
}: {
  title: string;
  lede?: string;
  actions?: ReactNode;
}) {
  return (
    <header className="flex flex-wrap items-end justify-between gap-x-6 gap-y-3 border-b border-line pb-5">
      <div className="min-w-0">
        <h2 id={HEADING_ID} tabIndex={-1} className="h3 focus:outline-none">
          {title}
        </h2>
        {lede && <p className="mt-2 max-w-[60ch] text-sm leading-relaxed text-muted">{lede}</p>}
      </div>
      {actions}
    </header>
  );
}

/** 子区块标题(分区内的二级标题)。 */
export function SubHead({ title, lede, actions }: { title: string; lede?: string; actions?: ReactNode }) {
  return (
    <div className="flex flex-wrap items-end justify-between gap-x-6 gap-y-2">
      <div className="min-w-0">
        <h3 className="text-base font-semibold tracking-tight">{title}</h3>
        {lede && <p className="mt-1 text-[13px] leading-relaxed text-muted">{lede}</p>}
      </div>
      {actions}
    </div>
  );
}

export function RefreshButton({ label, busy, onClick }: { label: string; busy: boolean; onClick: () => void }) {
  return (
    <button type="button" onClick={onClick} disabled={busy} aria-busy={busy} className="btn btn-secondary btn-sm disabled:opacity-60">
      <RefreshCw size={14} aria-hidden className={busy ? "animate-spin motion-reduce:animate-none" : ""} />
      {label}
    </button>
  );
}

export function EmptyState({
  icon: Icon,
  title,
  desc,
  action,
}: {
  icon: LucideIcon;
  title: string;
  desc?: string;
  action?: ReactNode;
}) {
  return (
    <div className="flex flex-col items-center gap-2 px-6 py-12 text-center">
      <span aria-hidden className="mb-1 grid size-10 place-items-center rounded-full bg-inset text-muted">
        <Icon size={18} />
      </span>
      <p className="text-sm font-semibold">{title}</p>
      {desc && <p className="max-w-[44ch] text-[13px] leading-relaxed text-muted">{desc}</p>}
      {action && <div className="mt-2">{action}</div>}
    </div>
  );
}

/** 分区级加载失败:说明 + 重试。 */
export function LoadFailed({ text, retryLabel, onRetry }: { text: string; retryLabel: string; onRetry: () => void }) {
  return (
    <div role="alert" className="flex flex-col items-start gap-3 border border-line bg-elev p-5 sm:flex-row sm:items-center sm:justify-between">
      <p className="flex items-start gap-2.5 text-sm text-muted">
        <TriangleAlert size={16} className="mt-0.5 shrink-0 text-warn" aria-hidden />
        <span>{text}</span>
      </p>
      <button type="button" onClick={onRetry} className="btn btn-secondary btn-sm shrink-0">
        {retryLabel}
      </button>
    </div>
  );
}

/** 用户数据文本:超长截断,完整内容放 title;空值显示 `-`。 */
export function Clip({ text, className = "" }: { text: string; className?: string }) {
  return (
    <span title={text} className={`block min-w-0 truncate ${className}`}>
      {text}
    </span>
  );
}

type TextFieldProps = Omit<InputHTMLAttributes<HTMLInputElement>, "id" | "className" | "type"> & {
  id: string;
  label: string;
  hint?: string;
  error?: string | null;
  /** 提供时渲染密码显隐切换(标签随状态在「显示 / 隐藏密码」间切换)。 */
  reveal?: { showLabel: string; hideLabel: string };
  type?: "text" | "email" | "password";
};

/** 标签 + 输入 + 提示 / 错误;错误经 `aria-describedby` 与输入关联。 */
export function TextField({ id, label, hint, error, reveal, type = "text", ...rest }: TextFieldProps) {
  const [shown, setShown] = useState(false);
  const describedBy = [hint ? `${id}-hint` : null, error ? `${id}-error` : null].filter(Boolean).join(" ") || undefined;
  const input = (
    <input
      id={id}
      type={reveal ? (shown ? "text" : "password") : type}
      aria-invalid={error ? true : undefined}
      aria-describedby={describedBy}
      className={`field ${reveal ? "pr-11" : ""}`}
      {...rest}
    />
  );
  return (
    <div>
      <label htmlFor={id} className="mb-2 block text-[13px] font-medium">
        {label}
      </label>
      {reveal ? (
        <div className="relative">
          {input}
          <button
            type="button"
            aria-label={shown ? reveal.hideLabel : reveal.showLabel}
            onClick={() => setShown((v) => !v)}
            className="absolute inset-y-0 right-0 grid w-10 place-items-center rounded-r-lg text-muted hover:text-fg"
          >
            {shown ? <EyeOff size={16} aria-hidden /> : <Eye size={16} aria-hidden />}
          </button>
        </div>
      ) : (
        input
      )}
      {hint && (
        <p id={`${id}-hint`} className="mt-2 text-[12.5px] leading-relaxed text-muted">
          {hint}
        </p>
      )}
      {error && (
        <p id={`${id}-error`} role="alert" className="mt-2 text-[13px] text-danger">
          {error}
        </p>
      )}
    </div>
  );
}

/** 用量条:原生 `<progress>`(`max = 0` 表示不限,调用方不渲染)。 */
export function UsageBar({ value, max, label, valueText }: { value: number; max: number; label: string; valueText: string }) {
  return (
    <progress
      value={Math.min(value, max)}
      max={max}
      aria-label={label}
      aria-valuetext={valueText}
      className="block h-1.5 w-full appearance-none overflow-hidden rounded-full bg-inset accent-[var(--accent)] [&::-moz-progress-bar]:bg-accent [&::-webkit-progress-bar]:bg-inset [&::-webkit-progress-value]:bg-accent"
    />
  );
}
