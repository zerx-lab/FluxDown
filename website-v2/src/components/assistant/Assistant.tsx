import { useCallback, useEffect, useRef, useState } from "react";
import { TriangleAlert } from "lucide-react";
import type { Lang } from "@/i18n/config";
import { assistant, type AssistantMessages } from "@/i18n/messages/assistant";
import { accountApi, accountErrorText, accountHref, currentPath } from "@/lib/account/client";
import type { AiConfig } from "@/lib/account/types";
import type { Failure } from "@/components/account/dashboard/resource";
import { DisabledState } from "@/components/account/dashboard/states";
import { Panel, Skeleton } from "@/components/account/dashboard/ui";
import ChatArea from "./ChatArea";
import ChatHeader from "./ChatHeader";
import { StatePanel, UnavailableState } from "./parts";
import SessionList from "./SessionList";
import { useAssistant } from "./useAssistant";
import { useNewChatShortcut } from "./shortcuts";

type Phase =
  | { kind: "loading" }
  | { kind: "ready"; config: AiConfig }
  | { kind: "disabled" }
  | { kind: "error"; code: string };

/** `?session=<id>`：悬浮面板「在完整页面打开」带过来的会话。 */
function sessionFromUrl(): string | null {
  if (typeof location === "undefined") return null;
  const id = new URLSearchParams(location.search).get("session");
  return id !== null && /^[A-Za-z0-9-]{1,64}$/.test(id) ? id : null;
}

function Workspace({ lang, t, guard, config }: { lang: Lang; t: AssistantMessages; guard: (f: Failure) => boolean; config: AiConfig }) {
  const s = useAssistant(lang, guard, config, sessionFromUrl());
  /** 每次「新对话」递增：已在新对话上再按一次也要把焦点送回输入框。 */
  const [newChatNonce, setNewChatNonce] = useState(0);
  const newChat = () => {
    s.select(null);
    setNewChatNonce((n) => n + 1);
  };
  useNewChatShortcut(newChat, "page");

  const streaming = s.live?.phase === "streaming";
  const session = s.selectedId === null ? null : (s.detail.data?.session ?? s.sessions.items.find((i) => i.id === s.selectedId) ?? null);

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
        onSearch={s.search}
        onClearAll={s.clearAll}
      />
      <Panel className="flex h-[calc(100dvh-var(--header-h)-7rem)] min-h-[560px] flex-col">
        <ChatHeader lang={lang} t={t} session={session} config={s.config} onDelete={s.remove} onRename={s.rename} onExport={s.exportSession} />
        <ChatArea lang={lang} t={t} guard={guard} s={s} newChatNonce={newChatNonce} onNewChat={newChat} />
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
