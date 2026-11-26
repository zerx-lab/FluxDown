import { useEffect, useId, useRef, useState, type SubmitEvent } from "react";
import type { Lang } from "@/i18n/config";
import { accountAuth } from "@/i18n/messages/accountAuth";
import { accountApi, accountErrorText, hasSignedInHint, nextAfterSignIn } from "@/lib/account/client";
import type { AccountCodeSent, AccountLoginResult, AccountMe } from "@/lib/account/types";
import {
  CODE_LENGTH,
  RESEND_COOLDOWN,
  authHref,
  normalizeEmail,
  ttlMinutes,
  useCooldown,
  useQueryParams,
  useStepFocus,
  type Failure,
} from "./hooks";
import {
  AgreeLine,
  AuthCard,
  CodeField,
  ErrorLine,
  Notice,
  PasswordField,
  StatusPanel,
  SubmitButton,
  TextField,
  VerifyStep,
} from "./parts";
import Tabs, { panelId, tabId } from "./Tabs";

type Phase = "checking" | "form" | "verify" | "done";
type Mode = "password" | "code";
type Busy = "submit" | "send" | "resend" | null;

/**
 * 登录:密码登录 / 验证码登录两个 tab;新浏览器密码登录会进入「验证新浏览器」步骤。
 *
 * 状态机:
 *   checking  带登录态提示 Cookie 时先 GET me,有效直接跳走,失效回到 form
 *   form      mode = password | code
 *     password --login--> ok: 跳转 | verify: -> verify
 *     code     --code/send (冷却 60s)--> 填码 --code/login--> 跳转
 *   verify    --login/verify--> 跳转;重新发送 = 再次 POST login(冷却 60s);返回 -> form
 *   done      已写入会话,正在跳转
 */
export default function LoginForm({ lang }: { lang: Lang }) {
  const t = accountAuth[lang];
  const uid = useId();
  const query = useQueryParams();
  const cooldown = useCooldown();

  const [phase, setPhase] = useState<Phase>("form");
  const [mode, setMode] = useState<Mode>("password");
  const [account, setAccount] = useState("");
  const [password, setPassword] = useState("");
  const [email, setEmail] = useState("");
  const [code, setCode] = useState("");
  const [sentTo, setSentTo] = useState<string | null>(null);
  const [verifyCode, setVerifyCode] = useState("");
  const [ttl, setTtl] = useState(0);
  const [busy, setBusy] = useState<Busy>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const passwordRef = useRef<HTMLInputElement>(null);
  const emailRef = useRef<HTMLInputElement>(null);
  const codeRef = useRef<HTMLInputElement>(null);
  /** 上一次触发「新浏览器验证码」的账号 + 密码:冷却内原样再提交就直接回到验证步骤,不再请求。 */
  const verifySent = useRef<string | null>(null);
  const root = useStepFocus<HTMLDivElement>(phase === "verify" ? "verify" : "form");

  // 从「重置密码完成」页带着邮箱回来:预填账号并直接去填密码。
  useEffect(() => {
    if (!query.email) return;
    setAccount((prev) => prev || query.email);
    passwordRef.current?.focus();
  }, [query.email]);

  // 已登录就不要再给登录表单:先确认会话,有效直接去 next。
  useEffect(() => {
    if (!hasSignedInHint()) return;
    let cancelled = false;
    setPhase("checking");
    void accountApi<AccountMe>("me").then((res) => {
      if (cancelled) return;
      if (res.ok) {
        setPhase("done");
        location.replace(nextAfterSignIn(lang));
      } else {
        setPhase("form");
      }
    });
    return () => {
      cancelled = true;
    };
  }, [lang]);

  const headingId = `${uid}-heading`;
  const subId = `${uid}-sub`;
  const tabsPrefix = `${uid}-mode`;
  const emailKey = normalizeEmail(email);
  /** 验证码登录的冷却只对刚发过码的那个邮箱生效,改了邮箱就可以重新发。 */
  const codeCooling = cooldown.left > 0 && sentTo === emailKey;

  function fail(failure: Failure, withCooldown = false) {
    setError(accountErrorText(lang, failure.code));
    if (withCooldown && failure.code === "rate_limited" && failure.retryAfter) cooldown.start(failure.retryAfter);
  }

  function finish() {
    setPhase("done");
    location.assign(nextAfterSignIn(lang));
  }

  function switchMode(next: Mode) {
    setMode(next);
    setError(null);
    setNotice(null);
  }

  function enterVerify() {
    setVerifyCode("");
    setError(null);
    setNotice(null);
    setPhase("verify");
  }

  async function requestVerify(): Promise<boolean> {
    const res = await accountApi<AccountLoginResult>("login", { body: { account: account.trim(), password } });
    if (!res.ok) {
      fail(res, true);
      return false;
    }
    if (res.data.status === "ok") {
      finish();
      return false;
    }
    verifySent.current = `${account.trim()}\n${password}`;
    setTtl(res.data.ttlSeconds);
    cooldown.start(RESEND_COOLDOWN);
    return true;
  }

  async function submitPassword(event: SubmitEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy) return;
    if (cooldown.left > 0 && verifySent.current === `${account.trim()}\n${password}`) {
      enterVerify();
      return;
    }
    setBusy("submit");
    setError(null);
    const needVerify = await requestVerify();
    setBusy(null);
    if (needVerify) enterVerify();
  }

  async function resendVerify() {
    if (busy || cooldown.left > 0) return;
    setBusy("resend");
    setError(null);
    setNotice(null);
    const sent = await requestVerify();
    setBusy(null);
    if (sent) setNotice(t.form.resent);
  }

  async function submitVerify(event: SubmitEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy || verifyCode.length !== CODE_LENGTH) return;
    setBusy("submit");
    setError(null);
    const res = await accountApi<{ status: "ok" }>("login/verify", {
      body: { account: account.trim(), password, code: verifyCode },
    });
    setBusy(null);
    if (!res.ok) return fail(res);
    finish();
  }

  async function sendCode() {
    if (busy || codeCooling) return;
    if (emailRef.current && !emailRef.current.reportValidity()) return;
    const target = email.trim();
    setBusy("send");
    setError(null);
    setNotice(null);
    const res = await accountApi<AccountCodeSent>("code/send", { body: { email: target } });
    setBusy(null);
    if (!res.ok) {
      if (res.code === "rate_limited") setSentTo(emailKey);
      return fail(res, true);
    }
    setSentTo(emailKey);
    cooldown.start(RESEND_COOLDOWN);
    setNotice(`${t.login.codeSentTo(target)} ${t.form.ttl(ttlMinutes(res.data.ttlSeconds))}`);
    codeRef.current?.focus();
  }

  async function submitCode(event: SubmitEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy || code.length !== CODE_LENGTH) return;
    setBusy("submit");
    setError(null);
    const res = await accountApi<{ status: "ok" }>("code/login", { body: { email: email.trim(), code } });
    setBusy(null);
    if (!res.ok) return fail(res);
    finish();
  }

  if (phase === "checking" || phase === "done") {
    return (
      <AuthCard bar={t.login.bar} heading={t.login.heading} headingId={headingId}>
        <StatusPanel text={phase === "checking" ? t.form.checking : t.form.redirecting} />
      </AuthCard>
    );
  }

  if (phase === "verify") {
    return (
      <div ref={root}>
        <AuthCard
          bar={t.login.bar}
          heading={t.login.verify.heading}
          headingId={headingId}
          sub={`${t.login.verify.desc} ${t.form.ttl(ttlMinutes(ttl))}`}
          subId={subId}
        >
          <VerifyStep
            idPrefix={uid}
            describedBy={subId}
            labels={t.form}
            code={verifyCode}
            onCode={setVerifyCode}
            submitLabel={t.login.verify.submit}
            backLabel={t.login.verify.back}
            busy={busy === "send" ? null : busy}
            error={error}
            notice={notice}
            cooldownLeft={cooldown.left}
            onSubmit={submitVerify}
            onResend={() => void resendVerify()}
            onBack={() => {
              setPhase("form");
              setError(null);
              setNotice(null);
            }}
          />
        </AuthCard>
      </div>
    );
  }

  const forgotEmail = account.includes("@") ? account : null;

  return (
    <div ref={root}>
      <AuthCard
        bar={t.login.bar}
        heading={t.login.heading}
        headingId={headingId}
        sub={t.login.sub}
        footer={
          <>
            <p>
              {t.login.noAccount}{" "}
              <a className="link font-medium" href={authHref(lang, "register", query.next)}>
                {t.login.register}
              </a>
            </p>
            <AgreeLine lang={lang} labels={t.login.agree} />
          </>
        }
      >
        <Tabs<Mode>
          label={t.login.tabsLabel}
          idPrefix={tabsPrefix}
          value={mode}
          onChange={switchMode}
          items={[
            { id: "password", label: t.login.tabPassword },
            { id: "code", label: t.login.tabCode },
          ]}
        />

        <div
          role="tabpanel"
          id={panelId(tabsPrefix, "password")}
          aria-labelledby={tabId(tabsPrefix, "password")}
          hidden={mode !== "password"}
          className="mt-5"
        >
          <form method="post" onSubmit={submitPassword} className="flex flex-col gap-4">
            <TextField
              id={`${uid}-account`}
              name="username"
              label={t.login.account}
              placeholder={t.login.accountPlaceholder}
              value={account}
              onChange={setAccount}
              autoComplete="username"
              autoCapitalize="none"
              autoCorrect="off"
              spellCheck={false}
              required
              stepFocus
            />
            <PasswordField
              id={`${uid}-password`}
              ref={passwordRef}
              label={t.form.password}
              value={password}
              onChange={setPassword}
              labels={t.form}
              autoComplete="current-password"
              action={
                <a className="link text-[12.5px]" href={authHref(lang, "forgot", query.next, forgotEmail)}>
                  {t.login.forgot}
                </a>
              }
            />
            <ErrorLine>{mode === "password" ? error : null}</ErrorLine>
            <SubmitButton busy={busy === "submit" && mode === "password"}>
              {t.login.submit}
            </SubmitButton>
          </form>
        </div>

        <div
          role="tabpanel"
          id={panelId(tabsPrefix, "code")}
          aria-labelledby={tabId(tabsPrefix, "code")}
          hidden={mode !== "code"}
          className="mt-5"
        >
          <form method="post" onSubmit={submitCode} className="flex flex-col gap-4">
            <TextField
              id={`${uid}-email`}
              ref={emailRef}
              name="email"
              type="email"
              label={t.form.email}
              placeholder={t.form.emailPlaceholder}
              help={t.login.autoCreate}
              value={email}
              onChange={setEmail}
              autoComplete="email"
              autoCapitalize="none"
              spellCheck={false}
              required
              stepFocus
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  void sendCode();
                }
              }}
            />
            <CodeField
              id={`${uid}-code`}
              ref={codeRef}
              label={t.form.code}
              value={code}
              onChange={setCode}
              labels={t.form}
              action={
                <button
                  type="button"
                  onClick={() => void sendCode()}
                  disabled={busy !== null || codeCooling}
                  className="btn btn-secondary shrink-0 disabled:cursor-not-allowed disabled:opacity-50"
                >
                  <span className="num">
                    {codeCooling ? t.form.sendIn(cooldown.left) : sentTo === emailKey ? t.form.resend : t.form.sendCode}
                  </span>
                </button>
              }
            />
            <ErrorLine>{mode === "code" ? error : null}</ErrorLine>
            <Notice text={mode === "code" ? notice : null} />
            <SubmitButton busy={busy === "submit" && mode === "code"} disabled={code.length !== CODE_LENGTH}>
              {t.login.submit}
            </SubmitButton>
          </form>
        </div>
      </AuthCard>
    </div>
  );
}
