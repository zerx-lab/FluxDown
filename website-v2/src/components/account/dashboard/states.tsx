import { Ban, TriangleAlert } from "lucide-react";
import type { Lang } from "@/i18n/config";
import { account } from "@/i18n/messages/account";
import { href } from "@/i18n/routing";
import { ErrorLine, Panel, Skeleton, Spinner } from "./ui";
import { useSignOut } from "./useSignOut";

/** 与最终布局同形的骨架(身份条 + 导航 + 概览格栅),加载完成时不产生布局位移。 */
export function DashboardSkeleton({ label }: { label: string }) {
  return (
    <div role="status" aria-label={label} aria-busy="true">
      <div className="flex flex-col gap-5 border-b border-line pb-8 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex items-center gap-4">
          <Skeleton className="size-14 shrink-0 rounded-full" />
          <div className="space-y-2.5">
            <Skeleton className="h-5 w-40" />
            <Skeleton className="h-3.5 w-56 max-w-full" />
            <div className="flex gap-2 pt-1">
              <Skeleton className="h-6 w-20 rounded-full" />
              <Skeleton className="h-6 w-24 rounded-full" />
              <Skeleton className="h-6 w-16 rounded-full" />
            </div>
          </div>
        </div>
        <Skeleton className="h-8 w-28 rounded-lg" />
      </div>
      <div className="mt-8 grid gap-8 lg:grid-cols-[220px_minmax(0,1fr)] lg:gap-12">
        <div className="flex gap-2 overflow-hidden lg:flex-col">
          {[0, 1, 2, 3, 4].map((i) => (
            <Skeleton key={i} className="h-9 w-28 shrink-0 lg:w-full" />
          ))}
        </div>
        <div>
          <div className="border-b border-line pb-5">
            <Skeleton className="h-7 w-36" />
            <Skeleton className="mt-3 h-4 w-72 max-w-full" />
          </div>
          <div className="cells mt-8 border border-line sm:grid-cols-2">
            {[0, 1, 2, 3].map((i) => (
              <div key={i} className="cell min-h-[148px]">
                <Skeleton className="h-3 w-20" />
                <Skeleton className="mt-6 h-8 w-32" />
                <Skeleton className="mt-4 h-3.5 w-40 max-w-full" />
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}

/** 403 `account_disabled`:说明 + 退出登录 / 联系我们。 */
export function DisabledState({ lang }: { lang: Lang }) {
  const t = account[lang];
  const signOut = useSignOut(lang);
  return (
    <Panel className="mx-auto flex max-w-[520px] flex-col items-center px-6 py-12 text-center sm:px-10">
      <span aria-hidden className="grid size-12 place-items-center rounded-full bg-inset text-danger">
        <Ban size={22} />
      </span>
      <h2 className="mt-5 text-lg font-semibold tracking-tight">{t.states.disabledTitle}</h2>
      <p className="mt-2 text-sm leading-relaxed text-muted">{t.states.disabledDesc}</p>
      <div className="mt-6 flex flex-wrap justify-center gap-3">
        <a href={href("/feedback", lang)} className="btn btn-secondary">
          {t.states.disabledContact}
        </a>
        <button type="button" onClick={() => void signOut.run()} disabled={signOut.busy} className="btn btn-primary disabled:opacity-60">
          {signOut.busy && <Spinner />}
          {signOut.busy ? t.identity.signingOut : t.identity.signOut}
        </button>
      </div>
      <ErrorLine text={signOut.error} />
    </Panel>
  );
}

/** `GET me` 失败(非 401 / 403):可重试。 */
export function LoadErrorState({ lang, message, onRetry }: { lang: Lang; message: string; onRetry: () => void }) {
  const t = account[lang];
  return (
    <Panel className="mx-auto flex max-w-[520px] flex-col items-center px-6 py-12 text-center sm:px-10">
      <div role="alert" className="flex flex-col items-center">
        <span aria-hidden className="grid size-12 place-items-center rounded-full bg-inset text-warn">
          <TriangleAlert size={22} />
        </span>
        <h2 className="mt-5 text-lg font-semibold tracking-tight">{t.states.loadFailedTitle}</h2>
        <p className="mt-2 text-sm leading-relaxed text-muted">{message}</p>
        <p className="mt-1 text-[13px] text-subtle">{t.states.loadFailedDesc}</p>
      </div>
      <button type="button" onClick={onRetry} className="btn btn-primary mt-6">
        {t.common.retry}
      </button>
    </Panel>
  );
}
