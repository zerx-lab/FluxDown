import { describe, expect, test } from "bun:test";
import { RecentResponses, mediaMagicMime, propagate, tag, tagsOf } from "./provenance";

describe("provenance 标记传递", () => {
  test("ArrayBuffer.slice 与 Uint8Array.set 之后标记仍可传递", () => {
    const src = new Uint8Array([1, 2, 3, 4]);
    tag(src, "https://cdn.example.com/seg-1.m4s");

    // slice 得到新 buffer：宿主 hook 会在拷贝点调用 propagate
    const sliced = src.buffer.slice(0, 2);
    expect(tagsOf(sliced)).toBeUndefined();
    propagate(src, sliced);
    expect([...(tagsOf(sliced) ?? [])]).toEqual(["https://cdn.example.com/seg-1.m4s"]);

    // set 把 src 拷进另一块内存
    const dst = new Uint8Array(8);
    dst.set(src, 2);
    propagate(src, dst);
    expect(tagsOf(dst)?.has("https://cdn.example.com/seg-1.m4s")).toBe(true);
  });

  test("subarray 共享同一 buffer，标记相同", () => {
    const whole = new Uint8Array(16);
    tag(whole, "https://cdn.example.com/a.m4s");
    const part = whole.subarray(4, 8);
    expect(tagsOf(part)).toBe(tagsOf(whole));
  });

  test("没有标记的来源不会污染目标", () => {
    const a = new Uint8Array(4);
    const b = new Uint8Array(4);
    propagate(a, b);
    expect(tagsOf(b)).toBeUndefined();
  });
});

describe("RecentResponses", () => {
  test("withinWindow 只返回窗口内的响应", () => {
    const recent = new RecentResponses();
    recent.push("https://x/old.m4s", 10, 1_000);
    recent.push("https://x/new.m4s", 10, 9_000);
    recent.push("https://x/future.m4s", 10, 20_000);
    const urls = recent.withinWindow(10_000, 3_000).map((r) => r.url);
    expect(urls).toEqual(["https://x/new.m4s"]);
  });

  test("超出容量时淘汰最旧项", () => {
    const recent = new RecentResponses(2);
    recent.push("a", 1, 1);
    recent.push("b", 1, 2);
    recent.push("c", 1, 3);
    expect(recent.withinWindow(3, 10).map((r) => r.url)).toEqual(["b", "c"]);
  });
});

describe("mediaMagicMime", () => {
  test("伪装成图片的 TS / fMP4 分片按魔数识别", () => {
    const ts = new Uint8Array(TS_LEN);
    ts[0] = 0x47;
    ts[188] = 0x47;
    expect(mediaMagicMime(ts.buffer)).toBe("video/mp2t");
    const moof = new Uint8Array([0, 0, 0, 24, 0x6d, 0x6f, 0x6f, 0x66]);
    expect(mediaMagicMime(moof)).toBe("video/mp4");
    // 视图从 buffer 中段开始时按视图偏移读
    const padded = new Uint8Array(16);
    padded.set([0, 0, 0, 24, 0x66, 0x74, 0x79, 0x70], 8);
    expect(mediaMagicMime(padded.subarray(8))).toBe("video/mp4");
  });

  test("真图片与过短数据不判为媒体", () => {
    const gif = new Uint8Array(TS_LEN);
    gif.set([0x47, 0x49, 0x46, 0x38, 0x39, 0x61]);
    expect(mediaMagicMime(gif)).toBeNull();
    const jpeg = new Uint8Array(TS_LEN);
    jpeg.set([0xff, 0xd8, 0xff, 0xe0, 0, 0x10, 0x4a, 0x46]);
    expect(mediaMagicMime(jpeg)).toBeNull();
    expect(mediaMagicMime(new Uint8Array([0x47]))).toBeNull();
  });

  test("图片头 + TS 的分片（1×1 PNG 前缀）按图片后的同步字节识别", () => {
    const png = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    const prefixed = new Uint8Array(70 + 4 * 188);
    prefixed.set(png);
    for (let i = 0; i < 4; i++) prefixed[70 + i * 188] = 0x47;
    expect(mediaMagicMime(prefixed)).toBe("video/mp2t");
    // 只有零星 0x47 的真图片不判为媒体
    const image = new Uint8Array(4096);
    image.set(png);
    image[100] = 0x47;
    image[288] = 0x47;
    expect(mediaMagicMime(image)).toBeNull();
    // 不是图片头时不在中段找同步字节
    const plain = prefixed.slice();
    plain.fill(0, 0, 8);
    expect(mediaMagicMime(plain)).toBeNull();
  });
});

const TS_LEN = 376;
