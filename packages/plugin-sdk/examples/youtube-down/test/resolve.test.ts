import { beforeEach, describe, expect, test } from "bun:test";
import { urlParam } from "../src/query.js";
import { type FetchRequest, type Route, type SolverRecord, host, installFlux, installSolver } from "./flux-shim.ts";
import "../src/main.js";

type Ctx = { taskId?: string; url?: string; resolverItem?: string; cookies?: string };
type Result = {
  url: string;
  audioUrl: string;
  fileName: string;
  ephemeral: boolean;
  rangeSupported: boolean;
  variants?: { label: string }[];
};
async function resolve(ctx: Ctx): Promise<Result | null> {
  // 入口按插件契约返回 ResolveResult 或 null（本插件自己的输出，形状由下面的断言核对）。
  const out = await host.resolve({ taskId: "t1", ...ctx });
  return out as Result | null;
}

const VIDEO = "dQw4w9WgXcQ";
const gv = (itag: number, n: string) => `https://rr1---sn-x.googlevideo.com/videoplayback?itag=${itag}&n=${n}`;

/** 假 YouTube：iframe_api 给出 `state.player`，各客户端按 `state.byClient` 应答 /player。 */
function fakeYouTube(state: { player: string; byClient: Record<string, unknown> }): Route[] {
  return [
    (req) =>
      req.url.endsWith("/iframe_api")
        ? { status: 200, body: `var scriptUrl = 'https:\\/\\/www.youtube.com\\/s\\/player\\/${state.player}\\/www-widgetapi.vflset\\/www-widgetapi.js';` }
        : undefined,
    (req) => (/\/s\/player\/\w+\/player_ias/.test(req.url) ? { status: 200, body: `var x={signatureTimestamp:20746};// ${req.url}` } : undefined),
    (req) =>
      req.url.includes("/embed/")
        ? {
            status: 200,
            body: `<script>ytcfg.set({"INNERTUBE_CLIENT_VERSION":"2.9","DATASYNC_ID":"uid||","SESSION_INDEX":"0","LOGGED_IN":true,"WEB_PLAYER_CONTEXT_CONFIGS":{"WEB_PLAYER_CONTEXT_CONFIG_ID_EMBEDDED_PLAYER":{"jsUrl":"/s/player/embd5678/player_embed_es6.vflset/zh_CN/base.js","encryptedHostFlags":"flags"}}});</script>`,
          }
        : undefined,
    (req) => {
      if (!req.url.includes("/youtubei/v1/player")) return undefined;
      const client = JSON.parse(req.body ?? "{}").context.client.clientName as string;
      const res = state.byClient[client] ?? { playabilityStatus: { status: "ERROR", reason: "no fake" } };
      return { status: 200, body: JSON.stringify(res) };
    },
  ];
}

const okResponse = {
  playabilityStatus: { status: "OK" },
  videoDetails: { videoId: VIDEO, title: "Never: Gonna/Give", lengthSeconds: "200", channelId: "UCfixture", author: "Fixture Artist" },
  streamingData: {
    adaptiveFormats: [
      {
        itag: 137,
        mimeType: 'video/mp4; codecs="avc1.640028"',
        width: 1920,
        height: 1080,
        bitrate: 4e6,
        contentLength: "1000",
        approxDurationMs: "200000",
        signatureCipher: `s=SIG1&sp=sig&url=${encodeURIComponent(gv(137, "nA"))}`,
      },
      { itag: 140, mimeType: 'audio/mp4; codecs="mp4a.40.2"', bitrate: 128000, contentLength: "500", approxDurationMs: "200000", url: gv(140, "nA") },
    ],
  },
};
const botCheck = { playabilityStatus: { status: "LOGIN_REQUIRED", reason: "Sign in to confirm you’re not a bot" } };
const clientsOf = (requests: FetchRequest[]) =>
  requests.filter((r) => r.url.includes("/youtubei/v1/player")).map((r) => JSON.parse(r.body ?? "{}").context.client.clientName);

describe("video resolve", () => {
  let solver: SolverRecord;
  beforeEach(() => {
    solver = installSolver();
  });

  test("cold start solves with the raw player, caches it, and later resolves reuse the cache", async () => {
    const state = { player: "abcd1234", byClient: { TVHTML5: okResponse, VISIONOS: botCheck } };
    const shim = installFlux(fakeYouTube(state), { quality: "ask" });
    const out = await resolve({ url: `https://youtu.be/${VIDEO}` });
    expect(out).toMatchObject({ archiveMetadata: { platform: "youtube", mediaKind: "post", creatorId: "UCfixture", creatorName: "Fixture Artist" } });
    expect(out).toMatchObject({ fileName: "Never Gonna Give.mp4", ephemeral: true, rangeSupported: true });
    expect(urlParam(out!.url, "sig")).toBe("1GIS");
    expect(urlParam(out!.url, "n")).toBe("N(nA)");
    expect(urlParam(out!.audioUrl, "n")).toBe("N(nA)");
    expect(out!.variants?.map((v) => v.label)).toEqual(["1080p · H.264 · MP4", "仅音频 · AAC · M4A"]);
    expect(solver.calls).toEqual([{ type: "player", requests: [{ type: "n", challenges: ["nA"] }, { type: "sig", challenges: ["SIG1"] }] }]);
    expect(shim.files.get("ejs-player-abcd1234.js")).toStartWith("pre:");
    const sent = JSON.parse(shim.requests.find((r) => r.url.includes("/youtubei/v1/player"))!.body ?? "{}");
    expect(sent.playbackContext.contentPlaybackContext.signatureTimestamp).toBe(20746);

    // 热路径：既不下载播放器 JS，也不再问 iframe_api，直接两个 /player 请求。
    const before = shim.requests.length;
    await resolve({ url: `https://www.youtube.com/watch?v=${VIDEO}` });
    expect(solver.calls[1]?.type).toBe("preprocessed");
    expect(shim.requests.slice(before).map((r) => r.url.replace(/\?.*$/, ""))).toEqual([
      "https://www.youtube.com/youtubei/v1/player",
      "https://www.youtube.com/youtubei/v1/player",
    ]);
  });

  test("a recently cached player stands in for a new rollout, and a failing cache is replaced", async () => {
    const state = { player: "abcd1234", byClient: { TVHTML5: okResponse } as Record<string, unknown> };
    const shim = installFlux(fakeYouTube(state), { quality: "best" });
    await resolve({ url: `https://youtu.be/${VIDEO}` });
    state.player = "eeee9999";
    await resolve({ url: `https://youtu.be/${VIDEO}` });
    expect(shim.requests.filter((r) => r.url.includes("/s/player/")).map((r) => r.url.split("/")[5])).toEqual(["abcd1234"]);

    // 缓存的播放器求解出错：丢弃它，换当前播放器冷启动重来。
    const realSolver = host.ytSolver;
    host.ytSolver = (input) => (input.type === "preprocessed" ? { type: "error", error: "stale" } : realSolver(input));
    const out = await resolve({ url: `https://youtu.be/${VIDEO}` });
    expect(urlParam(out!.url, "n")).toBe("N(nA)");
    expect(shim.files.has("ejs-player-abcd1234.js")).toBe(false);
    expect(shim.files.has("ejs-player-eeee9999.js")).toBe(true);
    expect(JSON.parse(shim.storage.get("ejs.players")!).map((e: { id: string }) => e.id)).toEqual(["eeee9999"]);
  });

  test("age-gated responses fall back to the embedded player client", async () => {
    const state = {
      player: "abcd1234",
      byClient: {
        TVHTML5: { playabilityStatus: { status: "LOGIN_REQUIRED", reason: "Sign in to confirm your age" } },
        VISIONOS: { playabilityStatus: { status: "UNPLAYABLE", reason: "x" } },
        WEB_EMBEDDED_PLAYER: okResponse,
      },
    };
    const shim = installFlux(fakeYouTube(state), { quality: "720" });
    const out = await resolve({ url: `https://youtu.be/${VIDEO}` });
    expect(clientsOf(shim.requests)).toEqual(["TVHTML5", "VISIONOS", "WEB_EMBEDDED_PLAYER"]);
    const embedded = JSON.parse(shim.requests.at(-1)!.body ?? "{}");
    expect(embedded.context.thirdParty).toEqual({ embedUrl: "https://www.reddit.com/" });
    expect(out!.variants).toBeUndefined();
  });

  test("bot checks explain how to pass cookies, with a different hint once cookies were sent", async () => {
    installFlux(fakeYouTube({ player: "abcd1234", byClient: { TVHTML5: botCheck, VISIONOS: botCheck } }));
    await expect(resolve({ url: `https://youtu.be/${VIDEO}` })).rejects.toThrow("从浏览器扩展发起下载");
    await expect(resolve({ url: `https://youtu.be/${VIDEO}`, cookies: "YSC=1" })).rejects.toThrow("当前 Cookie 未通过");
    expect(solver.calls).toEqual([]);
  });

  test("login cookies switch to cookie-capable clients with SAPISIDHASH auth", async () => {
    const shim = installFlux(fakeYouTube({ player: "abcd1234", byClient: { TVHTML5: okResponse } }), { quality: "best" });
    await resolve({ url: `https://youtu.be/${VIDEO}`, cookies: "LOGIN_INFO=l; SAPISID=s; SID=x" });
    expect(clientsOf(shim.requests).sort()).toEqual(["TVHTML5", "WEB_EMBEDDED_PLAYER"]);
    // 会话字段与播放器版本都取自嵌入页（只下载一次），不再拉 1.5 MB 的播放页，也不问 iframe_api。
    const pages = shim.requests.filter((r) => r.method === "GET").map((r) => r.url.replace(/\?.*$/, ""));
    expect(pages).toEqual([`https://www.youtube.com/embed/${VIDEO}`, "https://www.youtube.com/s/player/embd5678/player_ias.vflset/en_US/base.js"]);
    const tv = shim.requests.find((r) => r.url.includes("/youtubei/v1/player") && r.headers["X-YouTube-Client-Name"] === "7")!;
    expect(tv.headers.Authorization).toMatch(/^SAPISIDHASH \d+_[0-9a-f]{40}_u$/);
    expect(tv.headers["X-Goog-AuthUser"]).toBe("0");
    expect(tv.headers["X-Youtube-Bootstrap-Logged-In"]).toBe("true");
    expect(tv.headers.Cookie).toContain("LOGIN_INFO=l");
    expect(tv.headers.Cookie).not.toContain("SOCS=");
    const embedded = JSON.parse(shim.requests.find((r) => r.headers["X-YouTube-Client-Name"] === "56")!.body ?? "{}");
    expect(embedded.playbackContext.contentPlaybackContext.encryptedHostFlags).toBe("flags");
  });

  test("the new-download pre-resolve skips single videos", async () => {
    const shim = installFlux([]);
    expect(await resolve({ taskId: "", url: `https://youtu.be/${VIDEO}` })).toBeNull();
    expect(shim.requests).toEqual([]);
  });
});
