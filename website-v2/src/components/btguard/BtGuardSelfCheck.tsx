/**
 * BT 防吸血自查 / 申诉：挂载后请求同源代理 `GET /api/cloud/bt-guard/self`（访客自己的来源 IP），
 * 展示是否在名单、理由与到期时间；已列入时给申诉表单（`POST /api/cloud/bt-guard/appeal`，
 * 自查响应带 `turnstileSiteKey` 时附 Turnstile，token 一次性）。契约见 src/lib/bt-guard.ts。
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { LoaderCircle } from "lucide-react";
import { withBase } from "@/lib/base";
import {
  btGuardErrorKey,
  formatExpiry,
  MAX_APPEAL_CONTACT,
  MAX_APPEAL_DETAIL,
  parseSelf,
  type BtGuardCategory,
  type BtGuardEntryAction,
  type BtGuardErrorKey,
  type BtGuardSelf,
} from "@/lib/bt-guard";
import { isRecord } from "@/lib/is-record";
import TurnstileWidget, { type TurnstileHandle } from "./TurnstileWidget";

export interface BtGuardLabels {
  check: {
    loading: string;
    retry: string;
    groupLabel: string;
    notListed: string;
    notListedDesc: string;
    listed: string;
    listedDesc: string;
    reasonLabel: string;
    actionLabel: string;
    sourceLabel: string;
    expiresLabel: string;
    actions: Record<BtGuardEntryAction, string>;
    categories: Record<BtGuardCategory, string>;
    errors: Record<BtGuardErrorKey, string>;
  };
  appeal: {
    heading: string;
    intro: string;
    pending: string;
    detailLabel: string;
    detailPlaceholder: string;
    contactLabel: string;
    contactPlaceholder: string;
    verify: string;
    submit: string;
    sending: string;
    sent: string;
    detailRequired: string;
    contactInvalid: string;
  };
}

type State = { kind: "loading" } | { kind: "ready"; self: BtGuardSelf } | { kind: "error"; key: BtGuardErrorKey };

async function errorKeyOf(response: Response): Promise<BtGuardErrorKey> {
  let code: string | undefined;
  try {
    const data: unknown = await response.json();
    if (isRecord(data) && typeof data.code === "string") code = data.code;
  } catch {
    code = undefined;
  }
  return btGuardErrorKey(response.status, code);
}

const EMAIL_RE = /^[^\s@<>]+@[^\s@<>]+\.[^\s@<>]+$/;

export default function BtGuardSelfCheck({ lang, labels }: { lang: "en" | "zh"; labels: BtGuardLabels }) {
  const [state, setState] = useState<State>({ kind: "loading" });
  const [detail, setDetail] = useState("");
  const [contact, setContact] = useState("");
  const [token, setToken] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [sent, setSent] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const widget = useRef<TurnstileHandle>(null);

  const load = useCallback(async () => {
    setState({ kind: "loading" });
    try {
      const response = await fetch(withBase("/api/cloud/bt-guard/self"), { cache: "no-store" });
      if (!response.ok) {
        setState({ kind: "error", key: await errorKeyOf(response) });
        return;
      }
      const self = parseSelf(await response.json());
      setState(self ? { kind: "ready", self } : { kind: "error", key: "generic" });
    } catch {
      setState({ kind: "error", key: "generic" });
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const siteKey = state.kind === "ready" ? state.self.turnstileSiteKey : undefined;

  const submit = async (event: { preventDefault(): void }) => {
    event.preventDefault();
    if (detail.trim() === "") {
      setError(labels.appeal.detailRequired);
      return;
    }
    if (contact.trim() !== "" && !EMAIL_RE.test(contact.trim())) {
      setError(labels.appeal.contactInvalid);
      return;
    }
    if (siteKey && !token) {
      setError(labels.check.errors.turnstileRequired);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const response = await fetch(withBase("/api/cloud/bt-guard/appeal"), {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          detail,
          ...(contact.trim() && { contact }),
          ...(token && { turnstileToken: token }),
        }),
      });
      if (response.ok) {
        setSent(true);
        return;
      }
      setError(labels.check.errors[await errorKeyOf(response)]);
    } catch {
      setError(labels.check.errors.generic);
    } finally {
      setBusy(false);
      widget.current?.reset();
    }
  };

  if (state.kind === "loading") {
    return (
      <p className="flex items-center gap-2 text-muted" role="status">
        <LoaderCircle aria-hidden className="size-4 animate-spin" />
        <span>{labels.check.loading}</span>
      </p>
    );
  }

  if (state.kind === "error") {
    return (
      <div className="flex flex-col items-start gap-3">
        <p className="text-sm text-danger" role="alert">
          {labels.check.errors[state.key]}
        </p>
        <button type="button" className="btn btn-secondary btn-sm" onClick={() => void load()}>
          {labels.check.retry}
        </button>
      </div>
    );
  }

  const { self } = state;
  const group = self.ipGroup ? (
    <p className="text-sm text-muted">
      {labels.check.groupLabel}: <span className="mono num text-fg">{self.ipGroup}</span>
    </p>
  ) : null;

  if (!self.listed) {
    return (
      <div className="flex flex-col gap-2" role="status">
        <p className="flex items-center gap-2 font-medium text-ok">
          <span className="live-dot" />
          {labels.check.notListed}
        </p>
        <p className="text-muted">{labels.check.notListedDesc}</p>
        {group}
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-8">
      <div className="flex flex-col gap-3" role="status">
        <p className="flex items-center gap-2 font-medium text-warn">
          <span className="size-1.5 rounded-full bg-warn" />
          {labels.check.listed}
        </p>
        <p className="text-muted">{labels.check.listedDesc}</p>
        {group}
        <ul className="flex flex-col gap-3">
          {self.entries.map((entry, i) => (
            <li key={i} className="cells border border-line sm:grid-cols-[2fr_1fr_1fr_1.4fr]">
              <div className="px-5 py-4">
                <span className="eyebrow-plain">{labels.check.reasonLabel}</span>
                <p className="mt-2 text-fg [overflow-wrap:anywhere]">{entry.reason}</p>
              </div>
              <div className="px-5 py-4">
                <span className="eyebrow-plain">{labels.check.actionLabel}</span>
                <p className="mt-2 text-fg">{labels.check.actions[entry.action]}</p>
              </div>
              <div className="px-5 py-4">
                <span className="eyebrow-plain">{labels.check.sourceLabel}</span>
                <p className="mt-2 text-fg">{labels.check.categories[entry.category]}</p>
              </div>
              <div className="px-5 py-4">
                <span className="eyebrow-plain">{labels.check.expiresLabel}</span>
                <p className="mono num mt-2 text-fg">
                  <time dateTime={new Date(entry.expiresAt * 1000).toISOString()}>
                    {formatExpiry(entry.expiresAt, lang)}
                  </time>
                </p>
              </div>
            </li>
          ))}
        </ul>
      </div>

      {sent ? (
        <p className="text-sm text-ok" role="status">
          {labels.appeal.sent}
        </p>
      ) : self.appealPending ? (
        <p className="text-sm text-muted" role="status">
          {labels.appeal.pending}
        </p>
      ) : (
        <form className="flex max-w-[560px] flex-col gap-4" onSubmit={(event) => void submit(event)}>
          <div className="flex flex-col gap-1">
            <h2 className="h3">{labels.appeal.heading}</h2>
            <p className="text-sm text-muted">{labels.appeal.intro}</p>
          </div>
          <label className="flex flex-col gap-1.5">
            <span className="eyebrow-plain">{labels.appeal.detailLabel}</span>
            <textarea
              className="field min-h-28"
              maxLength={MAX_APPEAL_DETAIL}
              value={detail}
              placeholder={labels.appeal.detailPlaceholder}
              required
              onChange={(event) => setDetail(event.target.value)}
            />
          </label>
          <label className="flex flex-col gap-1.5">
            <span className="eyebrow-plain">{labels.appeal.contactLabel}</span>
            <input
              className="field"
              type="email"
              maxLength={MAX_APPEAL_CONTACT}
              autoComplete="email"
              value={contact}
              placeholder={labels.appeal.contactPlaceholder}
              onChange={(event) => setContact(event.target.value)}
            />
          </label>
          {siteKey && (
            <div className="flex flex-col gap-2">
              {!token && <p className="text-sm text-muted">{labels.appeal.verify}</p>}
              <TurnstileWidget
                ref={widget}
                siteKey={siteKey}
                lang={lang}
                onToken={setToken}
                onError={() => setError(labels.check.errors.turnstileFailed)}
              />
            </div>
          )}
          {error && (
            <p className="text-sm text-danger" role="alert">
              {error}
            </p>
          )}
          <div>
            <button type="submit" className="btn btn-secondary btn-sm" disabled={busy}>
              {busy && <LoaderCircle aria-hidden className="size-3.5 animate-spin" />}
              <span>{busy ? labels.appeal.sending : labels.appeal.submit}</span>
            </button>
          </div>
        </form>
      )}
    </div>
  );
}
