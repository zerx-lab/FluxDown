/**
 * 清晰度选择列表：列出所有行的 variants，标出「播放中」，点击发送 downloadCandidate。
 * 浮钮弹层与资源列表的「选择清晰度」共用；children 渲染为列表底部附加项。
 */
import type { ComponentChildren, JSX } from "preact";
import { useState } from "preact/hooks";
import type { CandidateRow } from "@/background/protocol";
import { sendAction } from "@/utils/messaging";
import { useT } from "@/ui/i18n";
import { formatBytes } from "@/ui/format";
import { showToast } from "@/ui/components/Toast";
import { variantLabel } from "./labels";
import "./features.css";

interface QualityPickerProps {
  rows: CandidateRow[];
  tabId?: number;
  onDone?: () => void;
  children?: ComponentChildren;
}

export function QualityPicker(props: QualityPickerProps): JSX.Element {
  const t = useT();
  const [busy, setBusy] = useState(false);

  const pick = async (candidateId: string, variantId: string) => {
    if (busy) return;
    setBusy(true);
    try {
      const res = await sendAction<"downloadCandidate">({
        action: "downloadCandidate",
        tabId: props.tabId,
        candidateId,
        variantId,
      });
      if (res?.cancelled) {
        showToast(t("remote.manualCancelled"));
      } else if (res?.success) {
        props.onDone?.();
      } else {
        showToast(res?.message || t("list.sendFailed"), "error");
      }
    } catch (e) {
      showToast(e instanceof Error ? e.message : t("list.sendFailed"), "error");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="fd-qp" role="menu">
      {props.rows.map((row) => (
        <div class="fd-qp-group" key={row.candidate.id}>
          {/* 标题单行截断，悬停显示完整名称 */}
          <div class="fd-qp-title" title={row.candidate.title}>
            {row.candidate.title}
          </div>
          {row.candidate.protected ? (
            <div class="fd-qp-note">{t("float.protectedHint")}</div>
          ) : row.candidate.browserOnly ? (
            <div class="fd-qp-note">{t("float.browserOnlyHint")}</div>
          ) : null}
          {row.candidate.variants.map((v) => {
            const playing = row.playingVariantId === v.id;
            const meta = [
              v.codec,
              v.audioUrl ? t("float.audioVideo") : "",
              v.fileSize && v.fileSize > 0 ? formatBytes(v.fileSize) : "",
            ].filter(Boolean).join(" · ");
            return (
              <button
                type="button"
                class={`fd-qp-item${playing ? " fd-playing" : ""}`}
                role="menuitem"
                key={v.id}
                title={`${row.candidate.title}\n${variantLabel(t, v)}${meta ? ` · ${meta}` : ""}`}
                disabled={busy || !row.candidate.downloadable || row.candidate.protected === true || row.candidate.browserOnly === true}
                onClick={() => void pick(row.candidate.id, v.id)}
              >
                <span class="fd-qp-label">{variantLabel(t, v)}</span>
                <span class="fd-qp-meta">{meta}</span>
                {playing ? <span class="fd-qp-playing">{t("list.playing")}</span> : null}
              </button>
            );
          })}
        </div>
      ))}
      {props.children}
    </div>
  );
}
