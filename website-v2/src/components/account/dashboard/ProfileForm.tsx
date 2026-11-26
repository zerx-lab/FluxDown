import { useState, type SubmitEvent } from "react";
import { accountApi, accountErrorText } from "@/lib/account/client";
import type { AccountMe } from "@/lib/account/types";
import type { DashboardCtx } from "./context";
import { charCount, textOrDash } from "./format";
import { Clip, Panel, Spinner, StatusLine, SubHead, TextField } from "./ui";

const NICKNAME_MAX = 32;

/** 昵称修改;成功后用响应里的 `AccountMe` 替换上下文。 */
export default function ProfileForm({ ctx }: { ctx: DashboardCtx }) {
  const { me, t, lang, guard, setMe } = ctx;
  const p = t.security.profile;
  const [nickname, setNickname] = useState(me.nickname);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  const trimmed = nickname.trim();
  const unchanged = trimmed === me.nickname;

  async function submit(e: SubmitEvent<HTMLFormElement>) {
    e.preventDefault();
    if (busy || unchanged) return;
    const length = charCount(trimmed);
    if (length < 1 || length > NICKNAME_MAX) {
      setError(p.nicknameInvalid);
      setSaved(false);
      return;
    }
    setBusy(true);
    setError(null);
    setSaved(false);
    const res = await accountApi<AccountMe>("me/nickname", { method: "PUT", body: { nickname: trimmed } });
    setBusy(false);
    if (!res.ok) {
      if (!guard(res)) setError(accountErrorText(lang, res.code));
      return;
    }
    setMe(res.data);
    setNickname(res.data.nickname);
    setSaved(true);
  }

  return (
    <div className="space-y-5">
      <SubHead title={p.heading} />
      <Panel className="p-4 sm:p-5">
        <form onSubmit={(e) => void submit(e)} noValidate className="flex max-w-[440px] flex-col gap-5">
          <TextField
            id="sec-nickname"
            label={p.nickname}
            hint={p.nicknameHint}
            error={error}
            value={nickname}
            autoComplete="nickname"
            onChange={(e) => {
              setNickname(e.target.value);
              setError(null);
              setSaved(false);
            }}
          />
          <div>
            <p className="mb-2 text-[13px] font-medium">{p.email}</p>
            <p className="text-sm text-muted">
              <Clip text={textOrDash(me.email)} />
            </p>
          </div>
          <div>
            <button type="submit" disabled={busy || unchanged} className="btn btn-primary disabled:opacity-50">
              {busy && <Spinner />}
              {busy ? p.saving : p.save}
            </button>
            <StatusLine text={saved ? p.saved : null} />
          </div>
        </form>
      </Panel>
    </div>
  );
}
