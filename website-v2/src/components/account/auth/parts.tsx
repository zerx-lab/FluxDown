import type { InputHTMLAttributes, ReactNode, Ref, SubmitEvent } from "react";
import { useState } from "react";
import { ArrowLeft, Check, Circle, Eye, EyeOff, LoaderCircle, ShieldCheck } from "lucide-react";
import type { Lang } from "@/i18n/config";
import type { accountAuth } from "@/i18n/messages/accountAuth";
import { href } from "@/i18n/routing";
import { CODE_LENGTH, PASSWORD_MIN, digitsOnly } from "./hooks";

export type FormLabels = (typeof accountAuth)["en"]["form"];
export type AgreeLabels = (typeof accountAuth)["en"]["login"]["agree"];

const DISABLED = "disabled:cursor-not-allowed disabled:opacity-50";

/** 认证卡片:mono 标题条 + 标题/副标题 + 内容 + 底部条(浅凹底),质感参考 `pay/Dialog.tsx` 面板。 */
export function AuthCard({
  bar,
  heading,
  headingId,
  sub,
  subId,
  children,
  footer,
}: {
  bar: string;
  heading: string;
  headingId: string;
  sub?: ReactNode;
  subId?: string;
  children: ReactNode;
  footer?: ReactNode;
}) {
  return (
    <section
      aria-labelledby={headingId}
      className="overflow-hidden rounded-[14px] bg-elev shadow-[0_0_0_1px_var(--line-strong),var(--shadow-float)]"
    >
      <div className="flex h-11 items-center gap-2 border-b border-line px-5 font-mono text-[11px] uppercase tracking-[0.12em] text-subtle sm:px-7">
        <ShieldCheck size={14} strokeWidth={1.75} aria-hidden />
        <span className="truncate">{bar}</span>
      </div>
      <div className="px-5 py-6 sm:px-7 sm:py-7">
        <h2 id={headingId} className="text-[22px] font-semibold leading-tight tracking-[-0.02em]">
          {heading}
        </h2>
        {sub && (
          <p id={subId} className="mt-1.5 text-[13.5px] leading-relaxed text-muted">
            {sub}
          </p>
        )}
        <div className="mt-6">{children}</div>
      </div>
      {footer && <div className="flex flex-col gap-2 border-t border-line bg-sunken px-5 py-4 text-[13px] text-muted sm:px-7">{footer}</div>}
    </section>
  );
}

export function ErrorLine({ children }: { children?: ReactNode }) {
  if (!children) return null;
  return (
    <p role="alert" className="text-[13px] leading-relaxed text-danger">
      {children}
    </p>
  );
}

/** 状态提示:容器始终在 DOM 里(空时隐藏),文字出现时才会被读屏播报。 */
export function Notice({ text }: { text: string | null }) {
  return (
    <p role="status" className="text-[13px] leading-relaxed text-muted empty:hidden">
      {text}
    </p>
  );
}

export function StatusPanel({ text }: { text: string }) {
  return (
    <p role="status" className="flex items-center gap-2.5 py-4 text-[13.5px] text-muted">
      <LoaderCircle size={15} className="animate-spin text-accent" aria-hidden />
      {text}
    </p>
  );
}

function FieldLabel({
  htmlFor,
  label,
  optional,
  action,
}: {
  htmlFor: string;
  label: string;
  optional?: string;
  action?: ReactNode;
}) {
  return (
    <div className="flex items-baseline justify-between gap-3">
      <label htmlFor={htmlFor} className="text-[13px] font-medium">
        {label}
        {optional && <span className="ml-1.5 text-xs font-normal text-subtle">{optional}</span>}
      </label>
      {action}
    </div>
  );
}

type NativeInput = Omit<InputHTMLAttributes<HTMLInputElement>, "id" | "value" | "onChange" | "className" | "children">;

export function TextField({
  id,
  label,
  value,
  onChange,
  optional,
  help,
  action,
  stepFocus,
  ref,
  ...native
}: NativeInput & {
  id: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  optional?: string;
  help?: string;
  action?: ReactNode;
  stepFocus?: boolean;
  ref?: Ref<HTMLInputElement>;
}) {
  const helpId = `${id}-help`;
  return (
    <div>
      <FieldLabel htmlFor={id} label={label} optional={optional} action={action} />
      <input
        {...native}
        ref={ref}
        id={id}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        aria-describedby={help ? helpId : undefined}
        data-step-focus={stepFocus ? "" : undefined}
        className="field mt-2"
      />
      {help && (
        <p id={helpId} className="mt-2 text-[12.5px] leading-relaxed text-muted">
          {help}
        </p>
      )}
    </div>
  );
}

/** 密码长度规则的实时提示:达标显示对勾,未达标显示空心圆;不做红色报错(提交时由浏览器校验)。 */
function PasswordRule({ id, value, labels }: { id: string; value: string; labels: FormLabels }) {
  const met = value.length >= PASSWORD_MIN;
  return (
    <p id={id} className={`mt-2 flex items-center gap-1.5 text-[12.5px] ${met ? "text-ok" : "text-muted"}`}>
      {met ? <Check size={13} strokeWidth={2.25} aria-hidden /> : <Circle size={13} aria-hidden />}
      <span>{labels.passwordRule}</span>
      {met && <span className="sr-only">{`, ${labels.ruleMet}`}</span>}
    </p>
  );
}

/** 密码输入:显隐切换按钮(文案随状态切换)+ 可选的长度规则提示。登录用 `current-password`,注册 / 重置用 `new-password`。 */
export function PasswordField({
  id,
  label,
  value,
  onChange,
  labels,
  autoComplete,
  action,
  stepFocus,
  ref,
}: {
  id: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  labels: FormLabels;
  autoComplete: "current-password" | "new-password";
  action?: ReactNode;
  stepFocus?: boolean;
  ref?: Ref<HTMLInputElement>;
}) {
  const [shown, setShown] = useState(false);
  const creating = autoComplete === "new-password";
  const ruleId = `${id}-rule`;
  return (
    <div>
      <FieldLabel htmlFor={id} label={label} action={action} />
      <div className="relative mt-2">
        <input
          ref={ref}
          id={id}
          name="password"
          type={shown ? "text" : "password"}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          autoComplete={autoComplete}
          minLength={creating ? PASSWORD_MIN : undefined}
          required
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
          aria-describedby={creating ? ruleId : undefined}
          data-step-focus={stepFocus ? "" : undefined}
          className="field pr-11"
        />
        <button
          type="button"
          onClick={() => setShown((v) => !v)}
          aria-label={shown ? labels.hidePassword : labels.showPassword}
          className="absolute inset-y-0 right-0 grid w-10 place-items-center rounded-r-lg text-subtle transition-colors hover:text-fg"
        >
          {shown ? <EyeOff size={16} strokeWidth={1.75} aria-hidden /> : <Eye size={16} strokeWidth={1.75} aria-hidden />}
        </button>
      </div>
      {creating && <PasswordRule id={ruleId} value={value} labels={labels} />}
    </div>
  );
}

/** 6 位验证码输入:只保留数字,可被系统 / 邮件自动填充(`one-time-code`)。`action` 放在输入框右侧(如「发送验证码」)。 */
export function CodeField({
  id,
  label,
  value,
  onChange,
  labels,
  describedBy,
  action,
  stepFocus,
  ref,
}: {
  id: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  labels: FormLabels;
  describedBy?: string;
  action?: ReactNode;
  stepFocus?: boolean;
  ref?: Ref<HTMLInputElement>;
}) {
  return (
    <div>
      <FieldLabel htmlFor={id} label={label} />
      <div className="mt-2 flex gap-2">
        <input
          ref={ref}
          id={id}
          name="code"
          value={value}
          onChange={(e) => onChange(digitsOnly(e.target.value))}
          inputMode="numeric"
          autoComplete="one-time-code"
          pattern={`[0-9]{${CODE_LENGTH}}`}
          placeholder={labels.codePlaceholder}
          autoCapitalize="none"
          autoCorrect="off"
          spellCheck={false}
          required
          aria-describedby={describedBy}
          data-step-focus={stepFocus ? "" : undefined}
          className="field mono num min-w-0 flex-1 tracking-[0.3em] placeholder:tracking-normal"
        />
        {action}
      </div>
    </div>
  );
}

export function SubmitButton({ busy, disabled, children }: { busy: boolean; disabled?: boolean; children: ReactNode }) {
  return (
    <button type="submit" disabled={busy || disabled} aria-busy={busy || undefined} className={`btn btn-primary w-full ${DISABLED}`}>
      {busy && <LoaderCircle size={15} className="animate-spin" aria-hidden />}
      {children}
    </button>
  );
}

/** 「重新发送」按钮:冷却中显示剩余秒数并禁用。 */
export function ResendButton({
  left,
  busy,
  onClick,
  labels,
}: {
  left: number;
  busy: boolean;
  onClick: () => void;
  labels: FormLabels;
}) {
  return (
    <button type="button" onClick={onClick} disabled={busy || left > 0} className={`btn btn-secondary btn-sm ${DISABLED}`}>
      {busy && <LoaderCircle size={13} className="animate-spin" aria-hidden />}
      <span className="num">{left > 0 ? labels.resendIn(left) : labels.resend}</span>
    </button>
  );
}

/** 步骤条:样式与 `pricing/WebPurchase.tsx` 一致(mono 序号 + 连接线 + `aria-current="step"`)。 */
export function Steps({ label, steps, current }: { label: string; steps: readonly string[]; current: number }) {
  return (
    <ol aria-label={label} className="mb-6 flex items-center gap-2 font-mono text-[10.5px] uppercase tracking-[0.1em]">
      {steps.map((name, i) => (
        <li
          key={name}
          aria-current={i === current ? "step" : undefined}
          className={`flex items-center gap-2 last:flex-none ${i < steps.length - 1 ? "flex-1" : ""} ${
            i > current ? "text-subtle" : i === current ? "text-accent-ink" : "text-muted"
          }`}
        >
          <span className="num">{String(i + 1).padStart(2, "0")}</span>
          <span className="truncate">{name}</span>
          {i < steps.length - 1 && <span aria-hidden className={`h-px min-w-3 flex-1 ${i < current ? "bg-accent" : "bg-line-strong"}`} />}
        </li>
      ))}
    </ol>
  );
}

/** 「注册 / 登录即表示同意服务条款与隐私政策」。 */
export function AgreeLine({ lang, labels }: { lang: Lang; labels: AgreeLabels }) {
  return (
    <p className="text-[12.5px] leading-relaxed text-subtle">
      {labels.prefix}
      <a className="link" href={href("/terms", lang)}>
        {labels.terms}
      </a>
      {labels.and}
      <a className="link" href={href("/privacy", lang)}>
        {labels.privacy}
      </a>
      {labels.suffix}
    </p>
  );
}

/** 验证码步骤(新浏览器验证 / 注册验证共用):验证码输入 + 提交 + 返回 + 重新发送(带冷却)。 */
export function VerifyStep({
  idPrefix,
  describedBy,
  labels,
  code,
  onCode,
  submitLabel,
  backLabel,
  busy,
  error,
  notice,
  cooldownLeft,
  onSubmit,
  onResend,
  onBack,
}: {
  idPrefix: string;
  describedBy: string;
  labels: FormLabels;
  code: string;
  onCode: (value: string) => void;
  submitLabel: string;
  backLabel: string;
  busy: "submit" | "resend" | null;
  error: ReactNode;
  notice: string | null;
  cooldownLeft: number;
  onSubmit: (event: SubmitEvent<HTMLFormElement>) => void;
  onResend: () => void;
  onBack: () => void;
}) {
  return (
    <form method="post" onSubmit={onSubmit} className="flex flex-col gap-4">
      <CodeField id={`${idPrefix}-code`} label={labels.code} value={code} onChange={onCode} labels={labels} describedBy={describedBy} stepFocus />
      <ErrorLine>{error}</ErrorLine>
      <Notice text={notice} />
      <SubmitButton busy={busy === "submit"} disabled={code.length !== CODE_LENGTH}>
        {submitLabel}
      </SubmitButton>
      <div className="flex items-center justify-between gap-3">
        <button type="button" onClick={onBack} className="btn btn-ghost btn-sm -ml-2.5">
          <ArrowLeft size={14} aria-hidden />
          {backLabel}
        </button>
        <ResendButton left={cooldownLeft} busy={busy === "resend"} onClick={onResend} labels={labels} />
      </div>
    </form>
  );
}
