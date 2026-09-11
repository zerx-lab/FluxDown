import { describe, expect, test } from "bun:test";
import {
  buildMediaCandidates,
  countMediaCandidateRows,
} from "./media-candidates";
import type { DashManifest } from "./dash-manifest";
import type { DetectedResource } from "./resource-types";

const PAGE_URL = "https://example.com/watch";

let sequence = 0;
function resource(overrides: Partial<DetectedResource>): DetectedResource {
  sequence += 1;
  return {
    id: `resource-${sequence}`,
    url: `https://cdn.example.com/resource-${sequence}.m4s`,
    filename: "",
    type: "stream",
    size: -1,
    detectedBy: "fetch-intercept",
    detectedAt: sequence,
    tabId: 1,
    pageUrl: PAGE_URL,
    confidence: "high",
    ...overrides,
  };
}

function manifest(
  videoQuery: string,
  audioQuery: string,
  directory = "video",
): DashManifest {
  return {
    video: [
      {
        id: "1080",
        url: `https://cdn.example.com/${directory}/1080.m4s?${videoQuery}`,
        mimeType: "video/mp4",
        codecs: "avc1.640028",
        bandwidth: 4_000_000,
        width: 1920,
        height: 1080,
      },
      {
        id: "360",
        url: `https://cdn.example.com/${directory}/360.m4s?${videoQuery}`,
        mimeType: "video/mp4",
        codecs: "avc1.4d401e",
        bandwidth: 700_000,
        width: 640,
        height: 360,
      },
    ],
    audio: [
      {
        id: "audio",
        url: `https://cdn.example.com/${directory}/audio.m4s?${audioQuery}`,
        mimeType: "audio/mp4",
        codecs: "mp4a.40.2",
        bandwidth: 128_000,
      },
    ],
  };
}

describe("buildMediaCandidates", () => {
  test("consumes raw video/audio tracks already represented by a DASH candidate", () => {
    const currentManifest = manifest("deadline=100&sig=one", "deadline=100&sig=one");
    const resources = [
      resource({
        id: "manifest",
        url: "https://cdn.example.com/play/manifest.json",
        type: "stream",
      }),
      resource({
        id: "video-1080",
        url: "https://cdn.example.com/video/1080.m4s?deadline=100&sig=one",
        type: "video",
        mimeType: "video/mp4",
      }),
      resource({
        id: "video-360",
        url: "https://cdn.example.com/video/360.m4s?deadline=100&sig=one",
        type: "video",
        mimeType: "video/mp4",
      }),
      resource({
        id: "audio",
        url: "https://cdn.example.com/audio/audio.m4s?deadline=100&sig=one",
        type: "audio",
        mimeType: "audio/mp4",
      }),
    ];

    const candidates = buildMediaCandidates(resources, {
      fallbackTitle: "Video",
      videoLabel: "Video",
      manifests: [
        {
          url: "https://cdn.example.com/play/manifest.json",
          manifest: currentManifest,
        },
      ],
    });

    expect(candidates).toHaveLength(1);
    expect(candidates[0].variants.map((variant) => variant.label)).toEqual([
      "1080p",
      "360p",
    ]);
    expect(candidates[0].rawResourceIds).toEqual(
      expect.arrayContaining(["manifest", "video-1080", "video-360", "audio"]),
    );
    expect(countMediaCandidateRows(candidates)).toBe(2);
  });

  test("deduplicates repeated manifests whose CDN signatures rotate", () => {
    const candidates = buildMediaCandidates([], {
      fallbackTitle: "Video",
      videoLabel: "Video",
      manifests: [
        {
          url: "https://cdn.example.com/play/manifest.json?session=one",
          manifest: manifest("deadline=100&sig=one", "deadline=100&sig=one"),
        },
        {
          url: "https://cdn.example.com/play/manifest.json?session=two",
          manifest: manifest("deadline=200&sig=two", "deadline=200&sig=two"),
        },
      ],
    });

    expect(candidates).toHaveLength(1);
    expect(candidates[0].variants).toHaveLength(2);
    expect(candidates[0].variants[0].videoUrl).toContain("deadline=200");
  });

  test("同一清单下不同目录的分片归入候选，且孤立分片不再占用资源数", () => {
    const currentManifest = manifest("deadline=100&sig=one", "deadline=100&sig=one");
    const resources = [
      resource({
        id: "manifest",
        url: "https://cdn.example.com/play/manifest.mpd",
        type: "stream",
      }),
      resource({
        id: "video-segment",
        url: "https://cdn.example.com/play/session-42/video/seg-1.m4s",
        type: "stream",
      }),
      resource({
        id: "orphan-a",
        url: "https://ads.example.com/other-player/a/seg-1.m4s",
        type: "stream",
      }),
      resource({
        id: "orphan-b",
        url: "https://ads.example.com/other-player/b/seg-1.m4s",
        type: "stream",
      }),
    ];

    const candidates = buildMediaCandidates(resources, {
      fallbackTitle: "Video",
      videoLabel: "Video",
      manifests: [{
        url: "https://cdn.example.com/play/manifest.mpd",
        manifest: currentManifest,
      }],
    });

    expect(candidates.filter((candidate) => candidate.downloadable)).toHaveLength(1);
    expect(candidates.find((candidate) => candidate.source === "dash")?.rawResourceIds)
      .toContain("video-segment");
    expect(candidates.find((candidate) => candidate.source === "fragments")?.fragmentCount)
      .toBe(2);
    expect(countMediaCandidateRows(candidates)).toBe(2);
  });

  test("备用 CDN 使用相同媒体路径时归入 DASH 候选", () => {
    const currentManifest = manifest("deadline=100&sig=one", "deadline=100&sig=one");
    const resources = [
      resource({
        id: "manifest",
        url: "https://cdn.example.com/play/manifest.mpd",
        type: "stream",
      }),
      resource({
        id: "video-primary",
        url: "https://cdn.example.com/video/1080.m4s?deadline=100&sig=one",
        type: "video",
      }),
      resource({
        id: "video-mirror",
        url: "https://mirror.example.net/video/1080.m4s?deadline=100&sig=two",
        type: "video",
      }),
      resource({
        id: "audio-mirror",
        url: "https://mirror.example.net/video/audio.m4s?deadline=100&sig=two",
        type: "audio",
      }),
    ];

    const candidates = buildMediaCandidates(resources, {
      fallbackTitle: "Video",
      videoLabel: "Video",
      manifests: [{
        url: "https://cdn.example.com/play/manifest.mpd",
        manifest: currentManifest,
      }],
    });

    expect(candidates.filter((candidate) => candidate.source === "fragments")).toHaveLength(0);
    expect(candidates[0].rawResourceIds).toEqual(
      expect.arrayContaining(["video-mirror", "audio-mirror"]),
    );
    expect(countMediaCandidateRows(candidates)).toBe(2);
  });

  test("当前页面有强关联清单时忽略其他视频的预加载清单和分片", () => {
    const currentManifest = manifest("deadline=100&sig=current", "deadline=100&sig=current", "current");
    const preloadManifest = manifest("deadline=100&sig=preload", "deadline=100&sig=preload", "preload");
    const resources = [
      resource({
        id: "current-video",
        url: "https://cdn.example.com/current/1080.m4s?deadline=100&sig=current",
        type: "video",
      }),
      resource({
        id: "current-audio",
        url: "https://cdn.example.com/current/audio.m4s?deadline=100&sig=current",
        type: "audio",
      }),
      resource({
        id: "preload-video",
        url: "https://cdn.example.com/preload/1080.m4s?deadline=100&sig=preload",
        type: "video",
      }),
      resource({
        id: "preload-audio",
        url: "https://cdn.example.com/preload/audio.m4s?deadline=100&sig=preload",
        type: "audio",
      }),
    ];

    const candidates = buildMediaCandidates(resources, {
      pageTitle: "Current video",
      pageUrl: "https://example.com/watch/current",
      fallbackTitle: "Video",
      videoLabel: "Video",
      manifests: [
        {
          url: "https://example.com/watch/current",
          manifest: currentManifest,
        },
        {
          url: "https://api.example.com/play?id=preload",
          manifest: preloadManifest,
        },
      ],
    });

    expect(candidates.filter((candidate) => candidate.downloadable)).toHaveLength(1);
    expect(candidates.find((candidate) => candidate.source === "dash")?.rawResourceIds)
      .toEqual(expect.arrayContaining(["current-video", "current-audio"]));
    expect(candidates.find((candidate) => candidate.source === "fragments")).toBeUndefined();
    expect(candidates.find((candidate) => candidate.source === "ignored")?.rawResourceIds)
      .toEqual(expect.arrayContaining(["preload-video", "preload-audio"]));
    expect(countMediaCandidateRows(candidates)).toBe(2);
  });
});
