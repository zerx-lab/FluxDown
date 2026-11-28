/**
 * 会话历史 → 展示时间线：把同一次执行（`runId`）的 `assistant_step`（工具调用前的说明文字，归入「过程」）、
 * 最终回答与开通会员标记合成一个助手回合，用户消息单列。纯函数，不依赖浏览器。
 */
import type { AiMessage } from "@/lib/account/types";

export interface UserItem {
  kind: "user";
  message: AiMessage;
}

export interface AssistantItem {
  kind: "assistant";
  /** 稳定的列表键（回合内第一条消息的 ID）。 */
  key: string;
  runId: string | null;
  /** 过程里的文本步骤（`assistant_step`，按 seq 顺序）。 */
  steps: AiMessage[];
  /** 最终回答（含 `interrupted` 的部分回答）；该次执行没有落库回答时为 null。 */
  answer: AiMessage | null;
  /** 该次执行成功调用过 `offer_membership`：回答后显示开通会员卡片。 */
  offer: boolean;
}

export type TimelineItem = UserItem | AssistantItem;

/**
 * 按 seq 顺序（接口已排好）分组。`hiddenRunId` = 正在显示的进行中的执行：它的所有消息由进行中的展示负责，历史里不再画
 * （否则切走再切回时提问与回答各出现两次）。
 */
export function buildTimeline(messages: readonly AiMessage[], hiddenRunId: string | null): TimelineItem[] {
  const items: TimelineItem[] = [];
  let open: AssistantItem | null = null;

  const turnFor = (message: AiMessage, needsRoom: boolean): AssistantItem => {
    // 同一次执行的消息并进同一回合；回答已落库后再来的步骤 / 回答（不应出现）另起一个回合
    if (open !== null && message.runId !== null && open.runId === message.runId && (!needsRoom || open.answer === null)) {
      return open;
    }
    const item: AssistantItem = { kind: "assistant", key: message.id, runId: message.runId, steps: [], answer: null, offer: false };
    items.push(item);
    open = item;
    return item;
  };

  for (const message of messages) {
    if (hiddenRunId !== null && message.runId === hiddenRunId) continue;
    switch (message.role) {
      case "user":
        items.push({ kind: "user", message });
        open = null;
        break;
      case "assistant_step":
        turnFor(message, true).steps.push(message);
        break;
      case "assistant":
        turnFor(message, true).answer = message;
        break;
      case "membership_offer":
        turnFor(message, false).offer = true;
        break;
    }
  }
  return items;
}

/** 最后一条用户消息（重新生成 / 编辑重发的目标）；它之后没有落库的回答（回答中 / 已中断）也照样返回。 */
export function lastUserMessage(messages: readonly AiMessage[]): AiMessage | null {
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const message = messages[i];
    if (message?.role === "user") return message;
  }
  return null;
}
