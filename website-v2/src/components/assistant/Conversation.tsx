import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode, type TouchEvent, type WheelEvent } from "react";
import { ArrowDown, Bot, CircleAlert, CircleCheck, LoaderCircle } from "lucide-react";
import type { Lang } from "@/i18n/config";
import type { AssistantMessages } from "@/i18n/messages/assistant";
import type { AiIssue, AiMessage } from "@/lib/account/types";
import type { Guard } from "@/components/account/dashboard/resource";
import { attachmentUrl } from "./attachments";
import ImageViewer, { type ViewerImage } from "./ImageViewer";
import IssueCard from "./IssueCard";
import { toolLabel, type LiveRun, type Segment } from "./live";
import Markdown from "./Markdown";
import type { RunNotice } from "./useAssistant";

interface Props {
  lang: Lang;
  t: AssistantMessages;
  guard: Guard;
  messages: AiMessage[];
  issues: AiIssue[];
  /** 本会话进行中（或刚结束、详情尚未刷新）的提问。 */
  live: LiveRun | null;
  notice: RunNotice | null;
  onIssueChange: (issue: AiIssue) => void;
}

/** 距底部不到这么多像素即视为回到了最新位置，恢复跟随。 */
const BOTTOM_PX = 24;

/** 用户提问：图片缩略图在上（点开看大图），文字气泡在下。`images` 为可直接用作 `src` 的地址。 */
function UserBubble({ text, images, t }: { text: string; images: string[]; t: AssistantMessages }) {
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
        <p className="whitespace-pre-wrap text-[14.5px] leading-relaxed [overflow-wrap:anywhere]">{text}</p>
      </div>
      {viewing && <ImageViewer image={viewing} t={t} onClose={() => setViewing(null)} />}
    </div>
  );
}

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

function noticeText(phase: RunNotice["phase"], t: AssistantMessages): string {
  if (phase === "stopped") return t.chat.stopped;
  if (phase === "failed") return t.chat.failed;
  return t.chat.interrupted;
}

function Notice({ text }: { text: string }) {
  return (
    <p role="status" className="flex items-center gap-2 text-[13px] text-muted">
      <CircleAlert size={14} aria-hidden className="text-warn" />
      {text}
    </p>
  );
}

/**
 * 会话的消息时间线：历史消息、进行中的回答（文本 / 工具进度）与问题单卡片。
 *
 * 跟随最新：缺省新内容到达即滚到底；用户往上滚（滚动位置变小）立即停止跟随，回答照常在下方生成；
 * 不在底部时显示「回到最新」，点它或自己滚回底部即恢复跟随。新的提问开始时强制回到跟随。
 */
export default function Conversation({ lang, t, guard, messages, issues, live, notice, onIssueChange }: Props) {
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const lastTop = useRef(0);
  const [atBottom, setAtBottom] = useState(true);

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
        {messages.map((message) =>
          message.role === "user" ? (
            <UserBubble key={message.id} text={message.content} images={message.images.map(attachmentUrl)} t={t} />
          ) : (
            <AssistantTurn key={message.id} t={t}>
              <Markdown text={message.content} labels={t.chat} />
            </AssistantTurn>
          ),
        )}
        {live && (
          <>
            <UserBubble text={live.question} images={live.images} t={t} />
            <AssistantTurn t={t}>
              {live.segments.map((segment, i) =>
                segment.kind === "text" ? (
                  <Markdown key={i} text={segment.text} labels={t.chat} />
                ) : (
                  <ToolChip key={segment.callId} segment={segment} t={t} />
                ),
              )}
              {live.phase === "streaming" && !liveText && (
                <p role="status" className="flex items-center gap-2 text-[13px] text-muted">
                  <LoaderCircle size={14} aria-hidden className="animate-spin motion-reduce:animate-none" />
                  {t.chat.thinking}
                </p>
              )}
              {live.phase !== "streaming" && live.phase !== "done" && <Notice text={noticeText(live.phase, t)} />}
            </AssistantTurn>
          </>
        )}
        {!live && notice && <Notice text={noticeText(notice.phase, t)} />}
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
