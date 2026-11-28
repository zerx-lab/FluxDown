import { useEffect, useRef, useState } from "react";
import { Monitor, Pencil, Trash2 } from "lucide-react";
import { BrandIcon, type BrandName } from "@/components/icons/brand";
import { accountApi, accountErrorText } from "@/lib/account/client";
import type { AccountDevice } from "@/lib/account/types";
import type { DashboardCtx } from "./context";
import { charCount, formatDateTime, formatRelative, lookup, textOrDash } from "./format";
import { Clip, Spinner, TextField } from "./ui";

const PLATFORM_ICON: Record<string, BrandName> = {
  windows: "windows",
  macos: "apple",
  ios: "apple",
  linux: "linux",
  android: "android",
};

const NAME_MAX = 64;

interface Props {
  device: AccountDevice;
  ctx: DashboardCtx;
  onRemove: (device: AccountDevice) => void;
}

export default function DeviceRow({ device, ctx, onRemove }: Props) {
  const { t, lang, guard, devices } = ctx;
  const d = t.devices;
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(device.name);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const renameBtn = useRef<HTMLButtonElement>(null);
  const wasEditing = useRef(false);

  // 关闭编辑态后把焦点还给「改名」按钮(键盘用户不丢位置)。
  useEffect(() => {
    if (wasEditing.current && !editing) renameBtn.current?.focus();
    wasEditing.current = editing;
  }, [editing]);

  const icon = PLATFORM_ICON[device.platform ?? ""];
  const platform = device.platform ? lookup(d.platforms, device.platform) : null;
  const name = textOrDash(device.name);

  function startEdit() {
    setDraft(device.name);
    setError(null);
    setEditing(true);
  }

  async function save() {
    const next = draft.trim();
    const length = charCount(next);
    if (length < 1 || length > NAME_MAX) {
      setError(d.renameInvalid);
      return;
    }
    if (next === device.name) {
      setEditing(false);
      return;
    }
    setBusy(true);
    setError(null);
    const res = await accountApi<AccountDevice>(`devices/${encodeURIComponent(device.id)}`, {
      method: "PATCH",
      body: { name: next },
    });
    setBusy(false);
    if (!res.ok) {
      if (!guard(res)) setError(accountErrorText(lang, res.code));
      return;
    }
    devices.mutate((list) => list.map((item) => (item.id === device.id ? res.data : item)));
    setEditing(false);
  }

  return (
    <li className="flex flex-col gap-4 px-4 py-4 sm:flex-row sm:items-start sm:justify-between sm:px-5">
      <div className="flex min-w-0 flex-1 items-start gap-4">
        <span aria-hidden className="grid size-10 shrink-0 place-items-center rounded-lg bg-inset text-muted">
          {icon ? <BrandIcon name={icon} size={18} /> : <Monitor size={18} />}
        </span>
        <div className="min-w-0 flex-1">
          {editing ? (
            <form
              onSubmit={(e) => {
                e.preventDefault();
                void save();
              }}
              onKeyDown={(e) => {
                if (e.key === "Escape" && !busy && !e.nativeEvent.isComposing) {
                  e.stopPropagation();
                  setEditing(false);
                }
              }}
            >
              <TextField
                id={`device-name-${device.id}`}
                label={d.renameLabel}
                hint={d.renameHint}
                error={error}
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                autoComplete="off"
                autoFocus
                enterKeyHint="done"
                spellCheck={false}
              />
              <div className="mt-3 flex gap-2">
                <button type="submit" disabled={busy || !draft.trim()} className="btn btn-primary btn-sm disabled:opacity-60">
                  {busy && <Spinner size={14} />}
                  {d.renameSave}
                </button>
                <button type="button" onClick={() => setEditing(false)} disabled={busy} className="btn btn-ghost btn-sm">
                  {d.renameCancel}
                </button>
              </div>
            </form>
          ) : (
            <>
              <div className="flex min-w-0 flex-wrap items-center gap-x-2.5 gap-y-1">
                <p className="min-w-0 max-w-full text-sm font-semibold">
                  <Clip text={name} />
                </p>
                {device.isOnline ? (
                  <span className="inline-flex items-center gap-1.5 text-xs text-ok">
                    <span aria-hidden className="live-dot motion-reduce:animate-none" />
                    {d.online}
                  </span>
                ) : (
                  <span className="inline-flex items-center gap-1.5 text-xs text-subtle">
                    <span aria-hidden className="size-1.5 rounded-full bg-line-strong" />
                    {d.offline}
                  </span>
                )}
              </div>
              <p className="mt-1.5 flex flex-wrap gap-x-3 gap-y-0.5 text-[12.5px] text-muted">
                {platform && <span>{platform}</span>}
                {device.appVersion && <span className="num font-mono">{d.version(device.appVersion)}</span>}
                <span title={formatDateTime(device.lastSeenAt, lang)}>{d.lastSeen(formatRelative(device.lastSeenAt, lang))}</span>
                {device.lastIp && (
                  <span>
                    {d.lastIp} <span className="num font-mono">{device.lastIp}</span>
                  </span>
                )}
              </p>
            </>
          )}
        </div>
      </div>
      {!editing && (
        <div className="flex shrink-0 gap-2 pl-14 sm:pl-0">
          <button ref={renameBtn} type="button" onClick={startEdit} className="btn btn-secondary btn-sm">
            <Pencil size={13} aria-hidden />
            {d.rename}
            <span className="sr-only"> {name}</span>
          </button>
          <button type="button" onClick={() => onRemove(device)} className="btn btn-secondary btn-sm text-danger">
            <Trash2 size={13} aria-hidden />
            {d.remove}
            <span className="sr-only"> {name}</span>
          </button>
        </div>
      )}
    </li>
  );
}
