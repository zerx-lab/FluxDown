import { useEffect, useState, type ReactNode } from "react";
import { TriangleAlert } from "lucide-react";
import type { Lang } from "@/i18n/config";
import type { AssistantMessages } from "@/i18n/messages/assistant";
import { accountErrorText } from "@/lib/account/client";
import type { AiMessage, AiSessionKind } from "@/lib/account/types";
import type { Guard } from "@/components/account/dashboard/resource";
import { LoadFailed, Skeleton } from "@/components/account/dashboard/ui";
import { attachmentUrl, imageLimits } from "./attachments";
import Composer from "./Composer";
import Conversation from "./Conversation";
import { Greeting } from "./parts";
import { sendErrorKind, sessionLimitView } from "./sessionLimit";
import type { AssistantState } from "./useAssistant";

interface Props {
  lang: Lang;
  t: AssistantMessages;
  guard: Guard;
  s: AssistantState;
  /** 悬浮面板：更紧凑的内边距与问候。 */
  compact?: boolean;
  /** 每次「新对话」递增：已在新对话上再按一次也要把焦点送回输入框。 */
  newChatNonce: number;
  /** 开始新对话（会话达到提问上限时提示卡上的按钮）。 */
  onNewChat: () => void;
}

/** 回答完成的朗读提示保留多久（够读屏软件念完即可）。 */
const ANNOUNCE_MS = 4000;

/**
 * 对话区（整页与悬浮面板共用）：消息时间线 / 问候 / 加载与失败态 + 发送失败提示 + 输入框。
 * 重新生成与编辑重发在这里串起来：二者都带上被替换的最后一条提问（`replaceFrom`），沿用它的附图（`keepImages`）。
 */
export default function ChatArea({ lang, t, guard, s, compact = false, newChatNonce, onNewChat }: Props) {
  const [kind, setKind] = useState<AiSessionKind>("ask");
  const [prefill, setPrefill] = useState<{ text: string; nonce: number } | null>(null);
  const [editing, setEditing] = useState<AiMessage | null>(null);
  const [announce, setAnnounce] = useState(false);

  const selectedId = s.selectedId;
  useEffect(() => {
    setEditing(null);
  }, [selectedId]);

  // 回答完成：给读屏软件一条礼貌的提示（流式输出期间不朗读，免得逐字打断）
  const done = s.live?.phase === "done";
  useEffect(() => {
    if (!done) return;
    setAnnounce(true);
    const timer = setTimeout(() => setAnnounce(false), ANNOUNCE_MS);
    return () => clearTimeout(timer);
  }, [done, s.live?.runId]);

  const live = s.live && s.live.sessionId === selectedId ? s.live : null;
  const session = selectedId === null ? null : (s.detail.data?.session ?? s.sessions.items.find((i) => i.id === selectedId) ?? null);
  const readOnly = session !== null && session.channel !== "website";
  const quotaFull = s.config.dailyLimit > 0 && s.config.usedToday >= s.config.dailyLimit;
  // 单会话提问上限：只针对已存在、已加载详情的当前会话；用量以服务端详情为准，不在客户端累加
  const questionsUsed = selectedId !== null && s.detail.data?.session.id === selectedId ? s.detail.data.questionsUsed : undefined;
  const limitView = sessionLimitView(readOnly ? undefined : s.config.sessionLimit, questionsUsed, s.answering);
  const blocked = readOnly ? t.chat.readOnly : limitView.reached ? null : quotaFull && !live ? t.chat.quotaReached : null;
  const sendFailure = s.sendError;
  const sendError = sendFailure
    ? sendErrorKind(sendFailure.status, sendFailure.code) === "busy"
      ? t.chat.busy
      : accountErrorText(lang, sendFailure.code)
    : null;
  const effectiveKind = session?.kind ?? kind;
  /** 另一个会话正在回答：同一时间只允许一条流。 */
  const busyElsewhere = s.busy && !live;
  const canRevise = !readOnly && !s.busy && !s.answering && !quotaFull && !limitView.reached && s.detail.status === "ready";
  const pad = compact ? "px-4" : "px-4 sm:px-6";

  const edit = (message: AiMessage) => {
    setEditing(message);
    setPrefill({ text: message.content, nonce: Date.now() });
  };
  const regenerate = (message: AiMessage) => {
    setEditing(null);
    void s.send(message.content, effectiveKind, [], {
      replaceFrom: message.id,
      keepImages: message.images.length > 0,
      previewImages: message.images.map(attachmentUrl),
    });
  };
  const send = async (text: string, images: string[]): Promise<boolean> => {
    if (!editing) return s.send(text, kind, images);
    const ok = await s.send(text, kind, images, {
      replaceFrom: editing.id,
      keepImages: editing.images.length > 0 && images.length === 0,
      previewImages: editing.images.map(attachmentUrl),
    });
    if (ok) setEditing(null);
    return ok;
  };

  let body: ReactNode;
  if (selectedId === null && !live) {
    body = <Greeting compact={compact} t={t} kind={kind} onKind={setKind} onPick={(text) => setPrefill({ text, nonce: Date.now() })} />;
  } else if (s.detail.status === "loading" && !live) {
    body = (
      <div className={`flex-1 space-y-4 py-6 ${compact ? "px-4" : "px-6"}`} aria-busy="true">
        <Skeleton className="ml-auto h-10 w-2/3" />
        <Skeleton className="h-24 w-full" />
      </div>
    );
  } else if (s.detail.status === "error" && !live) {
    body = (
      <div className={`flex-1 py-6 ${compact ? "px-4" : "px-6"}`}>
        <LoadFailed
          text={`${t.chat.loadFailed} · ${accountErrorText(lang, s.detail.errorCode ?? "fallback")}`}
          retryLabel={t.sidebar.retry}
          onRetry={() => selectedId && void s.loadDetail(selectedId)}
        />
      </div>
    );
  } else {
    body = (
      <Conversation
        key={selectedId ?? "new"}
        lang={lang}
        t={t}
        guard={guard}
        messages={s.detail.data?.messages ?? []}
        issues={s.detail.data?.issues ?? []}
        live={live}
        notice={s.notice?.sessionId === selectedId ? s.notice : null}
        ratings={s.ratings}
        onRate={s.rate}
        onIssueChange={s.updateIssue}
        onEdit={canRevise ? edit : undefined}
        onRegenerate={canRevise ? regenerate : undefined}
      />
    );
  }

  return (
    <>
      {body}
      {/* 回答中但本页没有可挂接的流（云端还在跑 / 刚重新打开）：说明在回答，答完自然恢复 */}
      {s.answering && !live && (
        <p role="status" className={`${pad} pt-3 text-[12.5px] text-muted`}>
          {t.chat.resuming}
        </p>
      )}
      <p role="status" aria-live="polite" className="sr-only">
        {announce ? t.chat.answerDone : ""}
      </p>
      {sendError && (
        <p role="alert" className={`flex items-center gap-2 ${pad} pb-1 pt-3 text-[13px] text-danger`}>
          <TriangleAlert size={14} aria-hidden />
          {sendError}
        </p>
      )}
      <Composer
        t={t}
        placeholder={effectiveKind === "feedback" ? t.chat.placeholderFeedback : t.chat.placeholder}
        maxChars={s.config.maxMessageChars}
        streaming={s.answering}
        busyElsewhere={busyElsewhere}
        blockedReason={blocked}
        sessionLimit={limitView.reached ? { limit: s.config.sessionLimit ?? 0, onNewChat } : null}
        remaining={limitView.remaining}
        prefill={prefill}
        editing={editing !== null}
        onCancelEdit={() => setEditing(null)}
        images={imageLimits(s.config)}
        onSend={send}
        onStop={s.stop}
        focusKey={`${selectedId ?? ""}|${newChatNonce}`}
      />
    </>
  );
}
