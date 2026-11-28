import { Receipt } from "lucide-react";
import { formatMinor } from "@/components/pay/money";
import type { AccountOrder } from "@/lib/account/types";
import type { DashboardCtx } from "./context";
import { formatDateTime, lookup } from "./format";
import { Clip, EmptyState, LoadFailed, Panel, Skeleton, SubHead } from "./ui";

const STATUS_TONE: Record<string, string> = {
  paid: "text-ok",
  pending: "text-warn",
  failed: "text-danger",
};

function OrderRow({ order, ctx }: { order: AccountOrder; ctx: DashboardCtx }) {
  const { t, lang } = ctx;
  const o = t.orders;
  const notes: string[] = [];
  if (order.creditMinor > 0) notes.push(o.credit(formatMinor(order.creditMinor, order.currency, lang)));
  if (order.referralDiscountMinor > 0) notes.push(o.referral(formatMinor(order.referralDiscountMinor, order.currency, lang)));
  const when = formatDateTime(order.createdAt, lang);
  return (
    <li className="px-4 py-4 sm:px-5">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <p className="text-sm font-semibold">
            <Clip text={order.planName || order.planCode} />
          </p>
          <p className="mt-1 text-[12.5px] text-muted">
            <time dateTime={order.createdAt}>{when}</time> · {lookup(o.methods, order.payMethod)}
          </p>
        </div>
        <div className="shrink-0 text-right">
          <p className="num font-mono text-sm font-semibold">
            {formatMinor(order.amountMinor, order.currency, lang)}
          </p>
          <span className={`chip mt-1.5 ${STATUS_TONE[order.status] ?? ""}`}>{lookup(o.statuses, order.status)}</span>
        </div>
      </div>
      {notes.length > 0 && <p className="mt-2 text-[12.5px] text-muted">{notes.join(" · ")}</p>}
      <p className="mt-2 flex min-w-0 gap-2 font-mono text-[11px] text-subtle">
        <span className="shrink-0">{o.orderNo}</span>
        <Clip text={order.orderNo} className="num" />
      </p>
    </li>
  );
}

function RowSkeleton() {
  return (
    <li className="px-4 py-4 sm:px-5">
      <div className="flex justify-between gap-4">
        <div className="space-y-2">
          <Skeleton className="h-4 w-32" />
          <Skeleton className="h-3.5 w-44 max-w-full" />
        </div>
        <div className="space-y-2">
          <Skeleton className="ml-auto h-4 w-16" />
          <Skeleton className="ml-auto h-6 w-14 rounded-full" />
        </div>
      </div>
      <Skeleton className="mt-3 h-3 w-56 max-w-full" />
    </li>
  );
}

/** 订单历史:最近 20 单,倒序。 */
export default function OrdersList({ ctx }: { ctx: DashboardCtx }) {
  const { t, orders } = ctx;
  const list = orders.data;
  return (
    <div className="space-y-5">
      <SubHead title={t.orders.heading} lede={t.orders.lede} />
      {orders.status === "error" ? (
        <LoadFailed text={t.orders.loadFailed} retryLabel={t.common.retry} onRetry={orders.reload} />
      ) : list == null ? (
        <Panel>
          <ul className="divide-y divide-line" role="status" aria-busy="true" aria-label={t.common.loading}>
            <RowSkeleton />
            <RowSkeleton />
            <RowSkeleton />
          </ul>
        </Panel>
      ) : list.length === 0 ? (
        <Panel>
          <EmptyState icon={Receipt} title={t.orders.empty} desc={t.orders.emptyDesc} />
        </Panel>
      ) : (
        <Panel>
          <ul className="divide-y divide-line">
            {list.map((order) => (
              <OrderRow key={order.orderNo} order={order} ctx={ctx} />
            ))}
          </ul>
        </Panel>
      )}
    </div>
  );
}
