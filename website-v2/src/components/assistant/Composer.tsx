import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type ChangeEvent, type ClipboardEvent, type DragEvent, type KeyboardEvent } from "react";
import { ArrowUp, ImagePlus, LoaderCircle, Square, X } from "lucide-react";
import type { AssistantMessages } from "@/i18n/messages/assistant";
import { charCount } from "@/components/account/dashboard/format";
import { filesOf, IMAGE_ACCEPT, pasteTakesImages, prepareImage, type AttachError, type Attachment } from "./attachments";
import ImageViewer, { type ViewerImage } from "./ImageViewer";
import { insertPlaceholder, reconcileDraft, removePlaceholder } from "./placeholders";
import { AI_ROOT_ATTR } from "./shortcuts";

/** 图片附件限制（来自 `ai/config`）；null = 当前模型不支持图片。 */
export interface ImageLimits {
  max: number;
  maxBytes: number;
}

interface Props {
  t: AssistantMessages;
  placeholder: string;
  maxChars: number;
  /** 当前会话正在回答：显示停止按钮。 */
  streaming: boolean;
  /** 另一个会话正在回答：同一时间只允许一条流，输入照常但不能发送，并说明原因。 */
  busyElsewhere: boolean;
  /** 不可输入时的说明（额度用完）；null = 可输入。 */
  blockedReason: string | null;
  /** 本对话已达提问上限：输入区换成提示卡（草稿保留在组件里，不显示输入框 / 附图 / 发送）；null = 未达上限。 */
  sessionLimit: { limit: number; onNewChat: () => void } | null;
  /** 剩余可提问次数（1..=3 时给值，输入区附近显示弱化提示）。 */
  remaining: number | null;
  /** 已排队到输入框的文字（点建议问题 / 编辑重发时填入）。 */
  prefill: { text: string; nonce: number } | null;
  /** 正在编辑重发一条旧提问：显示提示条，发送会替换它及其后的内容。 */
  editing: boolean;
  onCancelEdit: () => void;
  images: ImageLimits | null;
  /** `images` 为 data URL。返回 false = 请求被拒，文字与图片放回输入框。 */
  onSend: (text: string, images: string[]) => Promise<boolean>;
  onStop: () => void;
  /** 变化即把焦点移进输入框（切换 / 新建会话、切回对话视图等用户动作）。 */
  focusKey: string;
}

type ImageError = AttachError | "count" | "unsupported";

const toMb = (bytes: number) => Math.floor(bytes / 1024 / 1024);

/**
 * 输入框：Enter 发送（输入法组字中不触发），Shift+Enter 换行；回答中显示停止按钮。
 * 图片：粘贴、拖入或点按钮选择，超限的静态图在浏览器里先缩小（HEIC / BMP / AVIF 等转 PNG）；提问仍须有文字（FluxCloud 要求）。
 * 每张图在光标处写入 `[Image #N]` 占位符（与 GPUI 一致）：移除图片时占位符一并删除并重新编号，
 * 手动删掉占位符则图片随之移除。
 * 尽量让焦点停在这里：挂载、回答结束、恢复可输入时（焦点不在助手区域外的其他控件上才抢），
 * `focusKey` 变化与填入文字时（用户动作，直接抢）。触屏设备不自动聚焦，免得弹出键盘挡住内容。
 */
export default function Composer({
  t, placeholder, maxChars, streaming, busyElsewhere, blockedReason, sessionLimit, remaining, prefill, editing, onCancelEdit, images, onSend, onStop, focusKey,
}: Props) {
  const id = useId();
  const [text, setText] = useState("");
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [pending, setPending] = useState(0);
  const [imageError, setImageError] = useState<ImageError | null>(null);
  const [dragging, setDragging] = useState(false);
  const [viewing, setViewing] = useState<ViewerImage | null>(null);
  const [seenPrefill, setSeenPrefill] = useState<number | null>(null);
  const input = useRef<HTMLTextAreaElement>(null);
  const picker = useRef<HTMLInputElement>(null);
  // 文本与附件同步镜像：异步准备图片期间用户还在打字，插入占位符要基于最新值
  const textRef = useRef(text);
  const attachedRef = useRef(attachments);
  const pendingRef = useRef(0);
  /** 渲染后要落到的光标位置（受控输入框改文本会把光标甩到末尾）。 */
  const caretRef = useRef<number | null>(null);

  const putText = useCallback((next: string, caret: number | null = null) => {
    textRef.current = next;
    caretRef.current = caret;
    setText(next);
  }, []);
  const putAttachments = useCallback((next: Attachment[]) => {
    attachedRef.current = next;
    setAttachments(next);
  }, []);

  if (prefill && prefill.nonce !== seenPrefill) {
    setSeenPrefill(prefill.nonce);
    putText(prefill.text);
    putAttachments([]);
  }

  useLayoutEffect(() => {
    const caret = caretRef.current;
    const el = input.current;
    if (caret === null || !el) return;
    caretRef.current = null;
    el.setSelectionRange(caret, caret);
  });

  const focusInput = useCallback((force: boolean) => {
    const el = input.current;
    if (!el || matchMedia("(pointer: coarse)").matches) return;
    const active = document.activeElement;
    if (active === el && !force) return;
    // 非强制：焦点在页面其他控件，或在助手区域内另一个输入框（如编辑问题单）时不抢
    const elsewhere = active !== null && active !== document.body && !el.closest(`[${AI_ROOT_ATTR}]`)?.contains(active);
    const typing = active instanceof HTMLElement && (active.isContentEditable || active.matches("input, textarea, select"));
    if (!force && (elsewhere || typing)) return;
    el.focus({ preventScroll: true });
    if (caretRef.current === null) {
      const end = el.value.length;
      el.setSelectionRange(end, end);
    }
  }, []);

  const mounted = useRef(false);
  useEffect(() => {
    focusInput(mounted.current);
    mounted.current = true;
  }, [focusKey, focusInput]);

  useEffect(() => {
    if (!streaming) focusInput(false);
  }, [streaming, blockedReason, focusInput]);

  useEffect(() => {
    if (seenPrefill !== null) focusInput(true);
  }, [seenPrefill, focusInput]);

  /** 逐张准备（缩放 / 转码 / 读取），超出张数的直接报错；已有 + 处理中的都计入张数。 */
  const addFiles = async (files: File[]) => {
    if (files.length === 0) return;
    if (!images) {
      setImageError("unsupported");
      return;
    }
    setImageError(null);
    const room = images.max - attachedRef.current.length - pendingRef.current;
    const accepted = files.slice(0, Math.max(0, room));
    if (accepted.length < files.length) setImageError("count");
    pendingRef.current += accepted.length;
    setPending(pendingRef.current);
    for (const file of accepted) {
      const prepared = await prepareImage(file, images.maxBytes);
      pendingRef.current -= 1;
      setPending(pendingRef.current);
      if (!prepared.ok) {
        setImageError(prepared.error);
        continue;
      }
      const el = input.current;
      const start = el?.selectionStart ?? textRef.current.length;
      const end = el?.selectionEnd ?? start;
      const number = attachedRef.current.length + 1;
      const edit = insertPlaceholder(textRef.current, start, end, number);
      putAttachments([...attachedRef.current, prepared.attachment]);
      putText(edit.text, edit.caret);
    }
    focusInput(true);
  };

  const remove = (key: string) => {
    const index = attachedRef.current.findIndex((item) => item.key === key);
    if (index < 0) return;
    const edit = removePlaceholder(textRef.current, input.current?.selectionStart ?? textRef.current.length, index + 1, attachedRef.current.length);
    putAttachments(attachedRef.current.filter((item) => item.key !== key));
    putText(edit.text, edit.caret);
    setImageError(null);
    focusInput(true);
  };

  const onChange = (event: ChangeEvent<HTMLTextAreaElement>) => {
    const el = event.target;
    const change = reconcileDraft(textRef.current, el.value, el.selectionStart, attachedRef.current.length);
    if (change.removed.length > 0) {
      const dropped = new Set(change.removed);
      putAttachments(attachedRef.current.filter((_, index) => !dropped.has(index + 1)));
      setImageError(null);
    }
    putText(change.text, change.text === el.value ? null : change.caret);
  };

  const count = charCount(text);
  const trimmed = text.trim();
  const tooLong = maxChars > 0 && count > maxChars;
  const canSend = !streaming && !busyElsewhere && blockedReason === null && sessionLimit === null && trimmed !== "" && !tooLong && pending === 0;

  const submit = async () => {
    if (!canSend) return;
    const sentText = textRef.current;
    const sentImages = attachedRef.current;
    putText("");
    putAttachments([]);
    setImageError(null);
    const ok = await onSend(trimmed, sentImages.map((item) => item.url));
    if (!ok) {
      if (textRef.current === "") putText(sentText);
      if (attachedRef.current.length === 0) putAttachments(sentImages);
    }
    input.current?.focus();
  };

  const cancelEdit = () => {
    putText("");
    putAttachments([]);
    setImageError(null);
    onCancelEdit();
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Escape" && editing) {
      event.preventDefault();
      cancelEdit();
      return;
    }
    // Safari 结束输入法组字的那次 Enter 报 `isComposing: false`，只能靠 keyCode 229 识别。
    if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing || event.keyCode === 229) return;
    event.preventDefault();
    void submit();
  };

  // 剪贴板里有文件且文字只是文件名（截图、复制的图片）才当附件处理；带着正文的（Word / Excel / 网页复制常附一张截图）
  // 让浏览器照常粘贴文字；当前模型不支持图片时一律照常粘贴
  const onPaste = (event: ClipboardEvent<HTMLTextAreaElement>) => {
    const files = filesOf(event.clipboardData.items);
    if (!images || !pasteTakesImages(files, event.clipboardData.getData("text/plain"))) return;
    event.preventDefault();
    void addFiles(files);
  };

  const hasFiles = (event: DragEvent) => event.dataTransfer.types.includes("Files");
  const dropProps =
    blockedReason === null && sessionLimit === null
      ? {
          onDragEnter: (event: DragEvent) => {
            if (!hasFiles(event)) return;
            event.preventDefault();
            setDragging(true);
          },
          onDragOver: (event: DragEvent) => {
            if (!hasFiles(event)) return;
            event.preventDefault();
            event.dataTransfer.dropEffect = "copy";
          },
          onDragLeave: (event: DragEvent) => {
            if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setDragging(false);
          },
          onDrop: (event: DragEvent) => {
            if (!hasFiles(event)) return;
            event.preventDefault();
            setDragging(false);
            void addFiles(filesOf(event.dataTransfer.files));
          },
        }
      : {};

  const errorText =
    imageError === null
      ? null
      : imageError === "size"
        ? t.chat.imageErrors.size(toMb(images?.maxBytes ?? 0))
        : imageError === "count"
          ? t.chat.imageErrors.count(images?.max ?? 0)
          : t.chat.imageErrors[imageError];
  const hint =
    blockedReason !== null || sessionLimit !== null
      ? null
      : busyElsewhere
        ? t.chat.otherAnswering
        : tooLong || (maxChars > 0 && count > maxChars * 0.8)
          ? t.chat.counter(count, maxChars)
          : attachments.length > 0 && trimmed === ""
            ? t.chat.imageNeedsText
            : t.chat.sendHint;

  return (
    <form
      className="border-t border-line px-4 pb-4 pt-3 sm:px-6"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
      {...dropProps}
    >
      <div className="mx-auto max-w-[760px]">
        {editing && blockedReason === null && sessionLimit === null && (
          <p className="mb-2 flex items-center justify-between gap-3 rounded-lg bg-accent-soft px-3 py-1.5 text-[12.5px] text-accent-ink">
            <span>{t.chat.editNotice}</span>
            <button type="button" onClick={cancelEdit} className="shrink-0 underline underline-offset-2">
              {t.chat.editCancel}
            </button>
          </p>
        )}
        {blockedReason !== null ? (
          <p role="status" className="rounded-xl border border-line bg-inset px-4 py-3 text-[13.5px] text-muted">
            {blockedReason}
          </p>
        ) : sessionLimit !== null ? (
          <div role="status" className="rounded-xl border border-line bg-inset px-4 py-3">
            <p className="text-[13.5px] font-medium text-fg">{t.chat.sessionLimitTitle}</p>
            <p className="mt-1 text-[13px] text-muted">{t.chat.sessionLimitDesc(sessionLimit.limit)}</p>
            <button type="button" onClick={sessionLimit.onNewChat} className="btn btn-primary mt-3">
              {t.sidebar.newChat}
            </button>
          </div>
        ) : (
          <div
            className={`relative rounded-xl border bg-elev px-3 py-2 focus-within:border-line-strong ${dragging ? "border-accent" : "border-line"}`}
          >
            {dragging && (
              <div aria-hidden className="pointer-events-none absolute inset-0 z-10 grid place-items-center rounded-xl bg-elev/90 text-[13px] text-accent-ink">
                {t.chat.imageDrop}
              </div>
            )}
            {(attachments.length > 0 || pending > 0) && (
              <ul className="mb-2 flex flex-wrap gap-2 pt-1">
                {attachments.map((item, i) => (
                  <li key={item.key} className="group relative">
                    <button
                      type="button"
                      onClick={() => setViewing({ src: item.url, alt: t.chat.imageAlt(i + 1) })}
                      aria-label={`${t.chat.viewImage} · ${t.chat.imageAlt(i + 1)}`}
                      className="block size-14 overflow-hidden rounded-lg border border-line bg-inset"
                    >
                      <img src={item.url} alt="" className="size-full object-cover" />
                    </button>
                    <span aria-hidden className="pointer-events-none absolute bottom-0.5 left-0.5 rounded bg-black/60 px-1 text-[10px] leading-4 text-white">
                      #{i + 1}
                    </span>
                    <button
                      type="button"
                      onClick={() => remove(item.key)}
                      aria-label={`${t.chat.removeImage} · ${t.chat.imageAlt(i + 1)}`}
                      title={t.chat.removeImage}
                      className="absolute -right-1.5 -top-1.5 grid size-5 place-items-center rounded-full border border-line-strong bg-elev text-muted shadow-[var(--shadow-float)] hover:text-fg"
                    >
                      <X size={11} aria-hidden />
                    </button>
                  </li>
                ))}
                {pending > 0 && (
                  <li role="status" aria-label={t.chat.imagesProcessing} className="grid size-14 place-items-center rounded-lg border border-dashed border-line text-muted">
                    <LoaderCircle size={16} aria-hidden className="animate-spin motion-reduce:animate-none" />
                  </li>
                )}
              </ul>
            )}
            <div className="flex items-end gap-2">
              {images && (
                <>
                  <input
                    ref={picker}
                    type="file"
                    accept={IMAGE_ACCEPT}
                    multiple
                    hidden
                    tabIndex={-1}
                    onChange={(event) => {
                      void addFiles(filesOf(event.target.files));
                      event.target.value = "";
                    }}
                  />
                  <button
                    type="button"
                    onClick={() => picker.current?.click()}
                    disabled={attachments.length + pending >= images.max}
                    aria-label={t.chat.attachImage}
                    title={t.chat.imageHint(images.max, toMb(images.maxBytes))}
                    className="ai-icon-btn mb-0.5 shrink-0 disabled:opacity-40"
                  >
                    <ImagePlus size={17} aria-hidden />
                  </button>
                </>
              )}
              <label htmlFor={`${id}-input`} className="sr-only">
                {t.chat.inputLabel}
              </label>
              <textarea
                id={`${id}-input`}
                ref={input}
                rows={1}
                value={text}
                placeholder={placeholder}
                aria-describedby={`${id}-hint`}
                aria-invalid={tooLong || undefined}
                onChange={onChange}
                onKeyDown={onKeyDown}
                onPaste={onPaste}
                className="max-h-56 min-h-9 flex-1 resize-none bg-transparent py-1.5 text-[14.5px] leading-relaxed outline-none [field-sizing:content] placeholder:text-subtle"
              />
              {streaming ? (
                <button type="button" onClick={onStop} aria-label={t.chat.stop} title={t.chat.stop} className="btn btn-secondary size-9 shrink-0 p-0">
                  <Square size={14} aria-hidden className="fill-current" />
                </button>
              ) : (
                <button type="submit" disabled={!canSend} aria-label={t.chat.send} title={busyElsewhere ? t.chat.otherAnswering : t.chat.send} className="btn btn-primary size-9 shrink-0 p-0 disabled:opacity-50">
                  <ArrowUp size={16} aria-hidden />
                </button>
              )}
            </div>
          </div>
        )}
        {errorText && (
          <p role="alert" className="mt-2 text-[12.5px] text-danger">
            {errorText}
          </p>
        )}
        <div id={`${id}-hint`} className="mt-2 flex flex-wrap justify-between gap-x-4 gap-y-1 text-[12px] text-subtle">
          <span>{t.chat.disclaimer}</span>
          {((remaining !== null && sessionLimit === null) || hint) && (
            <span className="flex flex-wrap gap-x-3">
              {remaining !== null && sessionLimit === null && <span>{t.chat.sessionRemaining(remaining)}</span>}
              {hint && <span className={tooLong ? "text-danger" : undefined}>{hint}</span>}
            </span>
          )}
        </div>
      </div>
      {viewing && <ImageViewer image={viewing} t={t} onClose={() => setViewing(null)} />}
    </form>
  );
}
