import { afterEach, expectTypeOf, test } from "bun:test";
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

test("archive accepts category routing results and rejects mixed destinations", () => {
  expectTypeOf<
    (ctx: ArchiveContext) => { category: "audio" } | null
  >().toMatchTypeOf<typeof globalThis.archive>();
  expectTypeOf<
    (ctx: ArchiveContext) => { category: "audio"; directories: string[] }
  >().not.toMatchTypeOf<typeof globalThis.archive>();
  expectTypeOf<
    (ctx: ArchiveContext) => { category: "custom" }
  >().not.toMatchTypeOf<typeof globalThis.archive>();
});

test("archive rejects callbacks with an invalid folder result", () => {
  expectTypeOf<
    (ctx: ArchiveContext) => { folderName: number }
  >().not.toMatchTypeOf<typeof globalThis.archive>();
  expectTypeOf<
    (ctx: ArchiveContext) => Promise<{ folderName: number }>
  >().not.toMatchTypeOf<typeof globalThis.archive>();
});


test("defineArchive rejects callbacks with an invalid folder result", () => {
  // @ts-expect-error folderName must be a string
  defineArchive(() => ({ folderName: 1 }));
  // @ts-expect-error folderName must be a string
  defineArchive(async () => ({ folderName: 1 }));
});
