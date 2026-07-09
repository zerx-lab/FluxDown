import { describe, expect, test } from "bun:test";
import { parseDashJson } from "./dash-manifest";

const BASE = "https://cdn.example.com/";
const PAGE = "https://example.com/watch";

describe("parseDashJson — 标准 DASH JSON 结构识别", () => {
  test("顶层 video[]/audio[] 数组：正确分组 + 按清晰度/码率降序排列", () => {
    const manifest = {
      code: 0,
      dash: {
        duration: 600,
        video: [
          {
            id: 32,
            baseUrl: "video-480.m4s",
            bandwidth: 800_000,
            width: 854,
            height: 480,
            mimeType: "video/mp4",
            codecs: "avc1.64001E",
          },
          {
            id: 80,
            baseUrl: "https://cdn.example.com/video-1080.m4s",
            bandwidth: 3_000_000,
            width: 1920,
            height: 1080,
            mimeType: "video/mp4",
            codecs: "avc1.640032",
          },
          {
            id: 64,
            baseUrl: "https://cdn.example.com/video-720.m4s",
            bandwidth: 1_500_000,
            width: 1280,
            height: 720,
            mimeType: "video/mp4",
            codecs: "avc1.64001F",
          },
        ],
        audio: [
          {
            id: 30216,
            baseUrl: "https://cdn.example.com/audio-lo.m4s",
            bandwidth: 64_000,
            mimeType: "audio/mp4",
            codecs: "mp4a.40.2",
          },
          {
            id: 30280,
            baseUrl: "https://cdn.example.com/audio-hi.m4s",
            bandwidth: 128_000,
            mimeType: "audio/mp4",
            codecs: "mp4a.40.2",
          },
        ],
      },
    };

    const result = parseDashJson(manifest, PAGE);
    expect(result).not.toBeNull();
    expect(result!.video.map((t) => t.height)).toEqual([1080, 720, 480]);
    expect(result!.audio.map((t) => t.bandwidth)).toEqual([128_000, 64_000]);
    // 相对 baseUrl 用传入的 pageUrl 绝对化
    expect(result!.video.find((t) => t.id === 32)!.url).toBe(
      "https://example.com/video-480.m4s",
    );
    // 绝对 baseUrl 保持不变
    expect(result!.video.find((t) => t.id === 80)!.url).toBe(
      "https://cdn.example.com/video-1080.m4s",
    );
  });

  test("嵌套任意深度（结构驱动，不要求固定路径）也能识别", () => {
    const manifest = {
      response: {
        payload: {
          media: {
            video: [{ baseUrl: `${BASE}v1.m4s`, bandwidth: 2_000_000, height: 1080 }],
            audio: [{ baseUrl: `${BASE}a1.m4s`, bandwidth: 128_000 }],
          },
        },
      },
    };
    const result = parseDashJson(manifest, PAGE);
    expect(result).not.toBeNull();
    expect(result!.video).toHaveLength(1);
    expect(result!.audio).toHaveLength(1);
  });

  test("非 DASH 结构（无 video/audio 数组）返回 null", () => {
    expect(parseDashJson({ foo: "bar", list: [1, 2, 3] }, PAGE)).toBeNull();
    expect(parseDashJson({ items: [{ name: "a" }, { name: "b" }] }, PAGE)).toBeNull();
  });

  test("缺 DASH 特征字段的数组元素被过滤；全部过滤后返回 null", () => {
    const manifest = { video: [{ baseUrl: "x.mp4" }], audio: [{ baseUrl: "y.mp4" }] };
    expect(parseDashJson(manifest, PAGE)).toBeNull();
  });

  test("缺字段容错：只有 bandwidth（无 width/height/codecs）仍算有效轨道", () => {
    const manifest = {
      audio: [{ baseUrl: `${BASE}a.m4s`, bandwidth: 64_000 }],
    };
    const result = parseDashJson(manifest, PAGE);
    expect(result).not.toBeNull();
    expect(result!.audio).toHaveLength(1);
    expect(result!.video).toHaveLength(0);
  });

  test("mimeType 与容器矛盾的元素被过滤（video[] 里混入 audio/ mimeType）", () => {
    const manifest = {
      video: [
        { baseUrl: `${BASE}v.m4s`, bandwidth: 2_000_000, height: 1080, mimeType: "video/mp4" },
        { baseUrl: `${BASE}bad.m4s`, bandwidth: 64_000, mimeType: "audio/mp4" },
      ],
    };
    const result = parseDashJson(manifest, PAGE);
    expect(result).not.toBeNull();
    expect(result!.video).toHaveLength(1);
    expect(result!.video[0].mimeType).toBe("video/mp4");
  });

  test("非对象 / null / 原始类型输入不抛异常，返回 null", () => {
    expect(parseDashJson(null, PAGE)).toBeNull();
    expect(parseDashJson(undefined, PAGE)).toBeNull();
    expect(parseDashJson("plain string", PAGE)).toBeNull();
    expect(parseDashJson(42, PAGE)).toBeNull();
    expect(parseDashJson([1, 2, 3], PAGE)).toBeNull();
  });
});
