import { useEffect, useId, useRef, useState, type SubmitEvent } from "react";
import { ArrowLeft, CircleCheck } from "lucide-react";
import type { Lang } from "@/i18n/config";
import { accountAuth } from "@/i18n/messages/accountAuth";
import { accountApi, accountErrorText } from "@/lib/account/client";
import type { AccountCodeSent } from "@/lib/account/types";
import {
  CODE_LENGTH,
  PASSWORD_MIN,
  RESEND_COOLDOWN,
  authHref,
  normalizeEmail,
  useCooldown,
  useQueryParams,
  useStepFocus,
  type Failure,
} from "./hooks";
import { AuthCard, CodeField, ErrorLine, Notice, PasswordField, ResendButton, Steps, SubmitButton, TextField } from "./parts";

type Step = "email" | "reset" | "done";
type Busy = "send" | "resend" | "reset" | null;

/**
 * 找回密码:邮箱 -> `password/code`(不泄露邮箱是否注册)-> 验证码 + 新密码 -> `password/reset` -> 完成。
 * 重置会吊销该账户所有会话,不自动登录:完成页给「去登录」并预填邮箱。
 * 重发 = 再次 POST `password/code`(冷却 60s);冷却内换回同一邮箱直接回到重置步骤。
 */
export default function ForgotForm({ lang }: { lang: Lang }) {
  const t = accountAuth[lang];
  const uid = useId();
  const query = useQueryParams();
  const cooldown = useCooldown();

  const [step, setStep] = useState<Step>("email");
  const [email, setEmail] = useState("");
  const [code, setCode] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [busy, setBusy] = useState<Busy>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  /** 上一次发过码的邮箱(规范化后)。 */
  const sentTo = useRef<string | null>(null);
  const root = useStepFocus<HTMLDivElement>(step);

  // `?email=` 预填(从登录页的「忘记密码」带过来)。
  useEffect(() => {
    if (query.email) setEmail((prev) => prev || query.email);
  }, [query.email]);

  const headingId = `${uid}-heading`;
  const subId = `${uid}-sub`;
  const trimmedEmail = email.trim();
  const stepIndex = step === "email" ? 0 : step === "reset" ? 1 : 2;

  function fail(failure: Failure) {
    setError(accountErrorText(lang, failure.code));
    if (failure.code === "rate_limited" && failure.retryAfter) cooldown.start(failure.retryAfter);
  }

  async function sendCode(): Promise<boolean> {
    const res = await accountApi<AccountCodeSent>("password/code", { body: { email: trimmedEmail } });
    if (!res.ok) {
      fail(res);
      return false;
    }
    sentTo.current = normalizeEmail(trimmedEmail);
    cooldown.start(RESEND_COOLDOWN);
    return true;
  }

  async function submitEmail(event: SubmitEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy) return;
    setError(null);
    setNotice(null);
    if (cooldown.left > 0 && sentTo.current === normalizeEmail(trimmedEmail)) {
      setStep("reset");
      return;
    }
    setBusy("send");
    const sent = await sendCode();
    setBusy(null);
    if (sent) {
      setCode("");
      setNewPassword("");
      setStep("reset");
    }
  }

  async function resend() {
    if (busy || cooldown.left > 0) return;
    setBusy("resend");
    setError(null);
    setNotice(null);
    const sent = await sendCode();
    setBusy(null);
    if (sent) setNotice(t.form.resent);
  }

  async function submitReset(event: SubmitEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy || code.length !== CODE_LENGTH || newPassword.length < PASSWORD_MIN) return;
    setBusy("reset");
    setError(null);
    const res = await accountApi("password/reset", { body: { email: trimmedEmail, code, newPassword } });
    setBusy(null);
    if (!res.ok) return fail(res);
    setCode("");
    setNewPassword("");
    setStep("done");
  }

  const steps = <Steps label={t.forgot.stepsLabel} steps={t.forgot.steps} current={stepIndex} />;

  if (step === "done") {
    const doneId = `${uid}-done`;
    return (
      <div ref={root}>
        <AuthCard bar={t.forgot.bar} heading={t.forgot.done.heading} headingId={headingId}>
          {steps}
          <div role="status" className="flex flex-col gap-5">
            <span className="grid size-12 place-items-center rounded-full bg-accent-soft text-accent-ink">
              <CircleCheck size={24} strokeWidth={1.75} aria-hidden />
            </span>
            <p id={doneId} className="text-[13.5px] leading-relaxed text-muted">
              {t.forgot.done.desc}
            </p>
          </div>
          <a
            href={authHref(lang, "login", query.next, trimmedEmail)}
            aria-describedby={doneId}
            data-step-focus=""
            className="btn btn-primary mt-6 w-full"
          >
            {t.forgot.done.cta}
          </a>
        </AuthCard>
      </div>
    );
  }

  if (step === "reset") {
    return (
      <div ref={root}>
        <AuthCard
          bar={t.forgot.bar}
          heading={t.forgot.reset.heading}
          headingId={headingId}
          sub={t.forgot.reset.sent(trimmedEmail)}
          subId={subId}
        >
          {steps}
          <form method="post" onSubmit={submitReset} className="flex flex-col gap-4">
            {/* 给密码管理器一个用户名,保存 / 更新对应账号的密码;对用户与读屏不可见。 */}
            <input
              type="email"
              name="username"
              autoComplete="username"
              value={trimmedEmail}
              readOnly
              tabIndex={-1}
              aria-hidden
              className="sr-only"
            />
            <CodeField
              id={`${uid}-code`}
              label={t.form.code}
              value={code}
              onChange={setCode}
              labels={t.form}
              describedBy={subId}
              stepFocus
            />
            <PasswordField
              id={`${uid}-password`}
              label={t.forgot.reset.newPassword}
              value={newPassword}
              onChange={setNewPassword}
              labels={t.form}
              autoComplete="new-password"
            />
            <ErrorLine>{error}</ErrorLine>
            <Notice text={notice} />
            <SubmitButton busy={busy === "reset"} disabled={code.length !== CODE_LENGTH}>
              {t.forgot.reset.submit}
            </SubmitButton>
            <div className="flex items-center justify-between gap-3">
              <button
                type="button"
                onClick={() => {
                  setStep("email");
                  setError(null);
                  setNotice(null);
                }}
                className="btn btn-ghost btn-sm -ml-2.5"
              >
                <ArrowLeft size={14} aria-hidden />
                {t.forgot.reset.back}
              </button>
              <ResendButton left={cooldown.left} busy={busy === "resend"} onClick={() => void resend()} labels={t.form} />
            </div>
          </form>
        </AuthCard>
      </div>
    );
  }

  return (
    <div ref={root}>
      <AuthCard
        bar={t.forgot.bar}
        heading={t.forgot.email.heading}
        headingId={headingId}
        sub={t.forgot.email.desc}
        footer={
          <p>
            <a className="link font-medium" href={authHref(lang, "login", query.next, trimmedEmail)}>
              {t.forgot.backToSignIn}
            </a>
          </p>
        }
      >
        {steps}
        <form method="post" onSubmit={submitEmail} className="flex flex-col gap-4">
          <TextField
            id={`${uid}-email`}
            name="email"
            type="email"
            label={t.form.email}
            placeholder={t.form.emailPlaceholder}
            value={email}
            onChange={setEmail}
            autoComplete="email"
            autoCapitalize="none"
            spellCheck={false}
            required
            stepFocus
          />
          <ErrorLine>{error}</ErrorLine>
          <SubmitButton busy={busy === "send"}>
            {t.forgot.email.submit}
          </SubmitButton>
        </form>
      </AuthCard>
    </div>
  );
}
