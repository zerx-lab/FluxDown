import { afterEach, expect, expectTypeOf, test } from "bun:test";
import { defineArchive } from "./runtime";
import type { ArchiveContext, ArchiveResult } from "./types/flux";

afterEach(() => {
  Reflect.deleteProperty(globalThis, "archive");
});

test("archive accepts synchronous and async folder decisions, including no result", () => {
  expectTypeOf<
    (ctx: ArchiveContext) => ArchiveResult | null | undefined
  >().toMatchTypeOf<typeof globalThis.archive>();
  expectTypeOf<
    (ctx: ArchiveContext) => Promise<ArchiveResult | null | undefined>
  >().toMatchTypeOf<typeof globalThis.archive>();
});

test("archive rejects callbacks with an invalid folder result", () => {
  expectTypeOf<
    (ctx: ArchiveContext) => { folderName: number }
  >().not.toMatchTypeOf<typeof globalThis.archive>();
  expectTypeOf<
    (ctx: ArchiveContext) => Promise<{ folderName: number }>
  >().not.toMatchTypeOf<typeof globalThis.archive>();
});

test("defineArchive installs globalThis.archive and returns the callback", async () => {
  const sync = defineArchive((ctx) => ({ folderName: ctx.fileName.slice(0, 3) }));
  expect(Reflect.get(globalThis, "archive")).toBe(sync);
  expect(sync({ fileName: "abcdef.mkv" })).toEqual({ folderName: "abc" });

  const asyncFn = defineArchive(async () => null);
  expect(Reflect.get(globalThis, "archive")).toBe(asyncFn);
  expect(await asyncFn({ fileName: "a.mkv" })).toBeNull();

  const none = defineArchive(() => undefined);
  expect(none({ fileName: "a.mkv" })).toBeUndefined();
});

test("defineArchive rejects callbacks with an invalid folder result", () => {
  // @ts-expect-error folderName must be a string
  defineArchive(() => ({ folderName: 1 }));
  // @ts-expect-error folderName must be a string
  defineArchive(async () => ({ folderName: 1 }));
});
