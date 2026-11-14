import { describe, expect, test } from "bun:test";
import { youtubeMiniplayerSafeTop } from "./miniplayer-position";

describe("YouTube mini-player download badge", () => {
  test("moves the badge below the close-button strip", () => {
    expect(youtubeMiniplayerSafeTop(200, 400, 28, 238)).toBe(252);
  });

  test("uses the actual close-button edge when it is lower", () => {
    expect(youtubeMiniplayerSafeTop(200, 400, 28, 274)).toBe(282);
  });

  test("never places the badge over the close button on a tiny player", () => {
    expect(youtubeMiniplayerSafeTop(200, 277, 28, 250)).toBeNull();
  });

  test("supports larger touch targets without escaping the video", () => {
    expect(youtubeMiniplayerSafeTop(200, 400, 40, 238)).toBe(252);
    expect(youtubeMiniplayerSafeTop(200, 285, 40, 260)).toBeNull();
  });
});
