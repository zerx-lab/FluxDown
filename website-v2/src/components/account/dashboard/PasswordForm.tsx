import { useState, type SubmitEvent } from "react";
import { accountApi, accountErrorText, accountHref } from "@/lib/account/client";
import type { AccountCodeSent, AccountMe } from "@/lib/account/types";
import type { DashboardCtx } from "./context";
import { ErrorLine, Panel, Spinner, StatusLine, SubHead, TextField } from "./ui";
import { useCooldown } from "./useCooldown";

const MIN_LENGTH = 8;
const COOLDOWN_SECONDS = 60;

type Mode = "password" | "code";
type Field = "current" | "code" | "next" | "form";

/**
 * 修改 / 设置密码。有密码:当前密码或邮箱验证码二选一;无密码:只走邮箱验证码。
 * 成功后其他设备会话被吊销,本浏览器保持登录,响应里的 `AccountMe` 替换上下文。
 */
export default function PasswordForm({ ctx }: { ctx: DashboardCtx }) {
  const { me, t, lang, guard, setMe } = ctx;
  const p = t.security.password;
  const hasPassword = me.hasPassword;

  const [preferCode, setPreferCode] = useState(false);
  const mode: Mode = !hasPassword || preferCode ? "code" : "password";
  const [current, setCurrent] = useState("");
  const [code, setCode] = useState("");
  const [next, setNext] = useState("");
  const [busy, setBusy] = useState(false);
  const [sending, setSending] = useState(false);
  const [problem, setProblem] = useState<{ field: Field; text: string } | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const cooldown = useCooldown();

  const reveal = { showLabel: t.common.showPassword, hideLabel: t.common.hidePassword };
  const fieldError = (field: Field) => (problem?.field === field ? problem.text : null);

  function fail(field: Field, text: string) {
    setProblem({ field, text });
    setStatus(null);
  }

  function switchMode() {
    setPreferCode((v) => !v);
    setProblem(null);
    setStatus(null);
  }

  async function sendCode() {
    if (sending || cooldown.left > 0) return;
    setSending(true);
    setProblem(null);
    setStatus(null);
    const res = await accountApi<AccountCodeSent>("me/password/code", { method: "POST" });
    setSending(false);
    if (!res.ok) {
      if (res.code === "rate_limited" && res.retryAfter) cooldown.start(res.retryAfter);
      if (!guard(res)) fail("code", accountErrorText(lang, res.code));
      return;
    }
    cooldown.start(COOLDOWN_SECONDS);
    setStatus(p.codeSent(me.email));
  }

  async function submit(e: SubmitEvent<HTMLFormElement>) {
    e.preventDefault();
    if (busy) return;
    if (mode === "password" && !current) return fail("current", p.needCurrent);
    if (mode === "code" && !/^\d{6}$/.test(code)) return fail("code", p.needCode);
    if (next.length < MIN_LENGTH) return fail("next", p.tooShort);

    setBusy(true);
    setProblem(null);
    setStatus(null);
    const body = mode === "password" ? { newPassword: next, currentPassword: current } : { newPassword: next, code };
    const res = await accountApi<AccountMe>("me/password", { method: "POST", body });
    setBusy(false);
    if (!res.ok) {
      if (guard(res)) return;
      const field: Field =
        res.code === "wrong_password" ? "current" : res.code === "invalid_code" ? "code" : "form";
      fail(field, accountErrorText(lang, res.code));
      return;
    }
    setCurrent("");
    setCode("");
    setNext("");
    setPreferCode(false);
    setStatus(hasPassword ? p.doneChange : p.doneSet);
    setMe(res.data);
  }

  const codeLabel = sending ? p.sending : cooldown.left > 0 ? p.resendIn(cooldown.left) : p.sendCode;

  return (
    <div className="space-y-5">
      <SubHead title={p.heading} lede={hasPassword ? p.changeDesc : p.setDesc} />
      <Panel className="p-4 sm:p-5">
        <form onSubmit={(e) => void submit(e)} noValidate className="flex max-w-[440px] flex-col gap-5">
          {/* 让密码管理器把新密码存到这个账号下;不可见、不可聚焦。 */}
          <input
            type="text"
            name="username"
            autoComplete="username"
            value={me.email}
            readOnly
            tabIndex={-1}
            aria-hidden
            className="sr-only"
          />

          {mode === "password" ? (
            <TextField
              id="sec-current"
              label={p.current}
              error={fieldError("current")}
              value={current}
              reveal={reveal}
              autoComplete="current-password"
              onChange={(e) => {
                setCurrent(e.target.value);
                setProblem(null);
              }}
            />
          ) : (
            <div>
              <TextField
                id="sec-code"
                label={p.code}
                hint={p.codeHint(me.email)}
                error={fieldError("code")}
                value={code}
                inputMode="numeric"
                autoComplete="one-time-code"
                maxLength={6}
                spellCheck={false}
                onChange={(e) => {
                  setCode(e.target.value.replace(/\D/g, "").slice(0, 6));
                  setProblem(null);
                }}
              />
              <button
                type="button"
                onClick={() => void sendCode()}
                disabled={sending || cooldown.left > 0}
                className="btn btn-secondary btn-sm mt-3 disabled:opacity-60"
              >
                {sending && <Spinner size={13} />}
                {codeLabel}
              </button>
            </div>
          )}

          <TextField
            id="sec-new"
            label={p.next}
            hint={p.nextHint}
            error={fieldError("next")}
            value={next}
            reveal={reveal}
            autoComplete="new-password"
            onChange={(e) => {
              setNext(e.target.value);
              setProblem(null);
            }}
          />

          <ErrorLine text={fieldError("form")} />

          <div className="flex flex-col gap-3">
            <div className="flex flex-wrap items-center gap-x-5 gap-y-3">
              <button type="submit" disabled={busy} className="btn btn-primary disabled:opacity-50">
                {busy && <Spinner />}
                {busy ? p.saving : hasPassword ? p.change : p.set}
              </button>
              {hasPassword && (
                <button type="button" onClick={switchMode} className="link text-[13px]">
                  {mode === "password" ? p.useCode : p.usePassword}
                </button>
              )}
            </div>
            {hasPassword && mode === "password" && (
              <a href={accountHref(lang, "forgot")} className="link w-fit text-[13px]">
                {p.forgot}
              </a>
            )}
            <StatusLine text={status} />
          </div>
        </form>
      </Panel>
    </div>
  );
}
