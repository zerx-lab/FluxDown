import { afterEach, describe, expect, test } from "bun:test";
import {
  applySniffRuleOverrides,
  getDefaultSniffRules,
  groupTrackPairs,
  matchSniffRule,
} from "./resource-types";
import type { DetectedResource } from "./resource-types";

// 规则表是模块级单例：任何用到 applySniffRuleOverrides 的用例结束后都要恢复默认，
// 否则会污染后续用例的判定结果。
afterEach(() => {
  applySniffRuleOverrides(null);
});

describe("matchSniffRule — 后缀命中", () => {
  test("video 后缀 (mp4) 命中", () => {
    expect(matchSniffRule("https://x/a.mp4", undefined, 5_000_000)).toEqual({
      hit: true,
      category: "video",
      blocked: false,
    });
  });

  test("stream 后缀 (m4s) 命中且不受 <10MB 影响 —— B站分片核心场景", () => {
    // 分片文件通常远小于 10MB，minSize:0 意味着命中即收，不做大小丢弃。
    expect(matchSniffRule("https://x/seg.m4s", undefined, 500_000)).toEqual({
      hit: true,
      category: "stream",
      blocked: false,
    });
  });

  test("stream 后缀 (m3u8) 命中", () => {
    expect(matchSniffRule("https://x/i.m3u8", undefined, -1)).toEqual({
      hit: true,
      category: "stream",
      blocked: false,
    });
  });
});

describe("matchSniffRule — 后缀不命中", () => {
  test("image 类别不生成后缀规则 (png) → miss", () => {
    expect(matchSniffRule("https://x/a.png", undefined, -1)).toEqual({
      hit: false,
      category: "other",
      blocked: false,
    });
  });

  test("未知后缀 (html) → miss", () => {
    expect(matchSniffRule("https://x/page.html", undefined, -1)).toEqual({
      hit: false,
      category: "other",
      blocked: false,
    });
  });
});

describe("matchSniffRule — MIME 命中", () => {
  test("video/* 通配命中", () => {
    expect(matchSniffRule("https://x/noext", "video/mp4", -1)).toEqual({
      hit: true,
      category: "video",
      blocked: false,
    });
  });

  test("application/dash+xml 精确命中", () => {
    expect(matchSniffRule("https://x/noext", "application/dash+xml", -1)).toEqual({
      hit: true,
      category: "stream",
      blocked: false,
    });
  });

  test("application/m4s 精确命中", () => {
    expect(matchSniffRule("https://x/noext", "application/m4s", -1)).toEqual({
      hit: true,
      category: "stream",
      blocked: false,
    });
  });
});

describe("matchSniffRule — 后缀优先于 MIME", () => {
  test("URL 后缀命中后，忽略不匹配的 Content-Type", () => {
    expect(matchSniffRule("https://x/a.mp4", "text/html", -1)).toEqual({
      hit: true,
      category: "video",
      blocked: false,
    });
  });
});

describe("matchSniffRule — size 未知时不因 minSize 拦截", () => {
  test("size=-1 时即使规则有 minSize 也不 blocked", () => {
    applySniffRuleOverrides([
      { match: "bin", kind: "ext", category: "other", minSize: 1000, enabled: true },
    ]);
    expect(matchSniffRule("https://x/a.bin", undefined, -1)).toEqual({
      hit: true,
      category: "other",
      blocked: false,
    });
  });

  test("size=0 时即使规则有 minSize 也不 blocked", () => {
    applySniffRuleOverrides([
      { match: "bin", kind: "ext", category: "other", minSize: 1000, enabled: true },
    ]);
    expect(matchSniffRule("https://x/a.bin", undefined, 0)).toEqual({
      hit: true,
      category: "other",
      blocked: false,
    });
  });
});

describe("applySniffRuleOverrides — 自定义规则表行为", () => {
  test("已知 size 小于 minSize → blocked:true, hit:false", () => {
    applySniffRuleOverrides([
      { match: "bin", kind: "ext", category: "other", minSize: 1000, enabled: true },
    ]);
    expect(matchSniffRule("https://x/a.bin", undefined, 500)).toEqual({
      hit: false,
      category: "other",
      blocked: true,
    });
  });

  test("已知 size 大于等于 minSize → hit:true", () => {
    applySniffRuleOverrides([
      { match: "bin", kind: "ext", category: "other", minSize: 1000, enabled: true },
    ]);
    expect(matchSniffRule("https://x/a.bin", undefined, 2000)).toEqual({
      hit: true,
      category: "other",
      blocked: false,
    });
  });

  test("size 未知 (-1) 时不因 minSize 拦截 → hit:true", () => {
    applySniffRuleOverrides([
      { match: "bin", kind: "ext", category: "other", minSize: 1000, enabled: true },
    ]);
    expect(matchSniffRule("https://x/a.bin", undefined, -1)).toEqual({
      hit: true,
      category: "other",
      blocked: false,
    });
  });

  test("enabled:false 的规则命中 → blocked:true, hit:false", () => {
    applySniffRuleOverrides([
      { match: "mp4", kind: "ext", category: "video", minSize: 0, enabled: false },
    ]);
    expect(matchSniffRule("https://x/a.mp4", undefined, -1)).toEqual({
      hit: false,
      category: "video",
      blocked: true,
    });
  });

  test("blacklist:true 的规则命中 → blocked:true, hit:false", () => {
    applySniffRuleOverrides([
      {
        match: "mp4",
        kind: "ext",
        category: "video",
        minSize: 0,
        enabled: true,
        blacklist: true,
      },
    ]);
    expect(matchSniffRule("https://x/a.mp4", undefined, -1)).toEqual({
      hit: false,
      category: "video",
      blocked: true,
    });
  });

  test("传 null 恢复内置默认规则", () => {
    applySniffRuleOverrides([
      { match: "mp4", kind: "ext", category: "video", minSize: 0, enabled: false },
    ]);
    expect(matchSniffRule("https://x/a.mp4", undefined, -1).blocked).toBe(true);

    applySniffRuleOverrides(null);
    expect(matchSniffRule("https://x/a.mp4", undefined, -1)).toEqual({
      hit: true,
      category: "video",
      blocked: false,
    });
  });
});

describe("getDefaultSniffRules", () => {
  test("返回非空的内置规则数组", () => {
    const rules = getDefaultSniffRules();
    expect(rules.length).toBeGreaterThan(0);
  });

  test("包含 m4s → stream 后缀规则（锁定 B站分片修复，防回退）", () => {
    const rules = getDefaultSniffRules();
    const m4sRule = rules.find((r) => r.kind === "ext" && r.match === "m4s");
    expect(m4sRule).toBeDefined();
    expect(m4sRule?.category).toBe("stream");
  });
});

describe("groupTrackPairs — 离散音视频轨道分组", () => {
  let seq = 0;
  function res(overrides: Partial<DetectedResource>): DetectedResource {
    seq += 1;
    return {
      id: `r${seq}`,
      url: `https://example.com/track-${seq}.m4s`,
      filename: `track-${seq}.m4s`,
      type: "stream",
      size: 1000,
      detectedBy: "webRequest",
      detectedAt: Date.now(),
      tabId: 1,
      pageUrl: "https://example.com/watch",
      confidence: "high",
      ...overrides,
    };
  }

  test("双轨分组：video/audio mimeType 各一条 → 单档带 audioUrl", () => {
    const video = res({
      mimeType: "video/mp4",
      size: 50_000_000,
      quality: "1080p",
    });
    const audio = res({ mimeType: "audio/mp4", size: 3_000_000 });
    const groups = groupTrackPairs([video, audio]);
    expect(groups).toHaveLength(1);
    expect(groups[0].quality).toBe("1080p");
    expect(groups[0].videoUrl).toBe(video.url);
    expect(groups[0].audioUrl).toBe(audio.url);
    expect(groups[0].videoRes).toBe(video);
    expect(groups[0].audioRes).toBe(audio);
  });

  test("多清晰度：多条视频轨按 size 降序排列，共享同一条音频轨", () => {
    const v720 = res({ mimeType: "video/mp4", size: 25_000_000, quality: "720p" });
    const audio = res({ mimeType: "audio/mp4", size: 3_000_000 });
    const v1080 = res({ mimeType: "video/mp4", size: 50_000_000, quality: "1080p" });
    const v480 = res({ mimeType: "video/mp4", size: 12_000_000, quality: "480p" });
    const groups = groupTrackPairs([v720, audio, v1080, v480]);
    expect(groups.map((g) => g.quality)).toEqual(["1080p", "720p", "480p"]);
    for (const g of groups) {
      expect(g.audioUrl).toBe(audio.url);
    }
  });

  test("单轨无音频：只有视频资源时 audioUrl/audioRes 缺省", () => {
    const video = res({ mimeType: "video/mp4", size: 8_000_000, quality: "720p" });
    const groups = groupTrackPairs([video]);
    expect(groups).toHaveLength(1);
    expect(groups[0].videoUrl).toBe(video.url);
    expect(groups[0].audioUrl).toBeUndefined();
    expect(groups[0].audioRes).toBeUndefined();
  });

  test("mimeType 缺失回退：两条无 mimeType 资源按 size 分出视频/音频轨", () => {
    const big = res({ size: 40_000_000 });
    const small = res({ size: 2_000_000 });
    const groups = groupTrackPairs([big, small]);
    expect(groups).toHaveLength(1);
    expect(groups[0].videoUrl).toBe(big.url);
    expect(groups[0].audioUrl).toBe(small.url);
    // 缺 quality 字段时按顺位占位命名
    expect(groups[0].quality).toBe("画质1");
  });

  test("非媒体类型资源（document/image）被过滤，不参与分组", () => {
    const doc = res({ type: "document", mimeType: "application/pdf", size: 999_999 });
    const video = res({ mimeType: "video/mp4", size: 8_000_000, quality: "720p" });
    const groups = groupTrackPairs([doc, video]);
    expect(groups).toHaveLength(1);
    expect(groups[0].videoUrl).toBe(video.url);
  });
});
