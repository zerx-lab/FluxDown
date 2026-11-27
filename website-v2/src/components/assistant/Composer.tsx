import { useCallback, useEffect, useId, useRef, useState, type ClipboardEvent, type DragEvent, type KeyboardEvent } from "react";
import { ArrowUp, ImagePlus, LoaderCircle, Square, X } from "lucide-react";
import type { AssistantMessages } from "@/i18n/messages/assistant";
import { charCount } from "@/components/account/dashboard/format";
import { filesOf, IMAGE_ACCEPT, prepareImage, type AttachError, type Attachment } from "./attachments";
import ImageViewer, { type ViewerImage } from "./ImageViewer";
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
  streaming: boolean;
  /** 不可输入时的说明（额度用完 / 只读会话）；null = 可输入。 */
  blockedReason: string | null;
  /** 已排队到输入框的文字（点建议问题时填入）。 */
  prefill: { text: string; nonce: number } | null;
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
 * 图片：粘贴、拖入或点按钮选择，超限的静态图在浏览器里先缩小；提问仍须有文字（FluxCloud 要求）。
 * 尽量让焦点停在这里：挂载、回答结束、恢复可输入时（焦点不在助手区域外的其他控件上才抢），
 * `focusKey` 变化与填入建议问题时（用户动作，直接抢）。触屏设备不自动聚焦，免得弹出键盘挡住内容。
 */
export default function Composer({ t, placeholder, maxChars, streaming, blockedReason, prefill, images, onSend, onStop, focusKey }: Props) {
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
  const attachedRef = useRef(attachments);
  attachedRef.current = attachments;
  const pendingRef = useRef(0);

  if (prefill && prefill.nonce !== seenPrefill) {
    setSeenPrefill(prefill.nonce);
    setText(prefill.text);
  }

  const focusInput = useCallback((force: boolean) => {
    const el = input.current;
    if (!el || matchMedia("(pointer: coarse)").matches) return;
    const active = document.activeElement;
    if (active === el && !force) return;
    // 非强制：焦点在页面其他控件，或在助手区域内另一个输入框（如编辑问题单）时不抢
    const elsewhere = active !== null && active !== document.body && !el.closest(`[${AI_ROOT_ATTR}]`)?.contains(active);
    const editing = active instanceof HTMLElement && (active.isContentEditable || active.matches("input, textarea, select"));
    if (!force && (elsewhere || editing)) return;
    el.focus({ preventScroll: true });
    const end = el.value.length;
    el.setSelectionRange(end, end);
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

  /** 逐张准备（缩放 / 读取），超出张数的直接报错；已有 + 处理中的都计入张数。 */
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
      if (prepared.ok) setAttachments((list) => [...list, prepared.attachment]);
      else setImageError(prepared.error);
    }
    focusInput(true);
  };

  const remove = (key: string) => {
    setAttachments((list) => list.filter((item) => item.key !== key));
    setImageError(null);
    focusInput(true);
  };

  const count = charCount(text);
  const trimmed = text.trim();
  const tooLong = maxChars > 0 && count > maxChars;
  const canSend = !streaming && blockedReason === null && trimmed !== "" && !tooLong && pending === 0;

  const submit = async () => {
    if (!canSend) return;
    const sentText = text;
    const sentImages = attachments;
    setText("");
    setAttachments([]);
    setImageError(null);
    const ok = await onSend(trimmed, sentImages.map((item) => item.url));
    if (!ok) {
      setText((current) => (current === "" ? sentText : current));
      setAttachments((current) => (current.length === 0 ? sentImages : current));
    }
    input.current?.focus();
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    // Safari 结束输入法组字的那次 Enter 报 `isComposing: false`，只能靠 keyCode 229 识别。
    if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing || event.keyCode === 229) return;
    event.preventDefault();
    void submit();
  };

  // 剪贴板里有文件（截图、复制的图片）就当附件处理，不再粘贴随附的文字（多是文件名）；纯文字照常粘贴
  const onPaste = (event: ClipboardEvent<HTMLTextAreaElement>) => {
    const files = filesOf(event.clipboardData.items);
    if (files.length === 0) return;
    event.preventDefault();
    void addFiles(files);
  };

  const hasFiles = (event: DragEvent) => event.dataTransfer.types.includes("Files");
  const dropProps =
    blockedReason === null
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
    blockedReason !== null
      ? null
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
        {blockedReason !== null ? (
          <p role="status" className="rounded-xl border border-line bg-inset px-4 py-3 text-[13.5px] text-muted">
            {blockedReason}
          </p>
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
                onChange={(event) => setText(event.target.value)}
                onKeyDown={onKeyDown}
                onPaste={onPaste}
                className="max-h-56 min-h-9 flex-1 resize-none bg-transparent py-1.5 text-[14.5px] leading-relaxed outline-none [field-sizing:content] placeholder:text-subtle"
              />
              {streaming ? (
                <button type="button" onClick={onStop} aria-label={t.chat.stop} title={t.chat.stop} className="btn btn-secondary size-9 shrink-0 p-0">
                  <Square size={14} aria-hidden className="fill-current" />
                </button>
              ) : (
                <button type="submit" disabled={!canSend} aria-label={t.chat.send} title={t.chat.send} className="btn btn-primary size-9 shrink-0 p-0 disabled:opacity-50">
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
          {hint && <span className={tooLong ? "text-danger" : undefined}>{hint}</span>}
        </div>
      </div>
      {viewing && <ImageViewer image={viewing} t={t} onClose={() => setViewing(null)} />}
    </form>
  );
}
