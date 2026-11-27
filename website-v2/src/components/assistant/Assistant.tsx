import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { ThumbsDown, ThumbsUp, Trash2, TriangleAlert } from "lucide-react";
import type { Lang } from "@/i18n/config";
import { assistant, type AssistantMessages } from "@/i18n/messages/assistant";
import { accountApi, accountErrorText, accountHref, currentPath } from "@/lib/account/client";
import type { AiConfig, AiSession, AiSessionKind } from "@/lib/account/types";
import type { Failure } from "@/components/account/dashboard/resource";
import { DisabledState } from "@/components/account/dashboard/states";
import { ErrorLine, LoadFailed, Panel, Skeleton, Spinner } from "@/components/account/dashboard/ui";
import Composer from "./Composer";
import Conversation from "./Conversation";
import { Greeting, StatePanel, UnavailableState } from "./parts";
import SessionList from "./SessionList";
import { useAssistant } from "./useAssistant";
import { imageLimits } from "./attachments";
import { useNewChatShortcut } from "./shortcuts";

type Phase =
  | { kind: "loading" }
  | { kind: "ready"; config: AiConfig }
  | { kind: "disabled" }
  | { kind: "error"; code: string };

/** 对话区顶栏：标题、今日用量、评价与删除（删除需再点一次确认）。 */
function ChatHeader({
  lang, t, session, config, onRate, onDelete,
}: {
  lang: Lang;
  t: AssistantMessages;
  session: AiSession | null;
  config: AiConfig;
  onRate: (session: AiSession, rating: "up" | "down" | null) => Promise<string | null>;
  onDelete: (id: string) => Promise<string | null>;
}) {
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const sessionId = session?.id ?? null;
  useEffect(() => {
    setConfirming(false);
    setError(null);
  }, [sessionId]);

  const run = async (action: () => Promise<string | null>) => {
    setBusy(true);
    setError(null);
    const code = await action();
    setBusy(false);
    if (code !== null) setError(accountErrorText(lang, code));
  };

  const rateButton = (value: "up" | "down") => {
    if (!session) return null;
    const pressed = session.rating === value;
    const Icon = value === "up" ? ThumbsUp : ThumbsDown;
    const label = value === "up" ? t.chat.rateUp : t.chat.rateDown;
    return (
      <button
        type="button"
        aria-pressed={pressed}
        aria-label={label}
        title={label}
        disabled={busy}
        onClick={() => void run(() => onRate(session, pressed ? null : value))}
        className={`icon-btn ${pressed ? "text-accent-ink" : ""}`}
      >
        <Icon size={15} aria-hidden className={pressed ? "fill-current" : ""} />
      </button>
    );
  };

  return (
    <div className="border-b border-line px-4 py-2.5 sm:px-6">
      <div className="flex min-h-9 items-center gap-3">
        <h2 title={session?.title} className="min-w-0 flex-1 truncate text-[14px] font-semibold">
          {session ? session.title || t.sidebar.untitled : t.sidebar.newChat}
        </h2>
        {config.dailyLimit > 0 && (
          <span className="hidden shrink-0 text-[12px] text-subtle sm:inline">{t.chat.quota(config.usedToday, config.dailyLimit)}</span>
        )}
        {session && session.messageCount > 0 && (
          <div className="flex shrink-0 items-center">
            {rateButton("up")}
            {rateButton("down")}
          </div>
        )}
        {session &&
          (confirming ? (
            <button type="button" disabled={busy} onClick={() => void run(() => onDelete(session.id))} className="btn btn-secondary btn-sm shrink-0 text-danger disabled:opacity-60">
              {busy && <Spinner size={13} />}
              {t.chat.deleteConfirm}
            </button>
          ) : (
            <button type="button" aria-label={t.chat.delete} title={t.chat.delete} onClick={() => setConfirming(true)} className="icon-btn shrink-0">
              <Trash2 size={15} aria-hidden />
            </button>
          ))}
      </div>
      <ErrorLine text={error} />
    </div>
  );
}

/** `?session=<id>`：悬浮面板「在完整页面打开」带过来的会话。 */
function sessionFromUrl(): string | null {
  if (typeof location === "undefined") return null;
  const id = new URLSearchParams(location.search).get("session");
  return id !== null && /^[A-Za-z0-9-]{1,64}$/.test(id) ? id : null;
}

function Workspace({ lang, t, guard, config }: { lang: Lang; t: AssistantMessages; guard: (f: Failure) => boolean; config: AiConfig }) {
  const s = useAssistant(lang, guard, config, sessionFromUrl());
  const [kind, setKind] = useState<AiSessionKind>("ask");
  const [prefill, setPrefill] = useState<{ text: string; nonce: number } | null>(null);
  /** 每次「新对话」递增：已在新对话上再按一次也要把焦点送回输入框。 */
  const [newChatNonce, setNewChatNonce] = useState(0);
  const newChat = () => {
    s.select(null);
    setNewChatNonce((n) => n + 1);
  };
  useNewChatShortcut(newChat, "page");

  const live = s.live && s.live.sessionId === s.selectedId ? s.live : null;
  const streaming = s.live?.phase === "streaming";
  const session = s.selectedId === null ? null : s.detail.data?.session ?? s.sessions.items.find((i) => i.id === s.selectedId) ?? null;
  const readOnly = session !== null && session.channel !== "website";
  const quotaFull = s.config.dailyLimit > 0 && s.config.usedToday >= s.config.dailyLimit;
  const blocked = readOnly ? t.chat.readOnly : quotaFull && !live ? t.chat.quotaReached : null;
  const sendError = s.sendError
    ? s.sendError.status === 409
      ? t.chat.busy
      : accountErrorText(lang, s.sendError.code)
    : null;
  const effectiveKind = session?.kind ?? kind;

  let body: ReactNode;
  if (s.selectedId === null && !live) {
    body = <Greeting t={t} kind={kind} onKind={setKind} onPick={(text) => setPrefill({ text, nonce: Date.now() })} />;
  } else if (s.detail.status === "loading" && !live) {
    body = (
      <div className="flex-1 space-y-4 px-6 py-6" aria-busy="true">
        <Skeleton className="ml-auto h-10 w-2/3" />
        <Skeleton className="h-24 w-full" />
      </div>
    );
  } else if (s.detail.status === "error" && !live) {
    body = (
      <div className="flex-1 px-6 py-6">
        <LoadFailed
          text={`${t.chat.loadFailed} · ${accountErrorText(lang, s.detail.errorCode ?? "fallback")}`}
          retryLabel={t.sidebar.retry}
          onRetry={() => s.selectedId && void s.loadDetail(s.selectedId)}
        />
      </div>
    );
  } else {
    body = (
      <Conversation
        key={s.selectedId ?? "new"}
        lang={lang}
        t={t}
        guard={guard}
        messages={s.detail.data?.messages ?? []}
        issues={s.detail.data?.issues ?? []}
        live={live}
        notice={s.notice?.sessionId === s.selectedId ? s.notice : null}
        onIssueChange={s.updateIssue}
      />
    );
  }

  return (
    <div data-ai-root="" className="grid gap-6 lg:grid-cols-[240px_minmax(0,1fr)] lg:gap-8">
      <SessionList
        lang={lang}
        t={t}
        sessions={s.sessions}
        selectedId={s.selectedId}
        streamingId={streaming ? (s.live?.sessionId ?? null) : null}
        onSelect={(id) => (id === null ? newChat() : s.select(id))}
        onRetry={() => void s.loadSessions()}
        onMore={() => void s.loadMore()}
      />
      <Panel className="flex h-[calc(100dvh-var(--header-h)-7rem)] min-h-[560px] flex-col">
        <ChatHeader lang={lang} t={t} session={session} config={s.config} onRate={s.rate} onDelete={s.remove} />
        {body}
        {sendError && (
          <p role="alert" className="flex items-center gap-2 px-4 pb-1 pt-3 text-[13px] text-danger sm:px-6">
            <TriangleAlert size={14} aria-hidden />
            {sendError}
          </p>
        )}
        <Composer
          t={t}
          placeholder={effectiveKind === "feedback" ? t.chat.placeholderFeedback : t.chat.placeholder}
          maxChars={s.config.maxMessageChars}
          streaming={live?.phase === "streaming"}
          blockedReason={blocked}
          prefill={prefill}
          images={imageLimits(s.config)}
          onSend={(text, images) => s.send(text, kind, images)}
          onStop={s.stop}
          focusKey={`${s.selectedId ?? ""}|${newChatNonce}`}
        />
      </Panel>
    </div>
  );
}

/** AI 助手页：挂载即 `GET ai/config`；401 跳登录（带回本页），403 停用说明，不可用时给原因，其余失败可重试。 */
export default function Assistant({ lang }: { lang: Lang }) {
  const t = assistant[lang];
  const [phase, setPhase] = useState<Phase>({ kind: "loading" });
  const seq = useRef(0);

  const guard = useCallback(
    (failure: Failure) => {
      if (failure.status === 401 && failure.code === "unauthorized") {
        location.replace(accountHref(lang, "login", currentPath()));
        return true;
      }
      if (failure.status === 403 && failure.code === "account_disabled") {
        setPhase({ kind: "disabled" });
        return true;
      }
      return false;
    },
    [lang],
  );

  const load = useCallback(async () => {
    const id = ++seq.current;
    setPhase({ kind: "loading" });
    const res = await accountApi<AiConfig>("ai/config");
    if (id !== seq.current) return;
    if (res.ok) setPhase({ kind: "ready", config: res.data });
    else if (!guard(res)) setPhase({ kind: "error", code: res.code });
  }, [guard]);

  useEffect(() => {
    void load();
    return () => {
      seq.current += 1;
    };
  }, [load]);

  if (phase.kind === "loading") {
    return (
      <div role="status" aria-label={t.loading} aria-busy="true" className="grid gap-6 lg:grid-cols-[240px_minmax(0,1fr)] lg:gap-8">
        <div className="space-y-2">
          <Skeleton className="h-9 w-full" />
          <Skeleton className="h-11 w-full" />
          <Skeleton className="h-11 w-full" />
        </div>
        <Skeleton className="h-[calc(100dvh-var(--header-h)-7rem)] min-h-[560px] w-full" />
      </div>
    );
  }
  if (phase.kind === "disabled") return <DisabledState lang={lang} />;
  if (phase.kind === "error") {
    return (
      <StatePanel
        icon={<TriangleAlert size={22} />}
        title={t.states.loadFailedTitle}
        desc={`${accountErrorText(lang, phase.code)} ${t.states.loadFailedDesc}`}
        action={
          <button type="button" onClick={() => void load()} className="btn btn-primary">
            {t.sidebar.retry}
          </button>
        }
      />
    );
  }
  if (!phase.config.available) return <UnavailableState lang={lang} t={t} config={phase.config} />;
  return <Workspace lang={lang} t={t} guard={guard} config={phase.config} />;
}
