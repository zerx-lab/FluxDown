import { describe, expect, test } from "bun:test";
import { Session } from "../src/innertube.js";
import { fetchPlaylist, parsePage } from "../src/playlist.js";
import { installFlux } from "./flux-shim.ts";

const lockup = (id: string, title?: string) => ({
  lockupViewModel: {
    contentId: id,
    contentType: "LOCKUP_CONTENT_TYPE_VIDEO",
    metadata: { lockupMetadataViewModel: title === undefined ? { image: {} } : { title: { content: title } } },
  },
});
const continuation = (token: string) => ({
  continuationItemViewModel: { continuationCommand: { innertubeCommand: { continuationCommand: { token } } } },
});

describe("playlist pages", () => {
  test("reads lockups, legacy renderers and both continuation shapes", () => {
    const page = parsePage([
      lockup("aaaaaaaaaaa", "A"),
      lockup("bbbbbbbbbbb"),
      { playlistVideoRenderer: { videoId: "ccccccccccc", title: { runs: [{ text: "C" }, { text: "!" }] }, isPlayable: false } },
      { continuationItemRenderer: { continuationEndpoint: { continuationCommand: { token: "legacy" } } } },
    ]);
    expect(page.entries).toEqual([
      { id: "aaaaaaaaaaa", title: "A", playable: true },
      { id: "bbbbbbbbbbb", title: "", playable: false },
      { id: "ccccccccccc", title: "C!", playable: false },
    ]);
    expect(page.token).toBe("legacy");
  });

  test("follows continuations up to the limit, skipping unavailable and duplicate videos", async () => {
    const pages: Record<string, unknown> = {
      first: {
        metadata: { playlistMetadataRenderer: { title: "Mix", author: "Fixture Curator" } },
        contents: { tabs: [lockup("aaaaaaaaaaa", "A"), lockup("bbbbbbbbbbb"), continuation("t1")] },
      },
      t1: { onResponseReceivedActions: [{ appendContinuationItemsAction: { continuationItems: [lockup("aaaaaaaaaaa", "A again"), lockup("ddddddddddd", "[Private video]"), lockup("eeeeeeeeeee", "E"), continuation("t2")] } }] },
      t2: { onResponseReceivedActions: [{ appendContinuationItemsAction: { continuationItems: [lockup("fffffffffff", "F"), lockup("ggggggggggg", "G"), continuation("t2")] } }] },
    };
    const shim = installFlux([
      (req) => {
        if (!req.url.includes("/youtubei/v1/browse")) return undefined;
        const body = JSON.parse(req.body ?? "{}");
        return { status: 200, body: JSON.stringify(pages[body.continuation ?? "first"]) };
      },
    ]);
    const result = await fetchPlaylist(new Session(""), "PLx", 3);
    expect(result).toEqual({ title: "Mix", author: "Fixture Curator", entries: [{ id: "aaaaaaaaaaa", title: "A" }, { id: "eeeeeeeeeee", title: "E" }, { id: "fffffffffff", title: "F" }] });
    const first = JSON.parse(shim.requests[0]!.body ?? "{}");
    expect(first).toMatchObject({ browseId: "VLPLx", params: "wgYCCAA=", context: { client: { clientName: "WEB" } } });

    // 续页 token 自我循环时停止，而不是无限请求。
    const all = await fetchPlaylist(new Session(""), "PLx", 100);
    expect(all.entries.map((e) => e.id)).toEqual(["aaaaaaaaaaa", "eeeeeeeeeee", "fffffffffff", "ggggggggggg"]);
  });

  test("reports an inaccessible playlist from the error alert", async () => {
    installFlux([
      () => ({
        status: 200,
        body: JSON.stringify({ alerts: [{ alertRenderer: { type: "ERROR", text: { runs: [{ text: "The playlist does not exist." }] } } }] }),
      }),
    ]);
    await expect(fetchPlaylist(new Session(""), "PLnope", 10)).rejects.toThrow("播放列表不可访问：The playlist does not exist.");
  });
});
