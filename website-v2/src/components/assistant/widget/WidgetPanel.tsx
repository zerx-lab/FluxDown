import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { Ban, Bot, History, LogIn, Maximize2, PanelRight, PictureInPicture2, SquarePen, TriangleAlert, X } from "lucide-react";
import type { Lang } from "@/i18n/config";
import { account } from "@/i18n/messages/account";
import { assistant, type AssistantMessages } from "@/i18n/messages/assistant";
import { href } from "@/i18n/routing";
import { accountApi, accountErrorText, accountHref, currentPath, hasSignedInHint } from "@/lib/account/client";
import type { AiConfig, AiSessionKind } from "@/lib/account/types";
import type { Failure, Guard } from "@/components/account/dashboard/resource";
import { LoadFailed, Skeleton, Spinner } from "@/components/account/dashboard/ui";
import Composer from "../Composer";
import Conversation from "../Conversation";
import { Greeting, StatePanel, UnavailableState } from "../parts";
import SessionList from "../SessionList";
import { newChatShortcutLabel, useNewChatShortcut } from "../shortcuts";
import { useAssistant } from "../useAssistant";
import { imageLimits } from "../attachments";
import { readSession, writeSession, type WidgetMode } from "./storage";

type Phase =
  | { kind: "signin" }
  | { kind: "loading" }
  | { kind: "ready"; config: AiConfig }
  | { kind: "disabled" }
  | { kind: "error"; code: string };

interface Props {
  lang: Lang;
  mode: WidgetMode;
  onMode: (mode: WidgetMode) => void;
  onClose: () => void;
}

interface FrameProps {
  t: AssistantMessages;
  mode: WidgetMode;
  onMode: (mode: WidgetMode) => void;
  onClose: () => void;
  subtitle?: string | null;
  /** 顶栏左侧的会话动作（历史、新对话、完整页面）。 */
  actions?: ReactNode;
  children: ReactNode;
}

function IconButton({ label, onClick, pressed, shortcut, children }: { label: string; onClick: () => void; pressed?: boolean; shortcut?: string; children: ReactNode }) {
  return (
    <button
      type="button"
      aria-label={label}
      title={shortcut ? `${label} (${shortcut})` : label}
      aria-keyshortcuts={shortcut ? "Control+N Meta+N" : undefined}
      aria-pressed={pressed}
      onClick={onClick}
      className="ai-icon-btn"
    >
      {children}
    </button>
  );
}

/** 面板外框：标题栏（品牌 + 会话动作 + 形态切换 + 关闭）与内容区。 */
function Frame({ t, mode, onMode, onClose, subtitle, actions, children }: FrameProps) {
  return (
    <>
      <header data-drag-handle className="ai-widget-head flex h-14 shrink-0 items-center gap-2 border-b border-line pl-4 pr-2">
        <span aria-hidden className="grid size-7 shrink-0 place-items-center rounded-full bg-accent-soft text-accent-ink">
          <Bot size={15} />
        </span>
        <div className="min-w-0 flex-1">
          <h2 id="ai-widget-title" className="truncate text-[14px] font-semibold leading-tight">
            {t.widget.title}
          </h2>
          {subtitle && <p className="truncate text-[11.5px] leading-tight text-subtle">{subtitle}</p>}
        </div>
        <div className="flex shrink-0 items-center">
          {actions}
          <span className="hidden lg:contents">
            {mode === "float" ? (
              <IconButton label={t.widget.dock} onClick={() => onMode("dock")}>
                <PanelRight size={16} aria-hidden />
              </IconButton>
            ) : (
              <IconButton label={t.widget.float} onClick={() => onMode("float")}>
                <PictureInPicture2 size={16} aria-hidden />
              </IconButton>
            )}
          </span>
          <IconButton label={t.widget.close} onClick={onClose}>
            <X size={17} aria-hidden />
          </IconButton>
        </div>
      </header>
      <div className="flex min-h-0 flex-1 flex-col">{children}</div>
    </>
  );
}

/** 已登录且可用：会话动作 + 历史视图 / 对话视图 + 输入框。 */
function WidgetChat({ lang, t, guard, config, frame }: { lang: Lang; t: AssistantMessages; guard: Guard; config: AiConfig; frame: Omit<FrameProps, "children" | "actions" | "subtitle" | "t"> }) {
  const s = useAssistant(lang, guard, config, readSession());
  const [view, setView] = useState<"chat" | "history">("chat");
  const [kind, setKind] = useState<AiSessionKind>("ask");
  const [prefill, setPrefill] = useState<{ text: string; nonce: number } | null>(null);
  /** 每次「新对话」递增：已在新对话上再按一次也要把焦点送回输入框。 */
  const [newChatNonce, setNewChatNonce] = useState(0);

  useEffect(() => {
    writeSession(s.selectedId);
  }, [s.selectedId]);

  const live = s.live && s.live.sessionId === s.selectedId ? s.live : null;
  const streaming = s.live?.phase === "streaming";
  const session = s.selectedId === null ? null : s.detail.data?.session ?? s.sessions.items.find((i) => i.id === s.selectedId) ?? null;
  const readOnly = session !== null && session.channel !== "website";
  const quotaFull = s.config.dailyLimit > 0 && s.config.usedToday >= s.config.dailyLimit;
  const blocked = readOnly ? t.chat.readOnly : quotaFull && !live ? t.chat.quotaReached : null;
  const sendError = s.sendError ? (s.sendError.status === 409 ? t.chat.busy : accountErrorText(lang, s.sendError.code)) : null;
  const subtitle = view === "history" ? t.widget.history : session ? session.title || t.sidebar.untitled : s.config.dailyLimit > 0 ? t.chat.quota(s.config.usedToday, s.config.dailyLimit) : null;
  const fullPage = `${href("/assistant", lang)}${s.selectedId ? `?session=${encodeURIComponent(s.selectedId)}` : ""}`;

  const select = (id: string | null) => {
    s.select(id);
    setView("chat");
  };
  const newChat = () => {
    select(null);
    setNewChatNonce((n) => n + 1);
  };
  useNewChatShortcut(newChat, "root");

  const actions = (
    <>
      <IconButton label={view === "history" ? t.widget.backToChat : t.widget.history} pressed={view === "history"} onClick={() => setView(view === "history" ? "chat" : "history")}>
        <History size={16} aria-hidden />
      </IconButton>
      <IconButton label={t.sidebar.newChat} shortcut={newChatShortcutLabel()} onClick={newChat}>
        <SquarePen size={16} aria-hidden />
      </IconButton>
      <a href={fullPage} aria-label={t.widget.fullPage} title={t.widget.fullPage} className="ai-icon-btn">
        <Maximize2 size={15} aria-hidden />
      </a>
    </>
  );

  let body: ReactNode;
  if (view === "history") {
    body = (
      <SessionList
        lang={lang}
        t={t}
        sessions={s.sessions}
        selectedId={s.selectedId}
        streamingId={streaming ? (s.live?.sessionId ?? null) : null}
        onSelect={(id) => (id === null ? newChat() : select(id))}
        onRetry={() => void s.loadSessions()}
        onMore={() => void s.loadMore()}
        fill
      />
    );
  } else if (s.selectedId === null && !live) {
    body = <Greeting compact t={t} kind={kind} onKind={setKind} onPick={(text) => setPrefill({ text, nonce: Date.now() })} />;
  } else if (s.detail.status === "loading" && !live) {
    body = (
      <div className="flex-1 space-y-4 px-4 py-6" aria-busy="true">
        <Skeleton className="ml-auto h-10 w-2/3" />
        <Skeleton className="h-24 w-full" />
      </div>
    );
  } else if (s.detail.status === "error" && !live) {
    body = (
      <div className="flex-1 px-4 py-6">
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
    <Frame {...frame} t={t} subtitle={subtitle} actions={actions}>
      {body}
      {view === "chat" && (
        <>
          {sendError && (
            <p role="alert" className="flex items-center gap-2 px-4 pt-3 text-[13px] text-danger">
              <TriangleAlert size={14} aria-hidden />
              {sendError}
            </p>
          )}
          <Composer
            t={t}
            placeholder={(session?.kind ?? kind) === "feedback" ? t.chat.placeholderFeedback : t.chat.placeholder}
            maxChars={s.config.maxMessageChars}
            streaming={live?.phase === "streaming"}
            blockedReason={blocked}
            prefill={prefill}
            images={imageLimits(s.config)}
            onSend={(text, images) => s.send(text, kind, images)}
            onStop={s.stop}
            focusKey={`${s.selectedId ?? ""}|${newChatNonce}`}
          />
        </>
      )}
    </Frame>
  );
}

/**
 * 悬浮助手面板的内容：没有登录提示 Cookie 时直接给登录引导（不发请求）；有则 `GET ai/config`，
 * 401 回到登录引导（不整页跳转，用户在当前页决定是否去登录），403 停用说明，不可用给原因。
 */
export default function WidgetPanel({ lang, mode, onMode, onClose }: Props) {
  const t = assistant[lang];
  const [phase, setPhase] = useState<Phase>(() => (hasSignedInHint() ? { kind: "loading" } : { kind: "signin" }));
  const seq = useRef(0);
  const frame = { mode, onMode, onClose };

  const guard = useCallback((failure: Failure) => {
    if (failure.status === 401 && failure.code === "unauthorized") {
      setPhase({ kind: "signin" });
      return true;
    }
    if (failure.status === 403 && failure.code === "account_disabled") {
      setPhase({ kind: "disabled" });
      return true;
    }
    return false;
  }, []);

  const load = useCallback(async () => {
    const id = ++seq.current;
    setPhase({ kind: "loading" });
    const res = await accountApi<AiConfig>("ai/config");
    if (id !== seq.current) return;
    if (res.ok) setPhase({ kind: "ready", config: res.data });
    else if (!guard(res)) setPhase({ kind: "error", code: res.code });
  }, [guard]);

  useEffect(() => {
    if (hasSignedInHint()) void load();
    return () => {
      seq.current += 1;
    };
  }, [load]);

  if (phase.kind === "ready") {
    if (!phase.config.available) {
      return (
        <Frame {...frame} t={t}>
          <UnavailableState bare lang={lang} t={t} config={phase.config} />
        </Frame>
      );
    }
    return <WidgetChat lang={lang} t={t} guard={guard} config={phase.config} frame={frame} />;
  }

  let body: ReactNode;
  if (phase.kind === "signin") {
    const next = currentPath();
    body = (
      <StatePanel
        bare
        icon={<LogIn size={22} />}
        title={t.widget.signInTitle}
        desc={t.widget.signInDesc}
        action={
          <>
            <a href={accountHref(lang, "login", next)} className="btn btn-primary">
              {t.widget.signIn}
            </a>
            <a href={accountHref(lang, "register", next)} className="btn btn-secondary">
              {t.widget.register}
            </a>
          </>
        }
      />
    );
  } else if (phase.kind === "loading") {
    body = (
      <div role="status" aria-label={t.loading} className="flex flex-1 items-center justify-center text-muted">
        <Spinner size={18} />
      </div>
    );
  } else if (phase.kind === "disabled") {
    const a = account[lang].states;
    body = <StatePanel bare icon={<Ban size={22} />} title={a.disabledTitle} desc={a.disabledDesc} />;
  } else {
    body = (
      <StatePanel
        bare
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
  return (
    <Frame {...frame} t={t}>
      {body}
    </Frame>
  );
}
