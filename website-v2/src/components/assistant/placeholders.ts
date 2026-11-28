/**
 * 提问文本里的 `[Image #N]` 占位符（FluxCloud 与 GPUI / Web 同约定）：第 N 张附图（从 1 起）在文本里写成 `[Image #N]`，
 * 云端把图片紧跟在占位符之后发给模型。这里是 GPUI `crates/assistant/src/model.rs` 的移植：插入（与相邻文字补空格）、
 * 删除并重新编号、手动删除占位符后图片随之移除、删到一半的占位符整体删掉，以及历史里把占位符拆出来渲染成图片引用。
 * 纯函数，偏移一律是 UTF-16 下标（与 `textarea.selectionStart` 同口径）。
 */

export const imagePlaceholder = (number: number): string => `[Image #${number}]`;

const OPEN = "[Image #";

interface Span {
  start: number;
  end: number;
  number: number;
}

export interface TextEdit {
  text: string;
  /** 编辑后的光标位置。 */
  caret: number;
}

const clamp = (text: string, index: number): number => Math.min(text.length, Math.max(0, index));

/** 不超过 `index` 的最大字符边界（不把代理对切成两半）。 */
function floorBoundary(text: string, index: number): number {
  const at = clamp(text, index);
  const low = text.charCodeAt(at);
  const high = text.charCodeAt(at - 1);
  return at > 0 && low >= 0xdc00 && low <= 0xdfff && high >= 0xd800 && high <= 0xdbff ? at - 1 : at;
}


/** 文本里全部形如 `[Image #n]` 的占位符（`n` 为不带前导 0 的正整数），按出现顺序。 */
function placeholderSpans(text: string): Span[] {
  const spans: Span[] = [];
  let from = 0;
  for (;;) {
    const start = text.indexOf(OPEN, from);
    if (start < 0) return spans;
    const digitsStart = start + OPEN.length;
    let digitsEnd = digitsStart;
    while (digitsEnd < text.length && text.charCodeAt(digitsEnd) >= 0x30 && text.charCodeAt(digitsEnd) <= 0x39) digitsEnd += 1;
    const digits = text.slice(digitsStart, digitsEnd);
    if (digits !== "" && !digits.startsWith("0") && text[digitsEnd] === "]") {
      spans.push({ start, end: digitsEnd + 1, number: Number(digits) });
      from = digitsEnd + 1;
    } else {
      from = digitsStart;
    }
  }
}

const endsAtBoundary = (before: string): boolean => before === "" || /\s$/.test(before);

/**
 * 在选区 `[start, end)` 处放入第 `number` 张图的占位符（选区被替换，与粘贴一致）。与相邻文字之间补一个空格，
 * 免得占位符黏在词上；该侧已是空白则不重复补。光标落在占位符（及补的空格）之后。
 */
export function insertPlaceholder(text: string, start: number, end: number, number: number): TextEdit {
  const from = floorBoundary(text, Math.min(start, end));
  const to = Math.max(from, floorBoundary(text, Math.max(start, end)));
  const before = text.slice(0, from);
  const after = text.slice(to);
  const lead = before !== "" && !/\s$/.test(before) ? " " : "";
  const trail = after === "" || !/^\s/.test(after) ? " " : "";
  const inserted = `${lead}${imagePlaceholder(number)}${trail}`;
  return { text: before + inserted + after, caret: from + inserted.length };
}

interface Replacement {
  start: number;
  end: number;
  text: string;
}

/** 把升序、互不重叠的替换应用到文本，并把偏移 `caret` 映射到编辑后（落在被替换区间内取区间起点）。 */
function applyReplacements(text: string, edits: readonly Replacement[], caret: number): TextEdit {
  let out = "";
  let cursor = 0;
  let nextCaret = caret;
  for (const edit of edits) {
    out += text.slice(cursor, edit.start) + edit.text;
    cursor = edit.end;
    const delta = edit.text.length - (edit.end - edit.start);
    if (edit.end <= caret) nextCaret += delta;
    else if (edit.start < caret) nextCaret = out.length - edit.text.length;
  }
  out += text.slice(cursor);
  return { text: out, caret: clamp(out, nextCaret) };
}

/**
 * 删除第 `number` 张图（共 `count` 张）后文本要做的编辑：该编号的占位符全部移除（占位符前是行首 / 空白时顺带吞掉后面的
 * 一个空格），更大的编号（仅限 `count` 以内，其余是用户自己写的文字）依次减一，保持与剩余图片顺序一致。
 * 用户已手动删掉该占位符时只做重排。
 */
export function removePlaceholder(text: string, caret: number, number: number, count: number): TextEdit {
  const edits: Replacement[] = [];
  for (const span of placeholderSpans(text)) {
    if (span.number === number) {
      const end = endsAtBoundary(text.slice(0, span.start)) && text[span.end] === " " ? span.end + 1 : span.end;
      edits.push({ start: span.start, end, text: "" });
    } else if (span.number > number && span.number <= count) {
      edits.push({ start: span.start, end: span.end, text: imagePlaceholder(span.number - 1) });
    }
  }
  return applyReplacements(text, edits, clamp(text, caret));
}

/** 编号 1..=`count` 里在文本中已找不到占位符的图片编号（升序）：用户删掉了占位符，对应图片随之移除。 */
export function orphanedImages(text: string, count: number): number[] {
  const present = new Set(placeholderSpans(text).map((span) => span.number));
  const orphaned: number[] = [];
  for (let number = 1; number <= count; number += 1) if (!present.has(number)) orphaned.push(number);
  return orphaned;
}

/**
 * 一次纯删除（退格、选区删除等）只删掉了某个占位符的一部分时，删除后文本里该占位符的残余区间 `[start, end)`；
 * 删掉它让占位符整体消失，与删掉一个字符的手感一致。只认编号在 `count` 以内的占位符。不是纯删除、或没有碰到
 * 占位符的局部时返回 null。
 */
export function placeholderRemnant(previous: string, next: string, count: number): { start: number; end: number } | null {
  if (count === 0 || next.length >= previous.length) return null;
  let prefix = 0;
  while (prefix < next.length && previous.charCodeAt(prefix) === next.charCodeAt(prefix)) prefix += 1;
  const start = floorBoundary(previous, prefix);
  let suffix = 0;
  while (
    suffix < next.length - start &&
    previous.charCodeAt(previous.length - 1 - suffix) === next.charCodeAt(next.length - 1 - suffix)
  ) {
    suffix += 1;
  }
  const previousEnd = previous.length - suffix;
  // 新文本在 `start` 之后若还有新增内容就不是纯删除
  if (next.length - suffix !== start || floorBoundary(previous, previousEnd) !== previousEnd) return null;
  let lo = start;
  let hi = previousEnd;
  for (const span of placeholderSpans(previous)) {
    const overlaps = span.start < previousEnd && start < span.end;
    const covered = start <= span.start && span.end <= previousEnd;
    if (span.number <= count && overlaps && !covered) {
      lo = Math.min(lo, span.start);
      hi = Math.max(hi, span.end);
    }
  }
  if (lo === start && hi === previousEnd) return null;
  let end = start + (hi - previousEnd);
  if (endsAtBoundary(next.slice(0, lo)) && next[end] === " ") end += 1;
  return { start: lo, end };
}

/** 输入框文本变化后的收敛结果：`removed` 是随之移除的图片编号（降序，调用方按此顺序从附件列表里删）。 */
export interface DraftChange extends TextEdit {
  removed: number[];
}

/**
 * 输入框文本变了：删到一半的占位符整体删掉；占位符已不在文本里的图片随之移除（手动删除、剪切、撤销插入等），
 * 其后占位符与图片编号依次减一。`caret` 为 `next` 里的光标位置，`count` 为变化前的图片张数。
 */
export function reconcileDraft(previous: string, next: string, caret: number, count: number): DraftChange {
  let text = next;
  let at = clamp(next, caret);
  const remnant = placeholderRemnant(previous, next, count);
  if (remnant !== null) {
    text = next.slice(0, remnant.start) + next.slice(remnant.end);
    if (at >= remnant.end) at -= remnant.end - remnant.start;
    else if (at > remnant.start) at = remnant.start;
  }
  const removed: number[] = [];
  let remaining = count;
  // 从大到小移除，前面的编号才不受重排影响
  for (const number of orphanedImages(text, count).reverse()) {
    const edit = removePlaceholder(text, at, number, remaining);
    text = edit.text;
    at = edit.caret;
    removed.push(number);
    remaining -= 1;
  }
  return { text, caret: clamp(text, at), removed };
}

export type DraftPart = { kind: "text"; text: string } | { kind: "image"; text: string; number: number };

/** 历史里的提问文本拆成文字与图片引用（只认 1..=`count` 的占位符，其余照原样当文字）。 */
export function splitPlaceholders(text: string, count: number): DraftPart[] {
  const parts: DraftPart[] = [];
  let cursor = 0;
  for (const span of placeholderSpans(text)) {
    if (span.number > count) continue;
    if (span.start > cursor) parts.push({ kind: "text", text: text.slice(cursor, span.start) });
    parts.push({ kind: "image", text: text.slice(span.start, span.end), number: span.number });
    cursor = span.end;
  }
  if (cursor < text.length) parts.push({ kind: "text", text: text.slice(cursor) });
  return parts;
}
