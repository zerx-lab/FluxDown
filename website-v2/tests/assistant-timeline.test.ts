import { expect, test } from "bun:test";
import { applyEvent, resumeTarget, startRun } from "../src/components/assistant/live";
import { insertPlaceholder, reconcileDraft, removePlaceholder, splitPlaceholders } from "../src/components/assistant/placeholders";
import { buildTimeline, lastUserMessage, retryNeedsRefresh, retryPlan } from "../src/components/assistant/timeline";
import { pasteTakesImages } from "../src/components/assistant/attachments";
import { safeHref } from "../src/components/assistant/md-parse";
import type { AiMessage } from "../src/lib/account/types";

const msg = (id: string, runId: string | null, seq: number, role: AiMessage["role"], content = id, extra: Partial<AiMessage> = {}): AiMessage => ({
  id, runId, seq, role, content, images: [], createdAt: "t", ...extra,
});

test("timeline: steps, answer and offer of one run merge into one assistant turn", () => {
  const items = buildTimeline(
    [msg("u1", "r1", 1, "user"), msg("s1", "r1", 2, "assistant_step"), msg("a1", "r1", 3, "assistant"), msg("o1", "r1", 4, "membership_offer", ""), msg("u2", "r2", 5, "user")],
    null,
  );
  expect(items.map((item) => item.kind)).toEqual(["user", "assistant", "user"]);
  const turn = items[1];
  if (turn?.kind !== "assistant") throw new Error("assistant turn expected");
  expect(turn.steps.map((m) => m.id)).toEqual(["s1"]);
  expect(turn.answer?.id).toBe("a1");
  expect(turn.offer).toBe(true);
});

test("timeline: the running run is hidden so a switch-back never shows the question twice", () => {
  const messages = [msg("u0", "r0", 1, "user"), msg("a0", "r0", 2, "assistant"), msg("u1", "r1", 3, "user"), msg("s1", "r1", 4, "assistant_step")];
  const items = buildTimeline(messages, "r1");
  expect(items.map((item) => (item.kind === "user" ? item.message.id : item.key))).toEqual(["u0", "a0"]);
  expect(lastUserMessage(messages)?.id).toBe("u1");
});

test("live: resumed events with an already seen sequence number are ignored", () => {
  let run = startRun("s1", "q", [], "r1");
  run = applyEvent(run, { type: "text", delta: "Hel" }, 1);
  run = applyEvent(run, { type: "text", delta: "Hel" }, 1);
  run = applyEvent(run, { type: "text", delta: "lo" }, 2);
  expect(run.lastSeq).toBe(2);
  expect(run.segments).toEqual([{ kind: "text", text: "Hello" }]);
});

test("live: resumeTarget follows activeRunId, falls back to the last question only for older clouds", () => {
  const history = [msg("u0", "r0", 1, "user", "old"), msg("u1", "r1", 2, "user", "new", { images: ["i1"] })];
  expect(resumeTarget(history, "r1")).toEqual({ runId: "r1", question: "new", imageIds: ["i1"] });
  expect(resumeTarget(history, "r0")).toEqual({ runId: "r0", question: "old", imageIds: [] });
  // 云端给的执行在历史里还没有提问:不猜,交给详情轮询
  expect(resumeTarget(history, "r9")).toBeNull();
  // 云端明确说没有进行中的执行
  expect(resumeTarget(history, null)).toBeNull();
  // 旧版云端不带 activeRunId
  expect(resumeTarget(history, undefined)).toEqual({ runId: "r1", question: "new", imageIds: ["i1"] });
  expect(resumeTarget([msg("a", "r1", 1, "assistant")], undefined)).toBeNull();
});

test("attachments: paste only becomes an attachment when the text is empty or just the file names", () => {
  const shot = [{ name: "image.png" }];
  expect(pasteTakesImages([], "text")).toBe(false);
  expect(pasteTakesImages(shot, "")).toBe(true);
  expect(pasteTakesImages(shot, "image.png")).toBe(true);
  expect(pasteTakesImages(shot, "/Users/me/image.png\r\n")).toBe(true);
  // Word / Excel 复制:带正文的截图,文字不能丢
  expect(pasteTakesImages(shot, "Quarter report\n42")).toBe(false);
});

test("markdown: safeHref only lets http(s), mailto and same-site paths through", () => {
  expect(safeHref("/docs/a")).toBe("/docs/a");
  expect(safeHref("#top")).toBe("#top");
  expect(safeHref("https://example.com/a")).toBe("https://example.com/a");
  // 反斜杠与 `//` 会被浏览器当成外站地址
  expect(safeHref("//evil.example")).toBeNull();
  expect(safeHref("/\\evil.example")).toBeNull();
  expect(safeHref("javascript:alert(1)")).toBeNull();
});

test("retry: a known runId never resends; only a missing runId can resend, and only with data-URL images", () => {
  const history = [msg("u1", "r1", 1, "user", "same question"), msg("a1", "r1", 2, "assistant"), msg("u2", "r2", 3, "user", "same question")];
  const failed = (runId: string | null, images: string[] = []) => ({ runId, question: "same question", images });
  // runId 已知:只按 runId 认最后一条提问,同样的文字不能认错
  expect(retryPlan(history, failed("r2"))).toEqual({ kind: "regenerate", message: history[2] });
  expect(retryPlan(history, failed("r2", ["/api/account/ai/attachments/i1"]))).toEqual({ kind: "regenerate", message: history[2] });
  // 它不是最后一条:不替换、也不重发(提问已在云端,重发会重复并多计一次额度)
  expect(retryPlan(history, failed("r1"))).toBeNull();
  // 历史还没刷新(没有这次执行的提问):同样不重发,交给刷新
  expect(retryPlan(history.slice(0, 2), failed("r2"))).toBeNull();
  expect(retryPlan([], failed("r2"))).toBeNull();
  expect(retryNeedsRefresh(history.slice(0, 2), failed("r2"))).toBe(true);
  expect(retryNeedsRefresh(history, failed("r2"))).toBe(false);
  expect(retryNeedsRefresh(history, failed("r1"))).toBe(false);
  expect(retryNeedsRefresh([], failed(null))).toBe(false);
  // runId 为空才按文字匹配最后一条;对不上才重发
  expect(retryPlan(history, failed(null))).toEqual({ kind: "regenerate", message: history[2] });
  expect(retryPlan([], failed(null))).toEqual({ kind: "resend", question: "same question", images: [] });
  // 没落库且附图不是 data URL:占位符会悬空,不提供重试
  expect(retryPlan([], { runId: null, question: "see [Image #1]", images: ["/api/account/ai/attachments/i1"] })).toBeNull();
  expect(retryPlan([], { runId: null, question: "see [Image #1]", images: ["data:image/png;base64,AA"] })).toEqual({
    kind: "resend",
    question: "see [Image #1]",
    images: ["data:image/png;base64,AA"],
  });
});

test("placeholders: insert, remove with renumbering, and manual deletion drops the image", () => {
  const inserted = insertPlaceholder("see", 3, 3, 1);
  expect(inserted.text).toBe("see [Image #1] ");
  const two = insertPlaceholder(inserted.text, inserted.text.length, inserted.text.length, 2);
  expect(two.text).toBe("see [Image #1] [Image #2] ");
  expect(removePlaceholder(two.text, two.caret, 1, 2).text).toBe("see [Image #1] ");
  const change = reconcileDraft("[Image #1] x [Image #2]", "x [Image #2]", 0, 2);
  expect(change.removed).toEqual([1]);
  expect(change.text).toBe("x [Image #1]");
});

test("placeholders: history text splits into text and image references within the image count", () => {
  expect(splitPlaceholders("a [Image #1] b [Image #3]", 1)).toEqual([
    { kind: "text", text: "a " },
    { kind: "image", text: "[Image #1]", number: 1 },
    { kind: "text", text: " b [Image #3]" },
  ]);
});
