/**
 * 进行中的一次提问：SSE 事件 → 展示状态的纯归约（事件契约见 `src/lib/account/types.ts::AiStreamEvent`）。
 */
import { isRecord } from "@/lib/is-record";
import type { AiFailReason, AiIssue, AiStreamEvent } from "@/lib/account/types";

/** FluxCloud 的服务端工具：成功即该轮显示开通会员卡片，条目本身不进过程步骤。 */
const OFFER_MEMBERSHIP_TOOL = "offer_membership";

export type Segment =
  | { kind: "text"; text: string }
  | { kind: "tool"; callId: string; name: string; state: "running" | "ok" | "error" };

/** `streaming` 回答中；`done` 正常结束；`stopped` 用户停止；`failed` 执行失败；`interrupted` 连接中断。 */
export type LivePhase = "streaming" | "done" | "stopped" | "failed" | "interrupted";

export interface LiveRun {
  sessionId: string;
  question: string;
  /** 提问附带的图片（data URL，回答结束刷新详情后换成附件地址）。 */
  images: string[];
  /** 按到达顺序的回答文本与工具调用。 */
  segments: Segment[];
  issues: AiIssue[];
  /** 本轮调用过 `offer_membership` 且成功：回答后显示开通会员卡片。 */
  membershipOffer: boolean;
  phase: LivePhase;
  /** `failed` / `stopped` 时云端给出的原因（旧版 FluxCloud 不带）。 */
  failReason: AiFailReason | null;
  /** `done` 时这次回答的消息 ID（可对它评价；`replied` 等无落库消息时为 null）。 */
  messageId: string | null;
  /** 收到过任何事件（区分「请求被拒」与「中途断开」）。 */
  started: boolean;
}

export function startRun(sessionId: string, question: string, images: string[] = []): LiveRun {
  return { sessionId, question, images, segments: [], issues: [], membershipOffer: false, phase: "streaming", failReason: null, messageId: null, started: false };
}

/** BFF 已按白名单裁剪，这里只确认是带 `type` 的对象。 */
export function asStreamEvent(raw: unknown): AiStreamEvent | null {
  return isRecord(raw) && typeof raw.type === "string" ? (raw as AiStreamEvent) : null;
}

export function applyEvent(run: LiveRun, event: AiStreamEvent): LiveRun {
  const next: LiveRun = { ...run, started: true };
  const last = run.segments[run.segments.length - 1];
  switch (event.type) {
    case "run":
      return next;
    case "text":
      if (event.delta === "") return next;
      next.segments =
        last?.kind === "text"
          ? [...run.segments.slice(0, -1), { kind: "text", text: last.text + event.delta }]
          : [...run.segments, { kind: "text", text: event.delta }];
      return next;
    case "turnRetried":
      if (last?.kind === "text") next.segments = run.segments.slice(0, -1);
      return next;
    case "toolStarted":
      if (event.name === OFFER_MEMBERSHIP_TOOL) return next;
      next.segments = [...run.segments, { kind: "tool", callId: event.callId, name: event.name, state: "running" }];
      return next;
    case "toolFinished":
      if (event.name === OFFER_MEMBERSHIP_TOOL) {
        if (!event.isError) next.membershipOffer = true;
        return next;
      }
      next.segments = run.segments.map((segment) =>
        segment.kind === "tool" && segment.callId === event.callId
          ? { ...segment, state: event.isError ? "error" : "ok" }
          : segment,
      );
      return next;
    case "issueDraft":
      next.issues = [...run.issues.filter((issue) => issue.id !== event.issue.id), event.issue];
      return next;
    case "done":
      next.phase = "done";
      next.messageId = event.messageId;
      if (!run.segments.some((segment) => segment.kind === "text") && event.text.trim() !== "") {
        next.segments = [...run.segments, { kind: "text", text: event.text }];
      }
      return next;
    case "error":
      next.phase = event.status === "canceled" ? "stopped" : "failed";
      next.failReason = event.reason ?? null;
      next.segments = abandonTools(run.segments);
      return next;
  }
}

/** 没跑完的工具不会再有结果：异常结束时标为失败，不再转圈。 */
function abandonTools(segments: Segment[]): Segment[] {
  return segments.map((segment) => (segment.kind === "tool" && segment.state === "running" ? { ...segment, state: "error" } : segment));
}

/** 流结束（或中断）后的最终阶段：没收到终止事件即为中断，用户中止为停止。 */
export function settle(run: LiveRun, aborted: boolean): LiveRun {
  if (run.phase !== "streaming") return run;
  return { ...run, phase: aborted ? "stopped" : "interrupted", segments: abandonTools(run.segments) };
}

export type ToolLabel = "source" | "github" | "issue" | "other";

export function toolLabel(name: string): ToolLabel {
  if (name.startsWith("source_")) return "source";
  if (name.startsWith("github_")) return "github";
  if (name === "propose_issue") return "issue";
  return "other";
}
