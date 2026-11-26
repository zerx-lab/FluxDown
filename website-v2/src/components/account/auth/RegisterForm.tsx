import { useId, useRef, useState, type SubmitEvent } from "react";
import type { Lang } from "@/i18n/config";
import { accountAuth } from "@/i18n/messages/accountAuth";
import { accountApi, accountErrorText, nextAfterSignIn } from "@/lib/account/client";
import type { AccountCodeSent } from "@/lib/account/types";
import {
  CODE_LENGTH,
  NICKNAME_MAX,
  RESEND_COOLDOWN,
  authHref,
  ttlMinutes,
  useCooldown,
  useQueryParams,
  useStepFocus,
  type Failure,
} from "./hooks";
import { AgreeLine, AuthCard, ErrorLine, PasswordField, Steps, SubmitButton, TextField, VerifyStep } from "./parts";

type Step = "form" | "verify";
type Busy = "submit" | "resend" | null;

/**
 * 注册:昵称(选填)+ 邮箱 + 密码 -> `register` 发验证码 -> 输入验证码 `register/verify` -> 已登录并跳转。
 * 重新发送 = 再次 POST `register`(同参数,冷却 60s);冷却内回到表单后原样再提交直接回到验证步骤。
 * `email_taken` 时在错误行里给「去登录」链接。
 */
export default function RegisterForm({ lang }: { lang: Lang }) {
  const t = accountAuth[lang];
  const uid = useId();
  const query = useQueryParams();
  const cooldown = useCooldown();

  const [step, setStep] = useState<Step>("form");
  const [nickname, setNickname] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [code, setCode] = useState("");
  const [ttl, setTtl] = useState(0);
  const [busy, setBusy] = useState<Busy>(null);
  const [errorCode, setErrorCode] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  /** 上一次发码用的表单内容:冷却内原样再提交就不再请求。 */
  const sentKey = useRef<string | null>(null);
  const root = useStepFocus<HTMLDivElement>(step);

  const headingId = `${uid}-heading`;
  const subId = `${uid}-sub`;
  const trimmedEmail = email.trim();

  const errorText = errorCode ? accountErrorText(lang, errorCode) : null;
  const error = errorText && (
    <>
      {errorText}
      {errorCode === "email_taken" && (
        <>
          {" "}
          <a className="link" href={authHref(lang, "login", query.next, trimmedEmail)}>
            {t.register.emailTaken}
          </a>
        </>
      )}
    </>
  );

  function payload() {
    const nick = nickname.trim();
    return { email: trimmedEmail, password, ...(nick ? { nickname: nick } : {}) };
  }

  function fail(failure: Failure) {
    setErrorCode(failure.code);
    if (failure.code === "rate_limited" && failure.retryAfter) cooldown.start(failure.retryAfter);
  }

  /** 发验证码(首次提交与重发共用);成功返回 true。 */
  async function sendCode(): Promise<boolean> {
    const body = payload();
    const res = await accountApi<AccountCodeSent>("register", { body });
    if (!res.ok) {
      fail(res);
      return false;
    }
    sentKey.current = JSON.stringify(body);
    setTtl(res.data.ttlSeconds);
    cooldown.start(RESEND_COOLDOWN);
    return true;
  }

  async function submitDetails(event: SubmitEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy) return;
    setErrorCode(null);
    setNotice(null);
    if (cooldown.left > 0 && sentKey.current === JSON.stringify(payload())) {
      setStep("verify");
      return;
    }
    setBusy("submit");
    const sent = await sendCode();
    setBusy(null);
    if (sent) {
      setCode("");
      setStep("verify");
    }
  }

  async function resend() {
    if (busy || cooldown.left > 0) return;
    setBusy("resend");
    setErrorCode(null);
    setNotice(null);
    const sent = await sendCode();
    setBusy(null);
    if (sent) setNotice(t.form.resent);
  }

  async function submitVerify(event: SubmitEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy || code.length !== CODE_LENGTH) return;
    setBusy("submit");
    setErrorCode(null);
    const res = await accountApi<{ status: "ok" }>("register/verify", { body: { email: trimmedEmail, code } });
    setBusy(null);
    if (!res.ok) return fail(res);
    setDone(true);
    location.assign(nextAfterSignIn(lang));
  }

  const footer = (
    <>
      <p>
        {t.register.haveAccount}{" "}
        <a className="link font-medium" href={authHref(lang, "login", query.next)}>
          {t.register.signIn}
        </a>
      </p>
      <AgreeLine lang={lang} labels={t.register.agree} />
    </>
  );

  if (step === "verify") {
    return (
      <div ref={root}>
        <AuthCard
          bar={t.register.bar}
          heading={t.register.verify.heading}
          headingId={headingId}
          sub={`${t.register.verify.desc(trimmedEmail)} ${t.form.ttl(ttlMinutes(ttl))}`}
          subId={subId}
          footer={footer}
        >
          <Steps label={t.register.stepsLabel} steps={t.register.steps} current={1} />
          <VerifyStep
            idPrefix={uid}
            describedBy={subId}
            labels={t.form}
            code={code}
            onCode={setCode}
            submitLabel={done ? t.form.redirecting : t.register.verify.submit}
            backLabel={t.register.verify.back}
            busy={done ? "submit" : busy}
            error={error}
            notice={notice}
            cooldownLeft={cooldown.left}
            onSubmit={submitVerify}
            onResend={() => void resend()}
            onBack={() => {
              setStep("form");
              setErrorCode(null);
              setNotice(null);
            }}
          />
        </AuthCard>
      </div>
    );
  }

  return (
    <div ref={root}>
      <AuthCard bar={t.register.bar} heading={t.register.heading} headingId={headingId} sub={t.register.sub} footer={footer}>
        <Steps label={t.register.stepsLabel} steps={t.register.steps} current={0} />
        <form method="post" onSubmit={submitDetails} className="flex flex-col gap-4">
          <TextField
            id={`${uid}-nickname`}
            name="nickname"
            label={t.register.nickname}
            optional={t.register.optional}
            help={t.register.nicknameHelp}
            value={nickname}
            onChange={setNickname}
            autoComplete="nickname"
            maxLength={NICKNAME_MAX}
            stepFocus
          />
          <TextField
            id={`${uid}-email`}
            name="email"
            type="email"
            label={t.form.email}
            placeholder={t.form.emailPlaceholder}
            value={email}
            onChange={setEmail}
            autoComplete="username"
            autoCapitalize="none"
            spellCheck={false}
            required
          />
          <PasswordField
            id={`${uid}-password`}
            label={t.form.password}
            value={password}
            onChange={setPassword}
            labels={t.form}
            autoComplete="new-password"
          />
          <ErrorLine>{error}</ErrorLine>
          <SubmitButton busy={busy === "submit"}>
            {t.register.submit}
          </SubmitButton>
        </form>
      </AuthCard>
    </div>
  );
}
