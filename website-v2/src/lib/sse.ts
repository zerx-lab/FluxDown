/**
 * 增量 Server-Sent Events 帧解析（BFF 转发与浏览器读流共用）。
 *
 * 帧以空行（`\n\n` 或 `\r\n\r\n`）分隔；一帧内的多行 `data:` 以 `\n` 拼接，字段值去掉冒号后的一个空格。
 * 只关心 `data` 字段：`event` / `id` / `retry` 忽略，没有 `data` 的帧（如保活注释）返回 `null`。
 */
const FRAME_END = /\r\n\r\n|\n\n/;

export class SseParser {
  private buffer = "";

  /** 喂入一段文本，返回其中已完整的帧（未完整的部分留到下次）。 */
  push(chunk: string): (string | null)[] {
    this.buffer += chunk;
    const frames: (string | null)[] = [];
    for (let end = FRAME_END.exec(this.buffer); end; end = FRAME_END.exec(this.buffer)) {
      frames.push(frameData(this.buffer.slice(0, end.index)));
      this.buffer = this.buffer.slice(end.index + end[0].length);
    }
    return frames;
  }
}

function frameData(block: string): string | null {
  const data: string[] = [];
  for (const line of block.split(/\r\n|\n/)) {
    if (line === "data") data.push("");
    else if (line.startsWith("data:")) data.push(line.startsWith("data: ") ? line.slice(6) : line.slice(5));
  }
  return data.length === 0 ? null : data.join("\n");
}
