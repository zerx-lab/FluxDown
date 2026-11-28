import { expect, test } from "bun:test";
import { sendErrorKind, sessionLimitView } from "../src/components/assistant/sessionLimit";

test("session limit: 0, missing limit or missing detail means unlimited", () => {
  expect(sessionLimitView(0, 99, false)).toEqual({ reached: false, remaining: null });
  expect(sessionLimitView(undefined, 99, false)).toEqual({ reached: false, remaining: null });
  expect(sessionLimitView(10, undefined, false)).toEqual({ reached: false, remaining: null });
});

test("session limit: remaining hint only for the last 3 questions", () => {
  expect(sessionLimitView(10, 6, false)).toEqual({ reached: false, remaining: null });
  expect(sessionLimitView(10, 7, false)).toEqual({ reached: false, remaining: 3 });
  expect(sessionLimitView(10, 9, false)).toEqual({ reached: false, remaining: 1 });
});

test("session limit: reached at used >= limit, but not while a run is in progress", () => {
  expect(sessionLimitView(10, 10, false)).toEqual({ reached: true, remaining: null });
  expect(sessionLimitView(10, 12, false)).toEqual({ reached: true, remaining: null });
  expect(sessionLimitView(10, 10, true)).toEqual({ reached: false, remaining: null });
});

test("session limit: refusal is its own class, apart from busy / quota / generic", () => {
  expect(sendErrorKind(403, "ai_session_limit")).toBe("sessionLimit");
  expect(sendErrorKind(409, "conflict")).toBe("busy");
  expect(sendErrorKind(403, "ai_quota_exceeded")).toBe("other");
  expect(sendErrorKind(0, "network")).toBe("other");
});
