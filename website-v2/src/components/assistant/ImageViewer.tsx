import { useEffect, useRef } from "react";
import { X } from "lucide-react";
import type { AssistantMessages } from "@/i18n/messages/assistant";

export interface ViewerImage {
  src: string;
  alt: string;
}

/** 图片大图预览：原生模态 `<dialog>`（顶层渲染，盖过悬浮面板）；Esc、点背景或关闭按钮退出。 */
export default function ImageViewer({ image, t, onClose }: { image: ViewerImage; t: AssistantMessages; onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const el = dialog.current;
    if (el && !el.open) el.showModal();
  }, []);

  return (
    <dialog
      ref={dialog}
      aria-label={image.alt}
      onClose={onClose}
      // Esc 只关预览，不冒泡到悬浮面板（面板的 Esc 会收起整个助手）
      onKeyDown={(event) => {
        if (event.key === "Escape") event.stopPropagation();
      }}
      onClick={(event) => {
        if (event.target === event.currentTarget) dialog.current?.close();
      }}
      className="ai-image-viewer"
    >
      <img src={image.src} alt={image.alt} />
      <button type="button" onClick={() => dialog.current?.close()} aria-label={t.chat.closeImage} title={t.chat.closeImage} className="ai-image-viewer-close">
        <X size={18} aria-hidden />
      </button>
    </dialog>
  );
}
