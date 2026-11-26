import { LogOut } from "lucide-react";
import { PlanBadge } from "@/components/pricing/PlanBadge";
import type { DashboardCtx } from "./context";
import { firstGrapheme, formatOrdinal, textOrDash } from "./format";
import { planLabel } from "./plan";
import { Clip, ErrorLine, Spinner } from "./ui";
import { useSignOut } from "./useSignOut";

/** 顶部身份条:头像字母、昵称 / 邮箱、套餐、Origin ID、会员编号、退出登录。 */
export default function IdentityBar({ ctx }: { ctx: DashboardCtx }) {
  const { me, t, lang } = ctx;
  const signOut = useSignOut(lang);
  const nickname = textOrDash(me.nickname);
  const planName = planLabel(me, t);

  return (
    <header className="border-b border-line pb-8">
      <div className="flex flex-col gap-5 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex min-w-0 items-center gap-4">
          <span
            aria-hidden
            className="grid size-14 shrink-0 place-items-center rounded-full bg-accent-soft text-xl font-semibold text-accent-ink"
          >
            {firstGrapheme(me.nickname)}
          </span>
          <div className="min-w-0">
            <p className="text-xl font-semibold leading-tight tracking-tight">
              <Clip text={nickname} />
            </p>
            <p className="mt-1 text-sm text-muted">
              <Clip text={me.email} />
            </p>
            <div className="mt-3 flex flex-wrap items-center gap-2">
              <span className="chip font-medium text-fg">
                <Clip text={planName} className="max-w-[16ch]" />
              </span>
              {me.currentPlan && <PlanBadge plan={me.currentPlan} />}
              {me.originId != null && (
                <span className="chip num font-mono" title={t.identity.originId}>
                  ID {me.originId}
                </span>
              )}
              {me.membershipOrdinal != null && (
                <span className="chip num font-mono" title={t.identity.memberNo}>
                  {formatOrdinal(me.membershipOrdinal, me.currentPlan?.badgeNumberDigits ?? 4)}
                </span>
              )}
            </div>
          </div>
        </div>
        <button
          type="button"
          onClick={() => void signOut.run()}
          disabled={signOut.busy}
          className="btn btn-secondary btn-sm shrink-0 self-start disabled:opacity-60 sm:self-center"
        >
          {signOut.busy ? <Spinner size={14} /> : <LogOut size={14} aria-hidden />}
          {signOut.busy ? t.identity.signingOut : t.identity.signOut}
        </button>
      </div>
      <ErrorLine text={signOut.error} />
    </header>
  );
}
