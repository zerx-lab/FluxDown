import { describe, expect, mock, test } from "bun:test";
import { compileBundled } from "@/rules/engine";
import { checkElement } from "@/page/dom-scan";
import type { ElementState } from "./protocol";

// storage.session 缺省：不持久化；tab-state 只在 installTabState 里注册浏览器监听（本测试不调用）。
mock.module("wxt/browser", () => ({ browser: { storage: {}, tabs: {} } }));
const engine = compileBundled();
mock.module("./rules-store", () => ({ getEngine: () => engine }));
// 必须先 mock 再加载被测模块（与 settings.test.ts 同一约定），故此处动态导入。
const {
  getElements,
  getResources,
  getRetiredElements,
  ingestResources,
  isCurrentFramePage,
  registerSession,
  setSessionHooks,
  syncTabPageUrl,
  unseenEvidence,
  updateElements,
} = await import("./tab-state");

let nextTab = 1;

function el(key: string, overrides: Partial<ElementState> = {}): Omit<ElementState, "frameId"> {
  return {
    key,
    tag: "video",
    playing: false,
    visibleRatio: 1,
    rect: { w: 800, h: 450 },
    videoWidth: 0,
    videoHeight: 0,
    currentSrc: "",
    lastPlayedAt: 0,
    lastInteractionAt: 0,
    bindings: [],
    title: "",
    encrypted: false,
    tracks: [],
    ...overrides,
  };
}

const mse = (msId: number, url: string, hid = "h1") => ({
  msId,
  hid,
  bindings: [{ sbId: msId, mime: "video/mp4", kind: "video" as const, exact: [url], temporal: [] }],
});

describe("元素播放会话归档", () => {
  test("复用元素换了 MediaSource：上一会话归档，标题取它播放时的标题而不是换片后的 DOM 文案", () => {
    const tab = nextTab++;
    registerSession(tab, 0, "s");
    updateElements(tab, 0, "s", [el("s:1", { ...mse(1, "https://cdn/a"), playing: true, title: "第7集" })], [], false);
    // 换片前 DOM 已经切到下一条的描述，随后才换上新的 MediaSource
    updateElements(tab, 0, "s", [el("s:1", { ...mse(1, "https://cdn/a"), title: "第8集" })], [], false);
    updateElements(tab, 0, "s", [el("s:1", { ...mse(2, "https://cdn/b"), playing: true, title: "第8集" })], [], false);
    const retired = getRetiredElements(tab);
    expect(retired.map((r) => [r.title, r.bindings[0].exact[0], r.playing])).toEqual([["第7集", "https://cdn/a", false]]);
    expect(retired[0].key).not.toBe("s:1");
    expect(getElements(tab).map((e) => e.msId)).toEqual([2]);
  });

  test("会话又回到元素上（重注入后重新上报同一会话）：不保留重复的历史项", () => {
    const tab = nextTab++;
    registerSession(tab, 0, "old");
    updateElements(tab, 0, "old", [el("old:1", mse(1, "https://cdn/a"))], [], false);
    registerSession(tab, 0, "new");
    expect(getRetiredElements(tab)).toHaveLength(1);
    updateElements(tab, 0, "new", [el("new:1", mse(1, "https://cdn/a"))], [], true);
    expect(getRetiredElements(tab)).toHaveLength(0);
    expect(getElements(tab).map((e) => e.key)).toEqual(["new:1"]);
  });

  test("没有来源证据的元素被移除时不归档", () => {
    const tab = nextTab++;
    updateElements(tab, 0, "s", [el("s:1")], [], false);
    updateElements(tab, 0, "s", [], ["s:1"], false);
    expect(getRetiredElements(tab)).toHaveLength(0);
    expect(getElements(tab)).toHaveLength(0);
  });

  test("帧换了文档（钩子 hid 变化）：上一文档的 MSE 历史会话丢弃；hid 不变（仅重注入）则保留", () => {
    const tab = nextTab++;
    registerSession(tab, 0, "a");
    updateElements(tab, 0, "a", [el("a:1", mse(1, "https://cdn/a", "h1"))], [], false);
    registerSession(tab, 0, "b");
    setSessionHooks(tab, 0, "b", { protocol: 1, hid: "h1", late: false });
    expect(getRetiredElements(tab)).toHaveLength(1);
    registerSession(tab, 0, "c");
    setSessionHooks(tab, 0, "c", { protocol: 1, hid: "h2", late: false });
    expect(getRetiredElements(tab)).toHaveLength(0);
  });
});

describe("sensor 会话登记", () => {
  test("不认识的 sid 发来增量：要求全量重发；全量本身不再要求", () => {
    const tab = nextTab++;
    expect(updateElements(tab, 0, "s", [el("s:1")], [], false)).toBe(true);
    expect(updateElements(tab, 0, "s", [el("s:1")], [], false)).toBe(false);
    const other = nextTab++;
    expect(updateElements(other, 0, "s", [el("s:1")], [], true)).toBe(false);
  });

  test("同帧出现新 sid：旧实例的元素被移除（它们不会再更新），其它帧不受影响", () => {
    const tab = nextTab++;
    updateElements(tab, 0, "old", [el("old:1"), el("old:2")], [], false);
    updateElements(tab, 3, "sub", [el("sub:1")], [], false);
    registerSession(tab, 0, "new");
    expect(getElements(tab).map((e) => e.key)).toEqual(["sub:1"]);
  });

  test("全量快照删掉本 sid 中缺席的元素，其它帧不受影响", () => {
    const tab = nextTab++;
    updateElements(tab, 0, "s", [el("s:1"), el("s:2")], [], false);
    updateElements(tab, 2, "t", [el("t:1")], [], false);
    updateElements(tab, 0, "s", [el("s:2")], [], true);
    expect(getElements(tab).map((e) => e.key).sort()).toEqual(["s:2", "t:1"]);
  });
});

describe("按元素证据补报资源", () => {
  test("只补资源表里没有的 http(s) 地址，按 SourceBuffer 的 MIME 给类型线索；同一地址只检查一次", () => {
    const tab = nextTab++;
    const elements = [
      el("s:1", {
        bindings: [
          { sbId: 1, mime: 'audio/mp4; codecs="mp4a.40.2"', kind: "audio", exact: ["https://cdn/x/a.mp4", "blob:https://p/1"], temporal: [] },
        ],
      }),
    ];
    updateElements(tab, 0, "s", elements, [], false);
    const missing = unseenEvidence(tab, elements);
    expect(missing.map((m) => [m.url, m.mimeType, m.detectedBy])).toEqual([["https://cdn/x/a.mp4", "audio/mp4", "fetch-intercept"]]);
    expect(unseenEvidence(tab, elements)).toEqual([]);
  });
});

describe("嵌入 PDF 入库", () => {
  const scanCtx = { classify: (url: string) => engine.classify(url), sniffImages: false, keyOf: () => undefined };

  test("<embed type=application/pdf> 指向无扩展名地址：按作者声明的 MIME 归为文档，默认视图可见", () => {
    const tab = nextTab++;
    const embed = { tagName: "EMBED", src: "https://x.com/preview/42", type: "Application/PDF" } as unknown as Element;
    ingestResources(tab, "https://x.com/doc", checkElement(embed, "dom-scan", scanCtx));
    expect(getResources(tab).map((r) => [r.url, r.type, r.mimeType, r.confidence])).toEqual([
      ["https://x.com/preview/42", "document", "application/pdf", "medium"],
    ]);
  });

  test("<object> 未声明 type 时只靠地址分类，不臆造 MIME", () => {
    const tab = nextTab++;
    const object = { tagName: "OBJECT", data: "https://x.com/preview/43", type: "" } as unknown as Element;
    expect(checkElement(object, "dom-scan", scanCtx)).toEqual([
      { url: "https://x.com/preview/43", type: "other", detectedBy: "dom-scan" },
    ]);
    ingestResources(tab, "https://x.com/doc", checkElement(object, "dom-scan", scanCtx));
    expect(getResources(tab).map((r) => [r.type, r.confidence])).toEqual([["other", "low"]]);
  });

  test("接口路径下的 PDF 响应入库复核时不被路径噪音规则丢弃；同路径无 MIME 的请求仍被丢弃", () => {
    const tab = nextTab++;
    ingestResources(tab, "https://x.com/doc", [
      {
        url: "https://x.com/api/v1/files/42/content",
        type: "document",
        mimeType: "application/pdf",
        size: 5 * 1024 * 1024,
        detectedBy: "webRequest",
      },
      { url: "https://x.com/api/v1/files/42/meta", type: "other", detectedBy: "dom-scan" },
    ]);
    expect(getResources(tab).map((r) => [r.url, r.type, r.confidence])).toEqual([
      ["https://x.com/api/v1/files/42/content", "document", "high"],
    ]);
  });
});

describe("帧上报的页面归属", () => {
  test("顶层帧按当前投影判定，子帧（嵌入播放器）的 location 不是 tab 页面也接受", () => {
    const tab = nextTab++;
    syncTabPageUrl(tab, "https://site.example/watch/1.html", "document");
    expect(isCurrentFramePage(tab, 0, "https://site.example/watch/1.html")).toBe(true);
    expect(isCurrentFramePage(tab, 0, "https://site.example/watch/0.html")).toBe(false);
    expect(isCurrentFramePage(tab, 82, "https://player.example/embed/abc#site.example")).toBe(true);
  });
});
