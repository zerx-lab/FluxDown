/**
 * AI 回答的 Markdown 解析（纯函数，渲染见 `Markdown.tsx`）：只产出结构化节点，不产出 HTML，
 * 渲染端用 React 元素输出，模型回答里的任何标签都只会作为文本出现。
 *
 * 支持：围栏代码块（流式输出时未闭合的围栏视为到结尾）、ATX 标题、分隔线、引用、有序 / 无序列表（含缩进嵌套）、
 * GFM 表格、段落（段内换行保留）；行内为代码、粗体、斜体、删除线、链接与裸 URL。
 */

export type Block =
  | { kind: "code"; lang: string; text: string }
  | { kind: "heading"; level: number; text: string }
  | { kind: "hr" }
  | { kind: "quote"; blocks: Block[] }
  | { kind: "list"; ordered: boolean; start: number; items: Block[][] }
  | { kind: "table"; head: string[]; rows: string[][] }
  | { kind: "para"; lines: string[] };

export type Inline =
  | { kind: "text"; text: string }
  | { kind: "code"; text: string }
  | { kind: "strong" | "em" | "del"; children: Inline[] }
  | { kind: "link"; href: string; children: Inline[] };

const FENCE_RE = /^ {0,3}(`{3,}|~{3,})\s*([^`\s]*)/;
const HEADING_RE = /^ {0,3}(#{1,6})\s+(.*?)(?:\s+#+)?\s*$/;
const HR_RE = /^ {0,3}([-*_])(?:\s*\1){2,}\s*$/;
const QUOTE_RE = /^ {0,3}> ?(.*)$/;
const ITEM_RE = /^( *)([-*+]|\d{1,9}[.)])\s+(.*)$/;
const TABLE_SEP_RE = /^\s*\|?\s*:?-{3,}:?\s*(?:\|\s*:?-{3,}:?\s*)*\|?\s*$/;

const indentOf = (line: string) => line.length - line.trimStart().length;
const isBlank = (line: string) => line.trim() === "";

function splitRow(line: string): string[] {
  let row = line.trim();
  if (row.startsWith("|")) row = row.slice(1);
  if (row.endsWith("|") && !row.endsWith("\\|")) row = row.slice(0, -1);
  return row.split(/(?<!\\)\|/).map((cell) => cell.trim().replace(/\\\|/g, "|"));
}

/** 以 `lines[i]` 开头的行能否开始一个非段落块（用于结束段落）。 */
function startsBlock(lines: string[], i: number): boolean {
  const line = lines[i] ?? "";
  return (
    FENCE_RE.test(line) ||
    HEADING_RE.test(line) ||
    HR_RE.test(line) ||
    QUOTE_RE.test(line) ||
    ITEM_RE.test(line) ||
    (line.includes("|") && TABLE_SEP_RE.test(lines[i + 1] ?? ""))
  );
}

export function parseBlocks(source: string): Block[] {
  return parseLines(source.replace(/\r\n?/g, "\n").split("\n"));
}

function parseLines(lines: string[]): Block[] {
  const blocks: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i] ?? "";
    if (isBlank(line)) {
      i += 1;
      continue;
    }

    const fence = FENCE_RE.exec(line);
    if (fence) {
      const marker = fence[1] ?? "```";
      const body: string[] = [];
      i += 1;
      while (i < lines.length) {
        const current = lines[i] ?? "";
        const trimmed = current.trim();
        if (trimmed.startsWith(marker) && trimmed.replace(/[`~]/g, "") === "") break;
        body.push(current);
        i += 1;
      }
      i += 1;
      blocks.push({ kind: "code", lang: fence[2] ?? "", text: body.join("\n") });
      continue;
    }

    const heading = HEADING_RE.exec(line);
    if (heading) {
      blocks.push({ kind: "heading", level: heading[1]?.length ?? 1, text: heading[2] ?? "" });
      i += 1;
      continue;
    }

    if (HR_RE.test(line)) {
      blocks.push({ kind: "hr" });
      i += 1;
      continue;
    }

    if (QUOTE_RE.test(line)) {
      const body: string[] = [];
      for (let m = QUOTE_RE.exec(lines[i] ?? ""); i < lines.length && m; m = QUOTE_RE.exec(lines[i] ?? "")) {
        body.push(m[1] ?? "");
        i += 1;
      }
      blocks.push({ kind: "quote", blocks: parseLines(body) });
      continue;
    }

    const item = ITEM_RE.exec(line);
    if (item) {
      const [list, next] = parseList(lines, i);
      blocks.push(list);
      i = next;
      continue;
    }

    if (line.includes("|") && TABLE_SEP_RE.test(lines[i + 1] ?? "")) {
      const head = splitRow(line);
      const rows: string[][] = [];
      i += 2;
      while (i < lines.length && !isBlank(lines[i] ?? "") && (lines[i] ?? "").includes("|")) {
        rows.push(splitRow(lines[i] ?? ""));
        i += 1;
      }
      blocks.push({ kind: "table", head, rows });
      continue;
    }

    const para: string[] = [line.trim()];
    i += 1;
    while (i < lines.length && !isBlank(lines[i] ?? "") && !startsBlock(lines, i)) {
      para.push((lines[i] ?? "").trim());
      i += 1;
    }
    blocks.push({ kind: "para", lines: para });
  }
  return blocks;
}

/** 解析从 `start` 行开始的列表；返回列表块与列表之后的行号。 */
function parseList(lines: string[], start: number): [Block, number] {
  const first = ITEM_RE.exec(lines[start] ?? "");
  const baseIndent = first?.[1]?.length ?? 0;
  const ordered = /\d/.test(first?.[2] ?? "");
  const items: Block[][] = [];
  let i = start;
  while (i < lines.length) {
    const m = ITEM_RE.exec(lines[i] ?? "");
    if (!m || (m[1]?.length ?? 0) !== baseIndent || /\d/.test(m[2] ?? "") !== ordered) break;
    const contentIndent = baseIndent + (m[2]?.length ?? 1) + 1;
    const body: string[] = [m[3] ?? ""];
    i += 1;
    while (i < lines.length) {
      const current = lines[i] ?? "";
      if (isBlank(current)) {
        const nextLine = lines[i + 1] ?? "";
        if (isBlank(nextLine) || indentOf(nextLine) <= baseIndent) break;
        body.push("");
        i += 1;
        continue;
      }
      if (indentOf(current) <= baseIndent && (ITEM_RE.test(current) || startsBlock(lines, i))) break;
      body.push(current.slice(Math.min(indentOf(current), contentIndent)));
      i += 1;
    }
    items.push(parseLines(body));
    if (isBlank(lines[i] ?? "") && ITEM_RE.exec(lines[i + 1] ?? "")?.[1]?.length === baseIndent) i += 1;
  }
  const startNumber = ordered ? Number.parseInt(first?.[2] ?? "1", 10) : 1;
  return [{ kind: "list", ordered, start: Number.isFinite(startNumber) ? startNumber : 1, items }, i];
}

const INLINE_RE =
  /(`+)([^`]|[^`][\s\S]*?[^`])\1(?!`)|\*\*(?=\S)([\s\S]*?\S)\*\*|__(?=\S)([\s\S]*?\S)__|~~(?=\S)([\s\S]*?\S)~~|\*(?=[^\s*])([^*]*?[^\s*])\*|(?<![\p{L}\p{N}_])_(?=[^\s_])([^_]*?[^\s_])_(?![\p{L}\p{N}_])|\[([^\]\n]+)\]\(\s*<?([^)\s>]+)>?(?:\s+"[^"]*")?\s*\)|(https?:\/\/[^\s<>]*[^\s<>.,;:!?'")\]}*_~])/gu;

/** 链接只放行 http(s) / mailto 与站内路径（`/` 后不能再跟 `/` 或 `\`：浏览器会把 `/\` 当 `//`，变成外站链接）；其余只显示文字。 */
export function safeHref(raw: string): string | null {
  if (/^\/(?![\/\\])/.test(raw)) return raw;
  if (raw.startsWith("#")) return raw;
  try {
    const url = new URL(raw);
    return url.protocol === "http:" || url.protocol === "https:" || url.protocol === "mailto:" ? url.href : null;
  } catch {
    return null;
  }
}

export function parseInline(source: string): Inline[] {
  const out: Inline[] = [];
  let last = 0;
  const pushText = (text: string) => {
    if (text === "") return;
    const prev = out[out.length - 1];
    if (prev?.kind === "text") prev.text += text;
    else out.push({ kind: "text", text });
  };
  for (const m of source.matchAll(INLINE_RE)) {
    const index = m.index ?? 0;
    pushText(source.slice(last, index));
    last = index + m[0].length;
    if (m[2] !== undefined) {
      const code = m[2];
      out.push({ kind: "code", text: /^ .* $/.test(code) ? code.slice(1, -1) : code });
    } else if (m[3] !== undefined || m[4] !== undefined) {
      out.push({ kind: "strong", children: parseInline(m[3] ?? m[4] ?? "") });
    } else if (m[5] !== undefined) {
      out.push({ kind: "del", children: parseInline(m[5]) });
    } else if (m[6] !== undefined || m[7] !== undefined) {
      out.push({ kind: "em", children: parseInline(m[6] ?? m[7] ?? "") });
    } else if (m[8] !== undefined && m[9] !== undefined) {
      const href = safeHref(m[9]);
      const children = parseInline(m[8]);
      if (href) out.push({ kind: "link", href, children });
      else out.push(...children);
    } else if (m[10] !== undefined) {
      const href = safeHref(m[10]);
      if (href) out.push({ kind: "link", href, children: [{ kind: "text", text: m[10] }] });
      else pushText(m[10]);
    } else {
      pushText(m[0]);
    }
  }
  pushText(source.slice(last));
  return out;
}
