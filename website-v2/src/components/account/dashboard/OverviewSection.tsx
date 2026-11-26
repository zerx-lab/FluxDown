import { useEffect, type ReactNode } from "react";
import { ArrowUpRight, Bell, Crown, Laptop, UserRound, type LucideIcon } from "lucide-react";
import type { DashboardCtx, Section } from "./context";
import { DASH, daysUntil, formatDay, formatRelative } from "./format";
import { planLabel } from "./plan";
import { Clip, RefreshButton, SectionHead, Skeleton } from "./ui";
import { sectionLink } from "./useSection";

/** 概览格:标题行是指向对应分区的链接,用伪元素拉伸成整格可点(格内的重试按钮用 `relative` 浮在其上)。 */
function StatCell({
  ctx,
  section,
  icon: Icon,
  label,
  children,
}: {
  ctx: DashboardCtx;
  section: Section;
  icon: LucideIcon;
  label: string;
  children: ReactNode;
}) {
  return (
    <div className="cell group flex min-h-[148px] flex-col transition-colors hover:bg-sunken has-[a:focus-visible]:outline-2 has-[a:focus-visible]:-outline-offset-2 has-[a:focus-visible]:outline-accent">
      <a
        {...sectionLink(section, ctx.go)}
        className="flex items-center justify-between gap-3 font-mono text-[11.5px] uppercase tracking-[0.14em] text-muted after:absolute after:inset-0 after:content-[''] focus-visible:outline-none"
      >
        <span className="flex items-center gap-2">
          <Icon size={14} aria-hidden />
          {label}
        </span>
        <ArrowUpRight
          size={14}
          aria-hidden
          className="text-subtle transition-transform group-hover:-translate-y-px group-hover:translate-x-px motion-reduce:transition-none"
        />
      </a>
      {children}
    </div>
  );
}

function Value({ children }: { children: ReactNode }) {
  return <span className="num mt-5 block min-w-0 truncate text-3xl font-semibold leading-tight tracking-tight">{children}</span>;
}

function Sub({ children }: { children: ReactNode }) {
  return <span className="mt-2 block text-[13px] leading-relaxed text-muted">{children}</span>;
}

function CellSkeleton() {
  return (
    <>
      <Skeleton className="mt-5 h-8 w-24" />
      <Skeleton className="mt-3 h-3.5 w-36 max-w-full" />
    </>
  );
}

function CellFailed({ ctx, onRetry }: { ctx: DashboardCtx; onRetry: () => void }) {
  return (
    <>
      <Value>{DASH}</Value>
      <span className="mt-2 flex flex-wrap items-center gap-x-2 text-[13px] text-muted">
        {ctx.t.overview.loadFailed}
        <button type="button" onClick={onRetry} className="link relative">
          {ctx.t.common.retry}
        </button>
      </span>
    </>
  );
}

export default function OverviewSection({ ctx }: { ctx: DashboardCtx }) {
  const { me, t, lang, devices, notify } = ctx;
  const notifyOn = me.entitlements.notifyEnabled;

  // 概览首次展示时懒加载设备 / 推送;结果缓存在 ctx,分区切换不重复请求。
  useEffect(() => {
    devices.load();
    if (notifyOn) notify.load();
  }, [devices.load, notify.load, notifyOn]);

  const refreshing = devices.status === "loading" || devices.refreshing || notify.status === "loading" || notify.refreshing;
  const refresh = () => {
    devices.reload();
    if (notifyOn) notify.reload();
    void ctx.refreshMe();
  };

  const sub = me.activeSubscription;
  const left = sub ? daysUntil(sub.endsAt) : null;
  const deviceList = devices.data;
  const onlineCount = deviceList ? deviceList.filter((d) => d.isOnline).length : 0;
  const maxDevices = me.entitlements.maxDevices;
  const overview = notify.data;

  return (
    <section aria-labelledby="section-heading">
      <SectionHead
        title={t.overview.title}
        lede={t.overview.lede}
        actions={<RefreshButton label={t.common.refresh} busy={refreshing} onClick={refresh} />}
      />
      <div className="cells mt-8 border border-line sm:grid-cols-2">
        <StatCell ctx={ctx} section="membership" icon={Crown} label={t.overview.plan.label}>
          <Value>
            <Clip text={planLabel(me, t)} />
          </Value>
          <Sub>
            {sub ? (
              <>
                {t.overview.plan.until(formatDay(sub.endsAt, lang))}
                {left != null && <> · {left === 0 ? t.common.expiresToday : t.common.daysLeft(left)}</>}
              </>
            ) : (
              t.overview.plan.permanent
            )}
          </Sub>
        </StatCell>

        <StatCell ctx={ctx} section="devices" icon={Laptop} label={t.overview.devices.label}>
          {devices.status === "error" ? (
            <CellFailed ctx={ctx} onRetry={devices.reload} />
          ) : deviceList == null ? (
            <CellSkeleton />
          ) : (
            <>
              <Value>{deviceList.length}</Value>
              <Sub>
                {t.overview.devices.of(maxDevices)}
                <br />
                {deviceList.length > 0 && t.overview.devices.online(onlineCount, me.entitlements.maxOnlineDevices)}
              </Sub>
            </>
          )}
        </StatCell>

        <StatCell ctx={ctx} section="notifications" icon={Bell} label={t.overview.notify.label}>
          {!notifyOn || (overview && !overview.enabled) ? (
            <>
              <Value>{t.overview.notify.off}</Value>
              <Sub>{t.overview.notify.offDesc}</Sub>
            </>
          ) : notify.status === "error" ? (
            <CellFailed ctx={ctx} onRetry={notify.reload} />
          ) : overview == null ? (
            <CellSkeleton />
          ) : (
            <>
              <Value>{overview.usage.dailyUsed}</Value>
              <Sub>
                {t.overview.notify.unit}
                <br />
                {t.overview.notify.ofDay(overview.usage.dailyLimit)}
              </Sub>
            </>
          )}
        </StatCell>

        <StatCell ctx={ctx} section="security" icon={UserRound} label={t.overview.account.label}>
          <Value>{formatDay(me.createdAt, lang)}</Value>
          <Sub>
            {me.lastLoginAt ? t.overview.account.lastSignIn(formatRelative(me.lastLoginAt, lang)) : t.overview.account.never}
          </Sub>
        </StatCell>
      </div>
    </section>
  );
}
