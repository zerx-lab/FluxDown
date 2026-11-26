import { useEffect, useState } from "react";
import { Laptop } from "lucide-react";
import Dialog from "@/components/pay/Dialog";
import { accountApi, accountErrorText } from "@/lib/account/client";
import type { AccountDevice } from "@/lib/account/types";
import type { DashboardCtx } from "./context";
import DeviceRow from "./DeviceRow";
import { textOrDash } from "./format";
import { EmptyState, ErrorLine, LoadFailed, Panel, RefreshButton, SectionHead, Skeleton, Spinner, UsageBar } from "./ui";

function RowSkeleton() {
  return (
    <li className="flex items-start gap-4 px-4 py-4 sm:px-5">
      <Skeleton className="size-10 shrink-0 rounded-lg" />
      <div className="flex-1 space-y-2.5">
        <Skeleton className="h-4 w-40 max-w-full" />
        <Skeleton className="h-3.5 w-64 max-w-full" />
      </div>
    </li>
  );
}

function RemoveDialog({
  device,
  ctx,
  onClose,
}: {
  device: AccountDevice | null;
  ctx: DashboardCtx;
  onClose: () => void;
}) {
  const { t, lang, guard, devices } = ctx;
  const d = t.devices;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    setError(null);
  }, [device]);

  async function confirm() {
    if (!device || busy) return;
    setBusy(true);
    setError(null);
    const res = await accountApi(`devices/${encodeURIComponent(device.id)}`, { method: "DELETE" });
    setBusy(false);
    if (!res.ok) {
      if (!guard(res)) setError(accountErrorText(lang, res.code));
      return;
    }
    devices.mutate((list) => list.filter((item) => item.id !== device.id));
    onClose();
  }

  return (
    <Dialog open={device != null} onClose={onClose} title={d.removeTitle} closeLabel={d.close} dismissible={!busy}>
      <p className="text-[13px] leading-relaxed text-muted">{d.removeIntro}</p>
      <p className="mt-3 min-w-0 break-words rounded-lg bg-inset px-3 py-2 text-sm font-semibold">
        {textOrDash(device?.name)}
      </p>
      <ul className="mt-4 list-disc space-y-1.5 pl-5 text-[13px] leading-relaxed text-muted">
        {d.removeEffects.map((line) => (
          <li key={line}>{line}</li>
        ))}
      </ul>
      <ErrorLine text={error} />
      <div className="mt-6 grid grid-cols-2 gap-3">
        <button type="button" onClick={onClose} disabled={busy} className="btn btn-secondary disabled:opacity-60">
          {d.cancel}
        </button>
        <button
          type="button"
          onClick={() => void confirm()}
          disabled={busy}
          className="btn btn-primary bg-danger shadow-none hover:bg-danger disabled:opacity-60"
        >
          {busy && <Spinner />}
          {d.removeConfirm}
        </button>
      </div>
    </Dialog>
  );
}

export default function DevicesSection({ ctx }: { ctx: DashboardCtx }) {
  const { me, t, devices } = ctx;
  const d = t.devices;
  const [removing, setRemoving] = useState<AccountDevice | null>(null);

  useEffect(() => {
    devices.load();
  }, [devices.load]);

  const list = devices.data;
  const max = me.entitlements.maxDevices;

  return (
    <section aria-labelledby="section-heading">
      <SectionHead
        title={d.title}
        lede={d.lede}
        actions={
          <RefreshButton
            label={t.common.refresh}
            busy={devices.status === "loading" || devices.refreshing}
            onClick={devices.reload}
          />
        }
      />

      <div className="mt-8 space-y-5">
        {list && (
          <Panel className="p-4 sm:p-5">
            <p className="text-sm font-medium">{d.quota(list.length, max)}</p>
            {max > 0 && (
              <div className="mt-3">
                <UsageBar value={list.length} max={max} label={d.title} valueText={d.quota(list.length, max)} />
              </div>
            )}
            <p className="mt-3 text-[12.5px] leading-relaxed text-muted">{d.webNote}</p>
          </Panel>
        )}

        {devices.status === "error" && list == null ? (
          <LoadFailed text={d.loadFailed} retryLabel={t.common.retry} onRetry={devices.reload} />
        ) : list == null ? (
          <Panel>
            <ul className="divide-y divide-line" role="status" aria-busy="true" aria-label={t.common.loading}>
              <RowSkeleton />
              <RowSkeleton />
            </ul>
          </Panel>
        ) : list.length === 0 ? (
          <Panel>
            <EmptyState icon={Laptop} title={d.empty} desc={d.emptyDesc} />
          </Panel>
        ) : (
          <Panel>
            <ul className="divide-y divide-line">
              {list.map((device) => (
                <DeviceRow key={device.id} device={device} ctx={ctx} onRemove={setRemoving} />
              ))}
            </ul>
          </Panel>
        )}
        {devices.errorCode && list != null && <ErrorLine text={d.loadFailed} />}
      </div>

      <RemoveDialog device={removing} ctx={ctx} onClose={() => setRemoving(null)} />
    </section>
  );
}
