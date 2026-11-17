/**
 * 响应来源追踪（纯模块，零浏览器依赖，可在 bun 下测试）。
 *
 * MAIN world 的 hooks 给 fetch / XHR 读出的缓冲区与流打上来源 URL；buffer 经
 * slice / set 复制后标记沿拷贝传递，最终在 SourceBuffer.appendBuffer 处按数据
 * 归因到具体响应。无法归因（页面自己拼接过数据）时退化到 {@link RecentResponses}
 * 的时间窗口。
 */

/** 单个对象最多保留的来源 URL 数，防止被合并的缓冲区无限累积。 */
const MAX_TAGS_PER_OBJECT = 16;

const tags = new WeakMap<object, Set<string>>();
let taggedAny = false;

/** 是否曾打过任何标记；为 false 时调用方可直接跳过传播（热路径快速出口）。 */
export function hasAnyTag(): boolean {
  return taggedAny;
}

/** 标记载体：ArrayBuffer / Blob 本身，或视图对应的底层 buffer。 */
function carrier(value: unknown): object | null {
  if (value === null || typeof value !== "object") return null;
  if (ArrayBuffer.isView(value)) return value.buffer as object;
  return value;
}

/** 给缓冲区（或视图的底层 buffer、Blob）打上来源 URL。 */
export function tag(target: unknown, url: string): void {
  const key = carrier(target);
  if (!key || !url) return;
  let set = tags.get(key);
  if (!set) {
    set = new Set();
    tags.set(key, set);
  }
  if (set.has(url)) return;
  if (set.size >= MAX_TAGS_PER_OBJECT) {
    const oldest = set.values().next().value;
    if (oldest !== undefined) set.delete(oldest);
  }
  set.add(url);
  taggedAny = true;
}

/** 读取来源 URL；视图取其 `.buffer`（subarray 与原视图共享同一 buffer，标记自然相同）。 */
export function tagsOf(target: unknown): ReadonlySet<string> | undefined {
  const key = carrier(target);
  return key ? tags.get(key) : undefined;
}

/** 把 src 的来源并入 dst（slice / set 等拷贝路径）。 */
export function propagate(src: unknown, dst: unknown): void {
  const from = tagsOf(src);
  if (!from || from.size === 0) return;
  for (const url of from) tag(dst, url);
}

/** ISO BMFF 片段 / 文件开头可能出现的顶层 box。 */
const BMFF_HEAD_BOXES: ReadonlySet<string> = new Set(["ftyp", "styp", "moof", "sidx", "moov"]);
const TS_PACKET = 188;

/**
 * 按魔数识别响应正文是否其实是音视频分片：站点常把分片伪装成图片（`video0.jpeg` + `image/jpeg`），
 * 只看 URL / CT 会把它当图片丢掉，MSE 归因随之没有任何证据。TS 要求连续两个包的同步字节，
 * 避免把以 `G` 开头的 GIF 误判；ISO BMFF 看第一个 box 的类型。
 */
export function mediaMagicMime(data: ArrayBuffer | ArrayBufferView): "video/mp2t" | "video/mp4" | null {
  const bytes = ArrayBuffer.isView(data)
    ? new Uint8Array(data.buffer, data.byteOffset, Math.min(data.byteLength, TS_PACKET + 1))
    : new Uint8Array(data, 0, Math.min(data.byteLength, TS_PACKET + 1));
  if (bytes.length > TS_PACKET && bytes[0] === 0x47 && bytes[TS_PACKET] === 0x47) return "video/mp2t";
  if (bytes.length >= 8) {
    const box = String.fromCharCode(bytes[4], bytes[5], bytes[6], bytes[7]);
    if (BMFF_HEAD_BOXES.has(box)) return "video/mp4";
  }
  return null;
}

export interface RecentResponse {
  url: string;
  byteLength: number;
  endTs: number;
}

/** 最近完成的媒体响应环形缓冲（容量 64）。 */
export class RecentResponses {
  private readonly items: RecentResponse[] = [];

  constructor(private readonly capacity = 64) {}

  push(url: string, byteLength: number, endTs: number): void {
    this.items.push({ url, byteLength, endTs });
    if (this.items.length > this.capacity) this.items.shift();
  }

  /** 结束时间落在 [ts - ms, ts] 内的响应。 */
  withinWindow(ts: number, ms = 3000): RecentResponse[] {
    return this.items.filter((r) => r.endTs <= ts && ts - r.endTs <= ms);
  }
}
