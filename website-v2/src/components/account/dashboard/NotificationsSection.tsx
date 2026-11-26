import { useEffect } from "react";
import { BellOff, BellRing, Mail } from "lucide-react";
import { BrandIcon } from "@/components/icons/brand";
import type { AccountNotifyChannel, AccountNotifyOverview } from "@/lib/account/types";
import type { DashboardCtx } from "./context";
import { formatDateTime, formatRelative, lookup, textOrDash } from "./format";
import { Clip, EmptyState, LoadFailed, Panel, RefreshButton, SectionHead, Skeleton, SubHead, UsageBar } from "./ui";
import { sectionLink } from "./useSection";

function UsageRow({
  label,
  used,
  limit,
  resetAt,
  ctx,
}: {
  label: string;
  used: number;
  limit: number;
  resetAt: string;
  ctx: DashboardCtx;
}) {
  const u = ctx.t.notifications.usage;
  const text = u.used(used, limit);
  return (
    <div>
      <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1">
        <p className="text-sm font-medium">{label}</p>
        <p className="num font-mono text-[13px] text-muted">{text}</p>
      </div>
      {limit > 0 && (
        <div className="mt-2.5">
          <UsageBar value={used} max={limit} label={label} valueText={text} />
        </div>
      )}
      <p className="mt-2 text-[12.5px] text-subtle">
        <time dateTime={resetAt}>{u.resets(formatDateTime(resetAt, ctx.lang))}</time>
      </p>
    </div>
  );
}

function ChannelRow({ channel, ctx }: { channel: AccountNotifyChannel; ctx: DashboardCtx }) {
  const { t, lang } = ctx;
  const c = t.notifications.channels;
  const failing = channel.status === "failing";
  return (
    <li className="flex items-start gap-4 px-4 py-4 sm:px-5">
      <span aria-hidden className="grid size-10 shrink-0 place-items-center rounded-lg bg-inset text-muted">
        {channel.kind === "telegram" ? <BrandIcon name="telegram" size={18} /> : <Mail size={18} />}
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 flex-wrap items-center gap-x-2.5 gap-y-1.5">
          <p className="min-w-0 max-w-full text-sm font-semibold">
            <Clip text={textOrDash(channel.name)} />
          </p>
          <span className="chip">{lookup(c.kinds, channel.kind)}</span>
          <span className={`chip ${channel.enabled ? "text-ok" : ""}`}>{channel.enabled ? c.on : c.off}</span>
          {failing && <span className="chip text-danger">{c.failing}</span>}
        </div>
        <p className="mt-1.5 text-[12.5px] text-muted">
          <Clip text={textOrDash(channel.target)} className="num font-mono" />
        </p>
        <p className="mt-1 text-[12.5px] text-subtle">
          {channel.lastDeliveredAt ? (
            <span title={formatDateTime(channel.lastDeliveredAt, lang)}>
              {c.lastDelivered(formatRelative(channel.lastDeliveredAt, lang))}
            </span>
          ) : (
            c.neverDelivered
          )}
        </p>
        {failing && channel.lastError && (
          <p className="mt-2 break-words rounded-lg bg-inset px-3 py-2 text-[12.5px] leading-relaxed text-danger">
            <span className="font-medium">{c.lastError}: </span>
            {channel.lastError}
          </p>
        )}
      </div>
    </li>
  );
}

function Loading({ label }: { label: string }) {
  return (
    <div className="space-y-5" role="status" aria-busy="true" aria-label={label}>
      <Panel className="space-y-6 p-5">
        <Skeleton className="h-4 w-full max-w-[420px]" />
        <Skeleton className="h-4 w-full max-w-[420px]" />
      </Panel>
      <Panel>
        <div className="flex items-start gap-4 px-5 py-4">
          <Skeleton className="size-10 shrink-0 rounded-lg" />
          <div className="flex-1 space-y-2.5">
            <Skeleton className="h-4 w-40 max-w-full" />
            <Skeleton className="h-3.5 w-56 max-w-full" />
          </div>
        </div>
      </Panel>
    </div>
  );
}

function Ready({ data, ctx }: { data: AccountNotifyOverview; ctx: DashboardCtx }) {
  const { t } = ctx;
  const n = t.notifications;
  return (
    <div className="space-y-10">
      <div className="space-y-5">
        <SubHead title={n.usage.heading} />
        <Panel className="space-y-6 p-4 sm:p-5">
          <UsageRow label={n.usage.daily} used={data.usage.dailyUsed} limit={data.usage.dailyLimit} resetAt={data.usage.dailyResetAt} ctx={ctx} />
          <UsageRow
            label={n.usage.monthly}
            used={data.usage.monthlyUsed}
            limit={data.usage.monthlyLimit}
            resetAt={data.usage.monthlyResetAt}
            ctx={ctx}
          />
        </Panel>
      </div>

      <div className="space-y-5">
        <SubHead
          title={n.channels.heading}
          actions={<span className="num font-mono text-[12.5px] text-muted">{n.channels.count(data.channels.length, data.maxChannels)}</span>}
        />
        <Panel>
          {data.channels.length === 0 ? (
            <EmptyState icon={BellRing} title={n.channels.empty} desc={n.channels.emptyDesc} />
          ) : (
            <ul className="divide-y divide-line">
              {data.channels.map((channel) => (
                <ChannelRow key={channel.id} channel={channel} ctx={ctx} />
              ))}
            </ul>
          )}
        </Panel>
        <p className="text-[12.5px] leading-relaxed text-muted">{n.readOnly}</p>
      </div>
    </div>
  );
}

export default function NotificationsSection({ ctx }: { ctx: DashboardCtx }) {
  const { me, t, notify } = ctx;
  const n = t.notifications;
  const entitled = me.entitlements.notifyEnabled;

  useEffect(() => {
    if (entitled) notify.load();
  }, [entitled, notify.load]);

  const data = notify.data;
  const off = !entitled || (data != null && !data.enabled);

  return (
    <section aria-labelledby="section-heading">
      <SectionHead
        title={n.title}
        lede={n.lede}
        actions={
          entitled ? (
            <RefreshButton label={t.common.refresh} busy={notify.status === "loading" || notify.refreshing} onClick={notify.reload} />
          ) : undefined
        }
      />
      <div className="mt-8">
        {off ? (
          <Panel>
            <EmptyState
              icon={BellOff}
              title={n.offTitle}
              desc={n.offDesc}
              action={
                <a {...sectionLink("membership", ctx.go)} className="btn btn-secondary btn-sm">
                  {n.offAction}
                </a>
              }
            />
          </Panel>
        ) : notify.status === "error" && data == null ? (
          <LoadFailed text={n.loadFailed} retryLabel={t.common.retry} onRetry={notify.reload} />
        ) : data == null ? (
          <Loading label={t.common.loading} />
        ) : (
          <>
            {notify.errorCode && <LoadFailed text={n.loadFailed} retryLabel={t.common.retry} onRetry={notify.reload} />}
            <div className={notify.errorCode ? "mt-5" : undefined}>
              <Ready data={data} ctx={ctx} />
            </div>
          </>
        )}
      </div>
    </section>
  );
}
