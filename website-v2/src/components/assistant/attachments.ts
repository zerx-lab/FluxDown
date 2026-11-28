/**
 * 提问图片的浏览器侧准备：类型与大小校验、超限图片缩放重编码、读成 data URL。
 * 上限来自 `GET ai/config`（FluxCloud `ai::attachments`：每次最多 `maxImages` 张、单张解码后 ≤ `maxImageBytes`）；
 * FluxCloud 按文件头再判定一次格式与大小。
 */
import type { AiConfig } from "@/lib/account/types";
import { withBase } from "@/lib/base";

/** FluxCloud 直接接受的图片类型（按文件头识别）；其它位图先在浏览器里转 PNG，SVG 等矢量 / 可执行格式一律不收。 */
export const IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"] as const;
/** 文件选择器：任意图片（HEIC / BMP / AVIF 等由 `prepareImage` 转 PNG）。 */
export const IMAGE_ACCEPT = "image/*";

/** 缩放重编码时的最长边起点；仍超限就按比例继续缩小。 */
const MAX_EDGE = 2560;
const SHRINK = 0.75;
const MAX_ATTEMPTS = 5;
const QUALITY = 0.85;

export interface Attachment {
  /** 本地 key（React 列表与移除用）。 */
  key: string;
  /** `data:image/...;base64,...`，预览与上传共用。 */
  url: string;
}

export type AttachError = "type" | "size" | "read";

export type Prepared = { ok: true; attachment: Attachment } | { ok: false; error: AttachError };

const isImageType = (type: string): boolean => (IMAGE_TYPES as readonly string[]).includes(type);

/** 已存附件的地址（同源 BFF，浏览器直接用 `<img src>` 取，会话 Cookie 鉴权）。 */
export const attachmentUrl = (id: string): string => withBase(`/api/account/ai/attachments/${encodeURIComponent(id)}`);

/** `ai/config` → 输入框的图片限制；不支持图片时为 null（不显示添加按钮，粘贴 / 拖入给出提示）。 */
export function imageLimits(config: AiConfig): { max: number; maxBytes: number } | null {
  return config.imagesSupported && config.maxImages > 0 && config.maxImageBytes > 0
    ? { max: config.maxImages, maxBytes: config.maxImageBytes }
    : null;
}

function readDataUrl(blob: Blob): Promise<string | null> {
  const { promise, resolve } = Promise.withResolvers<string | null>();
  const reader = new FileReader();
  reader.onload = () => resolve(typeof reader.result === "string" ? reader.result : null);
  reader.onerror = () => resolve(null);
  reader.readAsDataURL(blob);
  return promise;
}

function toBlob(canvas: HTMLCanvasElement, type: string): Promise<Blob | null> {
  const { promise, resolve } = Promise.withResolvers<Blob | null>();
  canvas.toBlob(resolve, type, QUALITY);
  return promise;
}

/**
 * 把超限的位图缩到 `maxBytes` 以内：按最长边 2560 起步、每次缩 25%，优先 WebP（保留透明），
 * 浏览器不支持 WebP 编码（`toBlob` 回落成 PNG）时改用 JPEG。缩不下来返回 null。
 */
async function shrink(file: Blob, maxBytes: number): Promise<Blob | null> {
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(file);
  } catch {
    return null;
  }
  try {
    const canvas = document.createElement("canvas");
    const context = canvas.getContext("2d");
    if (!context) return null;
    let scale = Math.min(1, MAX_EDGE / Math.max(bitmap.width, bitmap.height));
    for (let attempt = 0; attempt < MAX_ATTEMPTS; attempt += 1) {
      canvas.width = Math.max(1, Math.round(bitmap.width * scale));
      canvas.height = Math.max(1, Math.round(bitmap.height * scale));
      context.clearRect(0, 0, canvas.width, canvas.height);
      context.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
      let blob = await toBlob(canvas, "image/webp");
      if (blob && blob.type !== "image/webp") {
        // 不支持 WebP 编码：JPEG 没有透明通道，先铺白底
        context.globalCompositeOperation = "destination-over";
        context.fillStyle = "#fff";
        context.fillRect(0, 0, canvas.width, canvas.height);
        context.globalCompositeOperation = "source-over";
        blob = await toBlob(canvas, "image/jpeg");
      }
      if (blob && isImageType(blob.type) && blob.size <= maxBytes) return blob;
      scale *= SHRINK;
    }
    return null;
  } finally {
    bitmap.close();
  }
}

/**
 * 云端不收的格式（HEIC / BMP / AVIF / TIFF 等）交给浏览器解码并转成 PNG；浏览器解不开（或是矢量图）返回 null。
 * 解码走 `createImageBitmap`，只取位图像素，SVG 一律拒绝。
 */
async function convertToPng(file: Blob): Promise<Blob | null> {
  if (file.type === "image/svg+xml") return null;
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(file);
  } catch {
    return null;
  }
  try {
    if (bitmap.width === 0 || bitmap.height === 0) return null;
    const canvas = document.createElement("canvas");
    canvas.width = bitmap.width;
    canvas.height = bitmap.height;
    const context = canvas.getContext("2d");
    if (!context) return null;
    context.drawImage(bitmap, 0, 0);
    const blob = await toBlob(canvas, "image/png");
    return blob?.type === "image/png" ? blob : null;
  } finally {
    bitmap.close();
  }
}

let seq = 0;

/**
 * 校验并准备一张图片：云端接受的格式原样使用，其余图片先转 PNG（解码失败才报 `type`）；
 * 超过 `maxBytes` 的静态图自动缩小（GIF 缩放会丢动画，直接拒绝）。
 */
export async function prepareImage(file: File, maxBytes: number): Promise<Prepared> {
  let blob: Blob = file;
  if (!isImageType(file.type)) {
    // 类型为空（部分浏览器给不出 HEIC 的 MIME）也试着解码；明确不是图片的直接拒绝
    if (file.type !== "" && !file.type.startsWith("image/")) return { ok: false, error: "type" };
    const png = await convertToPng(file);
    if (!png) return { ok: false, error: "type" };
    blob = png;
  }
  if (blob.size > maxBytes) {
    if (blob.type === "image/gif") return { ok: false, error: "size" };
    const shrunk = await shrink(blob, maxBytes);
    if (!shrunk) return { ok: false, error: "size" };
    blob = shrunk;
  }
  const url = await readDataUrl(blob);
  if (url === null || !url.startsWith("data:image/")) return { ok: false, error: "read" };
  seq += 1;
  return { ok: true, attachment: { key: `img-${seq}`, url } };
}

/** 从剪贴板 / 拖放 / 文件选择里挑出文件（非图片也留下，交给 `prepareImage` 报类型错误）。 */
export function filesOf(list: FileList | DataTransferItemList | null | undefined): File[] {
  if (!list) return [];
  const out: File[] = [];
  for (const item of Array.from(list as ArrayLike<File | DataTransferItem>)) {
    if (item instanceof File) out.push(item);
    else if (item.kind === "file") {
      const file = item.getAsFile();
      if (file) out.push(file);
    }
  }
  return out;
}
