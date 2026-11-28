import { useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { ThumbsDown, ThumbsUp } from "lucide-react";
import type { Lang } from "@/i18n/config";
import type { AssistantMessages } from "@/i18n/messages/assistant";
import { accountErrorText } from "@/lib/account/client";
import type { AiRating } from "@/lib/account/types";
import { charCount } from "@/components/account/dashboard/format";

/** 点踩说明的字符上限（FluxCloud 与 BFF `MAX_RATING_COMMENT_CHARS`）。 */
const MAX_COMMENT_CHARS = 500;
/** 「谢谢反馈」提示停留多久后收起。 */
const THANKS_MS = 2500;

/** 评价一条回答；`comment` 是点踩后的可选说明。失败返回错误码（已回滚）。 */
export type OnRate = (messageId: string, rating: AiRating | null, comment?: string) => Promise<string | null>;

interface CommentProps {
  lang: Lang;
  t: AssistantMessages;
  messageId: string;
  onRate: OnRate;
  onClose: () => void;
}

/** 点踩后的可选说明：提交走同一条评价接口（`rating: "down"` + `comment`），跳过或提交成功后收起。 */
function RateComment({ lang, t, messageId, onRate, onClose }: CommentProps) {
  const id = useId();
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const [thanked, setThanked] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  // 提示停留一会儿再收起；`onClose` 每次渲染都是新函数，用 ref 读，免得上层重渲染把计时重置
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  useEffect(() => {
    if (!thanked) return;
    const timer = setTimeout(() => closeRef.current(), THANKS_MS);
    return () => clearTimeout(timer);
  }, [thanked]);

  const count = charCount(text);
  const tooLong = count > MAX_COMMENT_CHARS;
  const canSend = !sending && text.trim() !== "" && !tooLong;

  const submit = async () => {
    if (!canSend) return;
    setSending(true);
    setError(null);
    const code = await onRate(messageId, "down", text);
    if (!mounted.current) return;
    setSending(false);
    if (code !== null) setError(accountErrorText(lang, code));
    else setThanked(true);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    // Esc 只收起说明，不冒泡到悬浮面板（面板的 Esc 会收起整个助手）
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      onClose();
      return;
    }
    // Safari 结束输入法组字的那次 Enter 报 `isComposing: false`，只能靠 keyCode 229 识别。
    if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing || event.keyCode === 229) return;
    event.preventDefault();
    void submit();
  };

  if (thanked) {
    return (
      <p role="status" className="mt-2 text-[12.5px] text-muted">
        {t.chat.rateCommentThanks}
      </p>
    );
  }

  return (
    <form
      className="mt-2 max-w-[520px] space-y-2 rounded-xl border border-line bg-inset p-3"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <label htmlFor={id} className="block text-[12.5px] text-muted">
        {t.chat.rateCommentPrompt}
      </label>
      <textarea
        id={id}
        rows={2}
        value={text}
        disabled={sending}
        aria-invalid={tooLong || undefined}
        onChange={(event) => setText(event.target.value)}
        onKeyDown={onKeyDown}
        className="block max-h-40 min-h-[3.25rem] w-full resize-none rounded-lg border border-line bg-elev px-3 py-2 text-[13.5px] leading-relaxed outline-none [field-sizing:content] focus:border-line-strong"
      />
      <div className="flex items-center justify-between gap-3">
        <span className={`text-[12px] ${tooLong ? "text-danger" : "text-subtle"}`}>
          {tooLong || count > MAX_COMMENT_CHARS * 0.8 ? t.chat.counter(count, MAX_COMMENT_CHARS) : ""}
        </span>
        <div className="flex gap-2">
          <button type="button" onClick={onClose} disabled={sending} className="btn btn-secondary btn-sm disabled:opacity-60">
            {t.chat.rateCommentSkip}
          </button>
          <button type="submit" disabled={!canSend} className="btn btn-primary btn-sm disabled:opacity-60">
            {t.chat.rateCommentSubmit}
          </button>
        </div>
      </div>
      {error && (
        <p role="alert" className="text-[12.5px] text-danger">
          {error}
        </p>
      )}
    </form>
  );
}

interface Props {
  lang: Lang;
  t: AssistantMessages;
  messageId: string;
  rating: AiRating | null;
  onRate: OnRate;
  /** 点踩后的说明框是否展开（由上层持有：进行中的回答落库后这条回答会换一个组件实例，展开状态不能跟着丢）。 */
  commentOpen: boolean;
  onCommentOpen: (open: boolean) => void;
  /** 同一行的其它动作。 */
  actions?: ReactNode;
}

/**
 * 一条回答下的评价：赞 / 踩，再点同一个清除；点踩成功后展开可选的说明框。乐观更新由 `onRate` 负责，
 * 失败时已回滚，这里只显示错误。
 */
export default function RateBar({ lang, t, messageId, rating, onRate, commentOpen, onCommentOpen, actions }: Props) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const click = async (value: AiRating) => {
    setBusy(true);
    setError(null);
    const next = rating === value ? null : value;
    const code = await onRate(messageId, next);
    // 说明框的展开状态在上层：即便这个实例已被换掉（回答落库）也要照常更新
    if (code === null) onCommentOpen(next === "down");
    if (!mounted.current) return;
    setBusy(false);
    if (code !== null) setError(accountErrorText(lang, code));
  };

  const button = (value: AiRating) => {
    const pressed = rating === value;
    const Icon = value === "up" ? ThumbsUp : ThumbsDown;
    const label = value === "up" ? t.chat.rateUp : t.chat.rateDown;
    // 选中态只靠图标变色（赞强调色、踩危险色），不加边框 / 底色
    const tone = pressed ? (value === "up" ? "text-accent-ink" : "text-danger") : "text-subtle hover:text-fg";
    return (
      <button
        type="button"
        aria-pressed={pressed}
        aria-label={label}
        title={label}
        disabled={busy}
        onClick={() => void click(value)}
        className={`grid size-7 place-items-center rounded-md transition-colors disabled:opacity-60 ${tone}`}
      >
        <Icon size={14} aria-hidden className={pressed ? "fill-current" : ""} />
      </button>
    );
  };

  return (
    <div className="-mt-1">
      <div className="flex items-center gap-0.5">
        {button("up")}
        {button("down")}
        {actions}
      </div>
      {error && (
        <p role="alert" className="mt-1 text-[12.5px] text-danger">
          {error}
        </p>
      )}
      {commentOpen && rating === "down" && (
        <RateComment lang={lang} t={t} messageId={messageId} onRate={onRate} onClose={() => onCommentOpen(false)} />
      )}
    </div>
  );
}
