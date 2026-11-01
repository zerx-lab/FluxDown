import { expectTypeOf, test } from "bun:test";
import type { ArchiveContext, ArchiveResult } from "./types/flux";

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
