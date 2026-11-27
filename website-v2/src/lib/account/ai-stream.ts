/**
 * AI 助手 SSE 事件的白名单裁剪：BFF 逐帧解析 FluxCloud 的执行事件流（`ai::runner::RunEvent`，
 * 每帧 `data` 为一个带 `type` 的 JSON），只把官网用得到的事件与字段原样转发给浏览器。
 *
 * 丢弃：`triage` / `routed`（分诊与路由内部信息）、`reasoning`（只推给后台测试）、`clientToolCall`
 * （官网建会话不声明客户端工具）、`done` 的 token 用量与任何未知事件。保活注释转成同样的注释帧。
 */
import { isRecord, pick } from "@/lib/is-record";
import { SseParser } from "@/lib/sse";

/** 问题单字段（FluxCloud `ai::issues::IssueDto`，去掉提交失败的内部错误）。 */
export const AI_ISSUE_KEYS = [
  "id", "sessionId", "kind", "title", "body", "labels", "status", "githubNumber", "githubUrl", "reviewNote",
  "createdAt", "updatedAt",
] as const;

const EVENT_KEYS: Record<string, readonly string[]> = {
  run: ["runId", "sessionId"],
  text: ["delta"],
  turnRetried: ["turn"],
  toolStarted: ["callId", "name", "kind"],
  toolFinished: ["callId", "name", "kind", "isError", "durationMs"],
  issueDraft: [],
  done: ["status", "messageId", "text"],
  error: ["status", "reason", "message"],
};

/** 一个上游事件 → 转发给浏览器的事件；不在白名单里或形状不对时返回 null。 */
export function sanitizeAiEvent(event: unknown): Record<string, unknown> | null {
  if (!isRecord(event) || typeof event.type !== "string" || !Object.hasOwn(EVENT_KEYS, event.type)) return null;
  const out = pick(event, EVENT_KEYS[event.type] ?? []);
  if (!out) return null;
  if (event.type === "issueDraft") {
    const issue = pick(event.issue, AI_ISSUE_KEYS);
    if (!issue) return null;
    out.issue = issue;
  }
  return { type: event.type, ...out };
}

function sanitizeFrame(data: string): string | null {
  let event: unknown;
  try {
    event = JSON.parse(data);
  } catch {
    return null;
  }
  const out = sanitizeAiEvent(event);
  return out === null ? null : JSON.stringify(out);
}

/** 字节流变换：上游 SSE（UTF-8）→ 裁剪后的 SSE（每帧一个 `data:` 行）；多字节字符可跨块。 */
export function aiEventFilter(): TransformStream<Uint8Array, Uint8Array> {
  const parser = new SseParser();
  const decoder = new TextDecoder();
  const encoder = new TextEncoder();
  const forward = (text: string, controller: TransformStreamDefaultController<Uint8Array>) => {
    for (const frame of parser.push(text)) {
      if (frame === null) {
        controller.enqueue(encoder.encode(": ka\n\n"));
        continue;
      }
      const out = sanitizeFrame(frame);
      if (out !== null) controller.enqueue(encoder.encode(`data: ${out}\n\n`));
    }
  };
  return new TransformStream({
    transform(chunk, controller) {
      forward(decoder.decode(chunk, { stream: true }), controller);
    },
    flush(controller) {
      forward(decoder.decode(), controller);
    },
  });
}
