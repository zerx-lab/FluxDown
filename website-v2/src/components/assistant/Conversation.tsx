import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode, type TouchEvent, type WheelEvent } from "react";
import { ArrowDown, Bot, Check, CircleAlert, CircleCheck, Copy, LoaderCircle, Pencil, RotateCcw } from "lucide-react";
import type { Lang } from "@/i18n/config";
import type { AssistantMessages } from "@/i18n/messages/assistant";
import type { AiIssue, AiMessage } from "@/lib/account/types";
import type { Guard } from "@/components/account/dashboard/resource";
import { attachmentUrl } from "./attachments";
import ImageViewer, { type ViewerImage } from "./ImageViewer";
import IssueCard from "./IssueCard";
import { toolLabel, type LiveRun, type Segment } from "./live";
import Markdown from "./Markdown";
import MembershipCard from "./MembershipCard";
import { splitPlaceholders } from "./placeholders";
import RateBar, { type OnRate } from "./RateBar";
import { buildTimeline, lastUserMessage } from "./timeline";
import { ratingOf, runPersisted, type Ratings, type RunNotice } from "./useAssistant";

interface Props {
  lang: Lang;
  t: AssistantMessages;
  guard: Guard;
  messages: AiMessage[];
  issues: AiIssue[];
  /** 本会话进行中（或刚结束、详情尚未刷新）的提问。 */
  live: LiveRun | null;
  notice: RunNotice | null;
  ratings: Ratings;
  /** 评价一条回答（`null` 清除；点踩后可带说明）；失败返回错误码（已回滚）。 */
  onRate: OnRate;
  onIssueChange: (issue: AiIssue) => void;
  /** 重新生成 / 编辑重发最后一条提问；undefined = 当前不可用（回答中、只读会话）。 */
  onRegenerate?: (message: AiMessage) => void;
  onEdit?: (message: AiMessage) => void;
  /** 重试失败 / 中断的轮次；undefined = 当前不可用（回答中、额度用尽、已达提问上限、没有可安全重试的方案）。 */
  onRetry?: () => void;
}

/** 距底部不到这么多像素即视为回到了最新位置，恢复跟随。 */
const BOTTOM_PX = 24;

/** 提问文字：`[Image #N]` 占位符渲染成可点开的图片引用（N 超出图片张数的当普通文字）。 */
function QuestionText({ text, images, t, onView }: { text: string; images: string[]; t: AssistantMessages; onView: (image: ViewerImage) => void }) {
  const parts = splitPlaceholders(text, images.length);
  return (
    <p className="whitespace-pre-wrap text-[14.5px] leading-relaxed [overflow-wrap:anywhere]">
      {parts.map((part, i) => {
        if (part.kind === "text") return part.text;
        const src = images[part.number - 1];
        if (src === undefined) return part.text;
        return (
          <button
            key={i}
            type="button"
            onClick={() => onView({ src, alt: t.chat.imageAlt(part.number) })}
            aria-label={`${t.chat.viewImage} · ${t.chat.imageAlt(part.number)}`}
            className="mx-0.5 inline rounded bg-accent-soft px-1.5 text-[12.5px] text-accent-ink hover:underline"
          >
            {part.text}
          </button>
        );
      })}
    </p>
  );
}

/** 用户提问：图片缩略图在上（点开看大图），文字气泡在下，`children` 是气泡下的动作。`images` 为可直接用作 `src` 的地址。 */
function UserBubble({ text, images, t, children }: { text: string; images: string[]; t: AssistantMessages; children?: ReactNode }) {
  const [viewing, setViewing] = useState<ViewerImage | null>(null);
  return (
    <div className="flex flex-col items-end gap-2">
      <p className="sr-only">{t.chat.you}</p>
      {images.length > 0 && (
        <ul className="flex max-w-[85%] flex-wrap justify-end gap-2">
          {images.map((src, i) => (
            <li key={i}>
              <button
                type="button"
                onClick={() => setViewing({ src, alt: t.chat.imageAlt(i + 1) })}
                aria-label={`${t.chat.viewImage} · ${t.chat.imageAlt(i + 1)}`}
                className="block size-24 overflow-hidden rounded-xl border border-line bg-inset"
              >
                <img src={src} alt="" loading="lazy" decoding="async" className="size-full object-cover" />
              </button>
            </li>
          ))}
        </ul>
      )}
      <div className="max-w-[85%] rounded-2xl rounded-br-md bg-inset px-4 py-2.5">
        <QuestionText text={text} images={images} t={t} onView={setViewing} />
      </div>
      {children}
      {viewing && <ImageViewer image={viewing} t={t} onClose={() => setViewing(null)} />}
    </div>
  );
}

const textBtn = "inline-flex items-center gap-1 rounded-md px-1.5 py-1 text-[12px] text-subtle transition-colors hover:text-fg";

function AssistantTurn({ t, children }: { t: AssistantMessages; children: ReactNode }) {
  return (
    <div className="flex gap-3">
      <span aria-hidden className="mt-0.5 grid size-7 shrink-0 place-items-center rounded-full bg-accent-soft text-accent-ink">
        <Bot size={15} />
      </span>
      <div className="min-w-0 flex-1 space-y-3">
        <p className="sr-only">{t.chat.assistant}</p>
        {children}
      </div>
    </div>
  );
}

function ToolChip({ segment, t }: { segment: Extract<Segment, { kind: "tool" }>; t: AssistantMessages }) {
  const label = t.chat.tools[toolLabel(segment.name)];
  return (
    <p className="flex items-center gap-2 text-[12.5px] text-muted">
      {segment.state === "running" && <LoaderCircle size={13} aria-hidden className="animate-spin motion-reduce:animate-none" />}
      {segment.state === "ok" && <CircleCheck size={13} aria-hidden className="text-ok" />}
      {segment.state === "error" && <CircleAlert size={13} aria-hidden className="text-warn" />}
      <span>
        {label}
        {segment.state === "error" && ` · ${t.chat.toolFailed}`}
      </span>
    </p>
  );
}

/** 非 done 收尾的说明：停止 / 连接中断 / 云端给出的失败原因（旧版云端不带原因，按通用失败）。 */
function noticeText(phase: RunNotice["phase"], reason: RunNotice["reason"], t: AssistantMessages): string {
  if (phase === "stopped") return t.chat.stopped;
  if (phase === "interrupted") return t.chat.interrupted;
  switch (reason) {
    case "maxTurns":
      return t.chat.maxTurns;
    case "noAnswer":
      return t.chat.noAnswer;
    case "timeout":
      return t.chat.timedOut;
    case "contextOverflow":
      return t.chat.contextOverflow;
    case "imageUnsupported":
      return t.chat.imageUnsupported;
    case "unavailable":
      return t.chat.unavailable;
    default:
      return t.chat.failed;
  }
}

function Notice({ text, retry }: { text: string; retry?: { label: string; onClick: () => void } }) {
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
      <p role="status" className="flex items-center gap-2 text-[13px] text-muted">
        <CircleAlert size={14} aria-hidden className="text-warn" />
        {text}
      </p>
      {retry && (
        <button type="button" onClick={retry.onClick} className={`${textBtn} text-accent-ink hover:text-accent-ink hover:underline`}>
          <RotateCcw size={12} aria-hidden />
          {retry.label}
        </button>
      )}
    </div>
  );
}

function Segments({ segments, t }: { segments: Segment[]; t: AssistantMessages }) {
  return segments.map((segment, i) =>
    segment.kind === "text" ? (
      <Markdown key={i} text={segment.text} labels={t.chat} />
    ) : (
      <ToolChip key={segment.callId} segment={segment} t={t} />
    ),
  );
}

/** 复制整条回答的 Markdown 原文。 */
function CopyAnswer({ text, t }: { text: string; t: AssistantMessages }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1500);
    return () => clearTimeout(timer);
  }, [copied]);
  const label = copied ? t.chat.copied : t.chat.copyAnswer;
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={() => {
        navigator.clipboard.writeText(text).then(
          () => setCopied(true),
          () => setCopied(false),
        );
      }}
      className="grid size-7 place-items-center rounded-md text-subtle transition-colors hover:text-fg"
    >
      {copied ? <Check size={14} aria-hidden /> : <Copy size={14} aria-hidden />}
    </button>
  );
}

/**
 * 会话的消息时间线：历史消息（工具前的说明文字归入「过程」）、进行中的回答（文本 / 工具进度）与问题单卡片。
 * 进行中的执行（`live.runId`）的历史消息不再画：回答期间切走再切回，云端已落库的提问 / 步骤不会和进行中的展示各出现一次。
 *
 * 跟随最新：缺省新内容到达即滚到底；用户往上滚（滚动位置变小）立即停止跟随，回答照常在下方生成；
 * 不在底部时显示「回到最新」，点它或自己滚回底部即恢复跟随。新的提问开始时强制回到跟随。
 */
export default function Conversation({
  lang, t, guard, messages, issues, live, notice, ratings, onRate, onIssueChange, onRegenerate, onEdit, onRetry,
}: Props) {
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const lastTop = useRef(0);
  const [atBottom, setAtBottom] = useState(true);
  const [commentFor, setCommentFor] = useState<string | null>(null);

  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    const bottom = el.scrollHeight - el.scrollTop - el.clientHeight < BOTTOM_PX;
    // 内容增长不触发 scroll；程序滚到底只会让位置变大。位置变小只可能是用户往上翻。
    if (bottom) stick.current = true;
    else if (el.scrollTop < lastTop.current) stick.current = false;
    lastTop.current = el.scrollTop;
    setAtBottom(bottom);
  };

  const jumpToLatest = () => {
    const el = scroller.current;
    if (!el) return;
    stick.current = true;
    el.scrollTo({ top: el.scrollHeight, behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
  };

  // 往上翻的意图要在输入事件里就截住：流式输出每帧都会重渲染，等 scroll 事件再判断会先被拉回底部
  const leaveBottom = () => {
    stick.current = false;
  };
  const onWheel = (event: WheelEvent) => {
    if (event.deltaY < 0) leaveBottom();
  };
  const onKeyDown = (event: KeyboardEvent) => {
    if (["ArrowUp", "PageUp", "Home"].includes(event.key) || (event.key === " " && event.shiftKey)) leaveBottom();
  };
  const touchY = useRef<number | null>(null);
  const onTouchMove = (event: TouchEvent) => {
    const y = event.touches[0]?.clientY ?? null;
    if (y !== null && touchY.current !== null && y > touchY.current) leaveBottom();
    touchY.current = y;
  };

  // 新的一问：无论之前翻到哪里都回到跟随
  const question = live?.question ?? null;
  useEffect(() => {
    if (question !== null) stick.current = true;
  }, [question]);

  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    if (stick.current) {
      el.scrollTop = el.scrollHeight;
      lastTop.current = el.scrollTop;
    }
    const bottom = el.scrollHeight - el.scrollTop - el.clientHeight < BOTTOM_PX;
    setAtBottom((prev) => (prev === bottom ? prev : bottom));
  });

  // 渲染之外的高度变化（图片加载完撑开、代码块折叠等）：跟随时同样贴底
  const content = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = scroller.current;
    const inner = content.current;
    if (!el || !inner) return;
    const observer = new ResizeObserver(() => {
      if (!stick.current) return;
      el.scrollTop = el.scrollHeight;
      lastTop.current = el.scrollTop;
    });
    observer.observe(inner);
    return () => observer.disconnect();
  }, []);

  const shownIssues = live ? [...issues.filter((i) => !live.issues.some((l) => l.id === i.id)), ...live.issues] : issues;
  const liveText = live?.segments.some((s) => s.kind === "text") ?? false;
  const items = buildTimeline(messages, live?.runId ?? null);
  const lastUser = live ? null : lastUserMessage(messages);
  const noticeHere = !live && notice !== null;
  const card = <MembershipCard lang={lang} t={t} guard={guard} />;
  // 失败 / 中断的轮次可重试；上下文超限与模型不识图重发仍会失败，用户停止的不算失败
  const noticeRetry =
    !live &&
    notice !== null &&
    onRetry &&
    (notice.phase === "interrupted" || (notice.phase === "failed" && notice.reason !== "contextOverflow" && notice.reason !== "imageUnsupported"))
      ? { label: t.chat.retry, onClick: onRetry }
      : undefined;

  const reviseActions = (message: AiMessage, withRegenerate: boolean): ReactNode =>
    onEdit || (withRegenerate && onRegenerate) ? (
      <div className="flex items-center gap-1">
        {onEdit && (
          <button type="button" onClick={() => onEdit(message)} className={textBtn}>
            <Pencil size={12} aria-hidden />
            {t.chat.editResend}
          </button>
        )}
        {withRegenerate && onRegenerate && (
          <button type="button" onClick={() => onRegenerate(message)} className={textBtn}>
            <RotateCcw size={12} aria-hidden />
            {t.chat.regenerate}
          </button>
        )}
      </div>
    ) : null;

  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
    <div
      ref={scroller}
      onScroll={onScroll}
      onWheel={onWheel}
      onKeyDown={onKeyDown}
      onTouchStart={(event) => (touchY.current = event.touches[0]?.clientY ?? null)}
      onTouchMove={onTouchMove}
      // 按在滚动条上（目标是滚动容器本身）准备拖动
      onPointerDown={(event) => event.target === event.currentTarget && leaveBottom()}
      className="min-h-0 flex-1 overflow-y-auto px-4 py-6 sm:px-6"
      aria-live="off"
    >
      <div ref={content} className="mx-auto flex max-w-[760px] flex-col gap-6">
        {items.map((item, index) => {
          const last = index === items.length - 1;
          if (item.kind === "user") {
            const { message } = item;
            const mine = lastUser !== null && message.id === lastUser.id;
            return (
              <UserBubble key={message.id} text={message.content} images={message.images.map(attachmentUrl)} t={t}>
                {mine && reviseActions(message, last)}
              </UserBubble>
            );
          }
          const { answer } = item;
          const reasonShown = noticeHere && notice.runId !== null && notice.runId === item.runId;
          return (
            <AssistantTurn key={item.key} t={t}>
              {item.steps.length > 0 && (
                <div className="space-y-2 border-l-2 border-line pl-3 text-muted">
                  {item.steps.map((step) => (
                    <Markdown key={step.id} text={step.content} labels={t.chat} />
                  ))}
                </div>
              )}
              {answer && <Markdown text={answer.content} labels={t.chat} />}
              {answer?.interrupted === true && !reasonShown && <Notice text={t.chat.answerInterrupted} />}
              {answer && (
                <RateBar
                  lang={lang}
                  t={t}
                  messageId={answer.id}
                  rating={ratingOf(ratings, answer.id, answer.rating)}
                  onRate={onRate}
                  commentOpen={commentFor === answer.id}
                  onCommentOpen={(open) => setCommentFor(open ? answer.id : null)}
                  actions={
                    <>
                      <CopyAnswer text={answer.content} t={t} />
                      {last && lastUser && onRegenerate && (
                        <button type="button" onClick={() => onRegenerate(lastUser)} className={textBtn}>
                          <RotateCcw size={12} aria-hidden />
                          {t.chat.regenerate}
                        </button>
                      )}
                    </>
                  }
                />
              )}
              {item.offer && card}
            </AssistantTurn>
          );
        })}
        {live && (
          <>
            <UserBubble text={live.question} images={live.images} t={t} />
            <AssistantTurn t={t}>
              <Segments segments={live.segments} t={t} />
              {live.phase === "streaming" && !liveText && (
                <p role="status" className="flex items-center gap-2 text-[13px] text-muted">
                  <LoaderCircle size={14} aria-hidden className="animate-spin motion-reduce:animate-none" />
                  {t.chat.thinking}
                </p>
              )}
              {live.phase !== "streaming" && live.phase !== "done" && <Notice text={noticeText(live.phase, live.failReason, t)} />}
              {live.phase === "done" && live.messageId !== null && (
                <RateBar
                  lang={lang}
                  t={t}
                  messageId={live.messageId}
                  rating={ratingOf(ratings, live.messageId, null)}
                  onRate={onRate}
                  commentOpen={commentFor === live.messageId}
                  onCommentOpen={(open) => setCommentFor(open && live.messageId !== null ? live.messageId : null)}
                />
              )}
              {/* 回答结束后才出现：流式输出期间不让购买弹窗开着被后续重渲染 / 详情刷新打断 */}
              {live.membershipOffer && live.phase !== "streaming" && card}
            </AssistantTurn>
          </>
        )}
        {noticeHere &&
          (runPersisted(messages, notice.runId) || (notice.segments.length === 0 && !notice.membershipOffer) ? (
            <Notice text={noticeText(notice.phase, notice.reason, t)} retry={noticeRetry} />
          ) : (
            <AssistantTurn t={t}>
              <Segments segments={notice.segments} t={t} />
              <Notice text={noticeText(notice.phase, notice.reason, t)} retry={noticeRetry} />
              {notice.membershipOffer && card}
            </AssistantTurn>
          ))}
        {shownIssues.map((issue) => (
          <IssueCard key={issue.id} issue={issue} lang={lang} t={t} guard={guard} onChange={onIssueChange} />
        ))}
      </div>
    </div>
    {!atBottom && (
      <button
        type="button"
        onClick={jumpToLatest}
        className="absolute bottom-3 left-1/2 inline-flex -translate-x-1/2 items-center gap-1.5 rounded-full border border-line-strong bg-elev px-3 py-1.5 text-[12.5px] text-muted shadow-[var(--shadow-float)] transition-colors hover:text-fg"
      >
        <ArrowDown size={13} aria-hidden />
        {live?.phase === "streaming" ? t.chat.jumpLatestLive : t.chat.jumpLatest}
      </button>
    )}
    </div>
  );
}
