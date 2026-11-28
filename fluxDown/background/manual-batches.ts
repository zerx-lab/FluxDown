import type { DispatchTarget } from "@/utils/download-dispatch";
import type { CandidateSelection, SendChannel, SendResult } from "./protocol";

export const CANCELLED_SELECTION = { success: false, cancelled: true } as const satisfies SendResult;

/** 单次 batch_download 条数上限（与桌面端 MAX_BATCH_ITEMS 对齐）。 */
const BATCH_LIMIT = 1000;

type Target = DispatchTarget | undefined;

interface BatchDispatch {
  chooseTarget(channel?: SendChannel): Promise<Target | null>;
  sendCandidate(tabId: number, candidateId: string, variantId: string | undefined, target: Target): Promise<SendResult>;
  sendResources(tabId: number, resourceIds: string[], target: Target): Promise<SendResult>;
  sendUrlChunk(urls: string[], referrer: string | undefined, tabId: number | undefined, target: Target): Promise<SendResult>;
}

export function createManualBatches({ chooseTarget, sendCandidate, sendResources, sendUrlChunk }: BatchDispatch) {
  return {
    async downloadSelection(tabId: number, candidates: CandidateSelection[], resourceIds: string[]): Promise<SendResult> {
      if (candidates.length === 0 && resourceIds.length === 0) return { success: true };
      const target = await chooseTarget();
      if (target === null) return CANCELLED_SELECTION;

      let failure: SendResult | undefined;
      for (const { candidateId, variantId } of candidates) {
        const result = await sendCandidate(tabId, candidateId, variantId, target);
        if (!result.success) failure ??= result;
      }
      if (resourceIds.length > 0) {
        const result = await sendResources(tabId, resourceIds, target);
        if (!result.success) failure ??= result;
      }
      return failure ?? { success: true };
    },

    async downloadManualUrls(
      urls: string[],
      referrer: string | undefined,
      tabId: number | undefined,
      channel?: SendChannel,
    ): Promise<SendResult> {
      const unique = [...new Set(urls.map((url) => url.trim()).filter(Boolean))];
      if (unique.length === 0) return { success: false, message: "No items" };
      const target = await chooseTarget(channel);
      if (target === null) return CANCELLED_SELECTION;

      if (unique.length <= BATCH_LIMIT) return sendUrlChunk(unique, referrer, tabId, target);
      let result: SendResult = { success: true };
      for (let i = 0; i < unique.length; i += BATCH_LIMIT) {
        result = await sendUrlChunk(unique.slice(i, i + BATCH_LIMIT), referrer, tabId, target);
        if (!result.success) return result;
      }
      return result;
    },
  };
}
