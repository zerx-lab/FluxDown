import { expect, test } from "bun:test";
import { applyEvent, resumeTarget, startRun } from "../src/components/assistant/live";
import { insertPlaceholder, reconcileDraft, removePlaceholder, splitPlaceholders } from "../src/components/assistant/placeholders";
import { buildTimeline, lastUserMessage } from "../src/components/assistant/timeline";
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

test("live: resumeTarget picks the last question of the running run", () => {
  const target = resumeTarget([msg("u0", "r0", 1, "user", "old"), msg("u1", "r1", 2, "user", "new", { images: ["i1"] })]);
  expect(target).toEqual({ runId: "r1", question: "new", imageIds: ["i1"] });
  expect(resumeTarget([msg("a", "r1", 1, "assistant")])).toBeNull();
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
