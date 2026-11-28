import { expect, test } from "bun:test";
import { applyEvent, startRun } from "../src/components/assistant/live";
import type { AiStreamEvent } from "../src/lib/account/types";

const started = (callId: string, name: string): AiStreamEvent => ({ type: "toolStarted", callId, name, kind: "server" });
const finished = (callId: string, name: string, isError: boolean): AiStreamEvent => ({
  type: "toolFinished", callId, name, kind: "server", isError, durationMs: 1,
});
const reduce = (events: AiStreamEvent[]) => events.reduce(applyEvent, startRun("s1", "q"));

test("live: offer_membership success sets the flag and never becomes a tool step", () => {
  const run = reduce([started("c1", "source_read"), started("offer-r1", "offer_membership"), finished("offer-r1", "offer_membership", false), finished("c1", "source_read", false)]);
  expect(run.membershipOffer).toBe(true);
  expect(run.segments).toEqual([{ kind: "tool", callId: "c1", name: "source_read", state: "ok" }]);
});

test("live: offer_membership running or failed shows no card and no step", () => {
  const running = reduce([started("offer-r1", "offer_membership")]);
  expect(running.membershipOffer).toBe(false);
  expect(running.segments).toEqual([]);

  const failed = reduce([started("o1", "offer_membership"), finished("o1", "offer_membership", true)]);
  expect(failed.membershipOffer).toBe(false);
  expect(failed.segments).toEqual([]);
});

test("live: several offer_membership calls in one run still set a single flag", () => {
  const run = reduce([finished("o1", "offer_membership", false), finished("o2", "offer_membership", false)]);
  expect(run.membershipOffer).toBe(true);
});
