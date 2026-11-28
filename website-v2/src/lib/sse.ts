/**
 * 增量 Server-Sent Events 帧解析（BFF 转发与浏览器读流共用）。
 *
 * 帧以空行（`\n\n` 或 `\r\n\r\n`）分隔；一帧内的多行 `data:` 以 `\n` 拼接，字段值去掉冒号后的一个空格。
 * 关心 `data` 与 `id`（事件序号，断线续读用）：`event` / `retry` 忽略，没有 `data` 的帧（如保活注释）`data` 为 `null`。
 */
const FRAME_END = /\r\n\r\n|\n\n/;

export interface SseEvent {
  /** `id:` 字段（同一帧多行取最后一个；含 NUL 的按规范忽略）；没有为 null。 */
  id: string | null;
  /** `data:` 行以 `\n` 拼接；没有 `data` 行（保活注释等）为 null。 */
  data: string | null;
}

export class SseParser {
  private buffer = "";

  /** 喂入一段文本，返回其中已完整的帧（含 `id`；未完整的部分留到下次）。 */
  pushEvents(chunk: string): SseEvent[] {
    this.buffer += chunk;
    const frames: SseEvent[] = [];
    for (let end = FRAME_END.exec(this.buffer); end; end = FRAME_END.exec(this.buffer)) {
      frames.push(parseFrame(this.buffer.slice(0, end.index)));
      this.buffer = this.buffer.slice(end.index + end[0].length);
    }
    return frames;
  }

  /** 同 `pushEvents`，只要 `data`。 */
  push(chunk: string): (string | null)[] {
    return this.pushEvents(chunk).map((frame) => frame.data);
  }
}

function parseFrame(block: string): SseEvent {
  const data: string[] = [];
  let id: string | null = null;
  for (const line of block.split(/\r\n|\n/)) {
    if (line === "data") data.push("");
    else if (line.startsWith("data:")) data.push(line.startsWith("data: ") ? line.slice(6) : line.slice(5));
    else if (line.startsWith("id:")) {
      const value = line.startsWith("id: ") ? line.slice(4) : line.slice(3);
      if (!value.includes("\0")) id = value;
    }
  }
  return { id, data: data.length === 0 ? null : data.join("\n") };
}
