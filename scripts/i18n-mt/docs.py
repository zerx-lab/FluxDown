"""官网文档：en 原文变化后，用 qwen-mt 起草 / 增量更新 `website-v2/src/content/docs/<lang>/**`。

- 目标语言 = 站点 `LANGS` 里 en 以外的全部（zh 也由 en 起草，再人工润色；其余语言纯机翻）。
- 过期判定沿用译文 frontmatter 的 `sourceHash`（与 `website-v2/scripts/docs-hash.mjs` 同一规格）。
- 增量：在 git 历史里找到 hash 等于 `sourceHash` 的旧 en 版本，按空行分块与新 en 做块级 diff；
  未变的块直接沿用现有译文（人工润色保留），只翻新增 / 改动的块。
- 代码块、行内代码、URL、HTML 原样保留；链接文字与图片 alt 单独翻译；`/docs/en/` 链接改指
  `/docs/<lang>/`，锚点按两边标题顺序映射；存在 `.<lang>.` 版本的截图自动换图。
"""

from __future__ import annotations

import difflib
import json
import re
import subprocess
import unicodedata
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Dict, List, Optional, Tuple

from common import (
    PLACEHOLDER,
    ROOT,
    Attempt,
    LocaleConfig,
    MemoryPool,
    Report,
    Translate,
    Unit,
    check_translation,
    has_letters,
    run_units,
    short_hash,
    site_locale_config,
    website_langs,
)

DOCS = ROOT / "website-v2" / "src" / "content" / "docs"
PUBLIC = ROOT / "website-v2" / "public"
FRONTMATTER = re.compile(r"^---\r?\n([\s\S]*?)\r?\n---\r?\n")
FIELD = re.compile(r"^([A-Za-z_][\w-]*):\s*(.*)$")
TRANSLATED_FIELDS = ("title", "description")

HEADING = re.compile(r"^(#{1,6}\s+)(.*?)(\s+#+)?\s*$")
LIST_ITEM = re.compile(r"^((?:[-*+]|\d+[.)])\s+(?:\[[ xX]\]\s+)?)(.*)$")
QUOTE = re.compile(r"^(>+\s?)(.*)$")
CALLOUT = re.compile(r"^\[![A-Za-z]+\]\s*$")
RULE = re.compile(r"^([-*_])(\s*\1){2,}\s*$")
TABLE_SEPARATOR = re.compile(r"^\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$")
INLINE = re.compile(
    r"(?P<code>(?P<ticks>`+).+?(?P=ticks))"
    r"|(?P<img>!\[(?P<alt>[^\]]*)\]\((?P<src>[^)\s]+)(?P<ititle>\s+\"[^\"]*\")?\))"
    r"|(?P<link>\[(?P<text>[^\]]+)\]\((?P<href>[^)\s]+)(?P<title>\s+\"[^\"]*\")?\))"
    r"|(?P<auto><https?://[^>\s]+>)"
    r"|(?P<url>https?://[^\s)<>\]]+)"
    r"|(?P<html></?[A-Za-z][^>\n]*>)"
)
TOKEN = re.compile(r"\{x(\d+)\}")
LOCALIZED_DOCS = re.compile(r"/docs/[a-z]{2}(?:-[a-z]{2})?/")
LOCALIZED_ASSET = re.compile(r"\.[a-z]{2}(?:-[a-z]{2})?\.(webp|png|jpe?g|gif|svg|avif)\b")


def anchor_link(lang: str) -> re.Pattern:
    return re.compile(r"\]\((/docs/" + re.escape(lang) + r"/[^)#\s]*)?#([^)\s]+)\)")


Renderer = Callable[[List[Optional[str]]], str]


def split_frontmatter(raw: str) -> Tuple[List[str], str]:
    match = FRONTMATTER.match(raw)
    if not match:
        return [], raw
    return match.group(1).replace("\r\n", "\n").split("\n"), raw[match.end() :]


def source_hash(raw: str) -> str:
    """与 docs-hash.mjs 一致：去 frontmatter 的正文，统一 \\n 并 trim 后取 sha256 前 12 位。"""
    return short_hash(split_frontmatter(raw)[1].replace("\r\n", "\n").strip())


def field_values(lines: List[str]) -> Dict[str, str]:
    values = {}
    for line in lines:
        match = FIELD.match(line)
        if match:
            values[match.group(1)] = _unquote(match.group(2).strip())
    return values


def _unquote(value: str) -> str:
    if len(value) >= 2 and value[0] == value[-1] == '"':
        try:
            return str(json.loads(value))
        except json.JSONDecodeError:
            return value[1:-1]
    if len(value) >= 2 and value[0] == value[-1] == "'":
        return value[1:-1].replace("''", "'")
    return value


def yaml_scalar(value: str) -> str:
    """能写成 YAML 纯量就不加引号（与现有 zh 文档一致），否则写双引号 JSON 字符串。"""
    plain_ok = (
        value
        and value == value.strip()
        and value[0] not in "!&*[]{}|>'\"%@`#,?:-"
        and ": " not in value
        and " #" not in value
        and not value.endswith(":")
    )
    return value if plain_ok else json.dumps(value, ensure_ascii=False)


def split_blocks(body: str) -> List[str]:
    """按空行分块，围栏代码块整体算一块（块内空行不拆）。"""
    blocks: List[str] = []
    current: List[str] = []
    fence: Optional[str] = None
    for line in body.replace("\r\n", "\n").strip().split("\n"):
        stripped = line.lstrip()
        if fence:
            current.append(line)
            if stripped.startswith(fence):
                fence = None
            continue
        if stripped.startswith("```") or stripped.startswith("~~~"):
            fence = stripped[:3]
            current.append(line)
            continue
        if not line.strip():
            if current:
                blocks.append("\n".join(current))
                current = []
            continue
        current.append(line)
    if current:
        blocks.append("\n".join(current))
    return blocks


def _plain_heading(text: str) -> str:
    text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", text)
    return text.replace("`", "").replace("**", "").replace("~~", "").replace("*", "")


def heading_slugs(body: str) -> List[str]:
    """标题锚点，按 github-slugger 规则：小写、去标点符号、空格变 `-`、重名加 `-1`。"""
    slugs: List[str] = []
    seen: Dict[str, int] = {}
    fence: Optional[str] = None
    for line in body.replace("\r\n", "\n").split("\n"):
        stripped = line.lstrip()
        if fence:
            if stripped.startswith(fence):
                fence = None
            continue
        if stripped.startswith("```") or stripped.startswith("~~~"):
            fence = stripped[:3]
            continue
        match = HEADING.match(line)
        if not match:
            continue
        chars = []
        for ch in _plain_heading(match.group(2)).lower():
            if ch == " ":
                chars.append("-")
            elif ch in "-_" or unicodedata.category(ch)[0] in "LNM":
                chars.append(ch)
        slug = "".join(chars)
        if slug in seen:
            seen[slug] += 1
            slug = f"{slug}-{seen[slug]}"
        else:
            seen[slug] = 0
        slugs.append(slug)
    return slugs


def _split_row(line: str) -> List[str]:
    inner = line.strip()
    if inner.startswith("|"):
        inner = inner[1:]
    if inner.endswith("|") and not inner.endswith("\\|"):
        inner = inner[:-1]
    cells, current, in_code, index = [], "", False, 0
    while index < len(inner):
        ch = inner[index]
        if ch == "\\" and index + 1 < len(inner):
            current += inner[index : index + 2]
            index += 2
            continue
        if ch == "`":
            in_code = not in_code
        if ch == "|" and not in_code:
            cells.append(current)
            current = ""
        else:
            current += ch
        index += 1
    cells.append(current)
    return cells


def _const(text: str) -> Renderer:
    return lambda results: text


def check_doc_segment(source: str, translated: str, traditional_chinese: bool = False) -> Optional[str]:
    problem = check_translation(source, translated, traditional_chinese)
    if problem:
        return problem
    if translated.count("**") % 2 != source.count("**") % 2:
        return "粗体标记 ** 不成对"
    return None


class DocDraft:
    """收集一篇文档里所有待译片段，统一并发翻译后再拼装。"""

    def __init__(self, config: LocaleConfig, pool: MemoryPool, label: str, lang: str = "zh") -> None:
        self.config = config
        self.pool = pool
        self.label = label
        self.lang = lang
        self.units: List[Unit] = []

    def segment(self, text: str) -> Renderer:
        """一段行内文本：代码 / URL / HTML 换成 `{xN}` 占位，链接文字与图片 alt 递归成独立片段。"""
        tokens: List[Renderer] = []

        def mask(match: re.Match) -> str:
            if match.group("img") is not None:
                alt = self.segment(match.group("alt"))
                tail = rewrite_image(match.group("src"), self.lang) + (match.group("ititle") or "")
                tokens.append(lambda results, alt=alt, tail=tail: f"![{alt(results)}]({tail})")
            elif match.group("link") is not None:
                inner = self.segment(match.group("text"))
                tail = rewrite_link(match.group("href"), self.lang) + (match.group("title") or "")
                tokens.append(lambda results, inner=inner, tail=tail: f"[{inner(results)}]({tail})")
            else:
                tokens.append(_const(match.group(0)))
            return "{x%d}" % (len(tokens) - 1)

        lead = text[: len(text) - len(text.lstrip())]
        trail = text[len(text.rstrip()) :]
        masked = INLINE.sub(mask, text.strip())

        def fill(out: str, results: List[Optional[str]]) -> str:
            return lead + TOKEN.sub(lambda m: tokens[int(m.group(1))](results), out) + trail

        if not has_letters(TOKEN.sub("", masked)):
            return lambda results: fill(masked, results)
        index = len(self.units)
        terms = self.config.terms_for(PLACEHOLDER.sub(" ", masked))
        self.units.append(
            Unit(
                label=f"{self.label}: {text.strip()[:60]}",
                source_lang=self.config.source_lang,
                target_lang=self.config.target_lang,
                domains=self.config.domains,
                attempts=[Attempt(masked, terms, self.pool.select(masked)), Attempt(masked, terms, [])],
                check=lambda out: check_doc_segment(masked, out, self.config.traditional_chinese),
            )
        )
        return lambda results: fill(results[index] if results[index] is not None else masked, results)

    def block(self, block: str) -> Renderer:
        lines = block.split("\n")
        if all(_is_paragraph_line(line) for line in lines):
            return self.segment(" ".join(line.strip() for line in lines))
        renderers: List[Renderer] = []
        fence: Optional[str] = None
        for line in lines:
            stripped = line.lstrip()
            indent = line[: len(line) - len(stripped)]
            if fence:
                renderers.append(_const(line))
                if stripped.startswith(fence):
                    fence = None
                continue
            if stripped.startswith("```") or stripped.startswith("~~~"):
                fence = stripped[:3]
                renderers.append(_const(line))
                continue
            renderers.append(self._line(indent, stripped))
        return lambda results: "\n".join(r(results) for r in renderers)

    def _line(self, indent: str, stripped: str) -> Renderer:
        if not stripped or RULE.match(stripped) or stripped.startswith("<") or stripped.startswith(":::"):
            return _const(indent + stripped)
        heading = HEADING.match(stripped)
        if heading:
            text = self.segment(heading.group(2))
            return lambda results: indent + heading.group(1) + text(results) + (heading.group(3) or "")
        if stripped.startswith("|"):
            if TABLE_SEPARATOR.match(stripped):
                return _const(indent + stripped)
            cells = [self.segment(cell.strip()) for cell in _split_row(stripped)]
            return lambda results: indent + "| " + " | ".join(c(results) for c in cells) + " |"
        quote = QUOTE.match(stripped)
        if quote:
            if CALLOUT.match(quote.group(2)):
                return _const(indent + stripped)
            inner = self._line("", quote.group(2))
            return lambda results: indent + quote.group(1) + inner(results)
        item = LIST_ITEM.match(stripped)
        if item:
            text = self.segment(item.group(2))
            return lambda results: indent + item.group(1) + text(results)
        text = self.segment(stripped)
        return lambda results: indent + text(results)


def _is_paragraph_line(line: str) -> bool:
    stripped = line.lstrip()
    return not (
        line != stripped
        or stripped.startswith(("#", "|", ">", "<", "```", "~~~", ":::"))
        or LIST_ITEM.match(stripped)
        or RULE.match(stripped)
    )


def rewrite_link(href: str, lang: str) -> str:
    if href.startswith("/docs/en/"):
        return f"/docs/{lang}/" + href[len("/docs/en/") :]
    return href


def rewrite_image(src: str, lang: str) -> str:
    """有该语言版本的截图（`x.<lang>.webp`）就换图，否则沿用 en 截图。"""
    if ".en." in src and src.startswith("/"):
        localized = src.replace(".en.", f".{lang}.")
        if (PUBLIC / localized.lstrip("/")).is_file():
            return localized
    return src


def git_revision_with_hash(rel: str, wanted: str) -> Optional[str]:
    """沿 en 文件历史找正文 hash 等于 wanted 的版本（CI 须 fetch-depth: 0）。"""
    log = subprocess.run(["git", "log", "--format=%H", "--", rel], cwd=ROOT, capture_output=True, text=True, check=False)
    if log.returncode != 0:
        return None
    for commit in log.stdout.split():
        shown = subprocess.run(["git", "show", f"{commit}:{rel}"], cwd=ROOT, capture_output=True, text=True, check=False)
        if shown.returncode == 0 and source_hash(shown.stdout) == wanted:
            return shown.stdout
    return None


@dataclass
class DocPlan:
    en_path: Path
    zh_path: Path
    en_raw: str
    old_en_raw: Optional[str]
    zh_raw: Optional[str]
    reuse: Dict[int, str]
    note: Optional[str]
    skip: bool = False


def plan_doc(en_path: Path, zh_path: Path) -> Optional[DocPlan]:
    """None = 已同步；skip = 无法增量对齐、保留人工译文；note = 整篇重译的原因。"""
    en_raw = en_path.read_text(encoding="utf-8")
    current = source_hash(en_raw)
    if not zh_path.is_file():
        return DocPlan(en_path, zh_path, en_raw, None, None, {}, None)
    zh_raw = zh_path.read_text(encoding="utf-8")
    recorded = field_values(split_frontmatter(zh_raw)[0]).get("sourceHash")
    if recorded == current:
        return None
    rel = str(en_path.relative_to(ROOT))
    old_raw = git_revision_with_hash(rel, recorded) if recorded else None
    if old_raw is None:
        return DocPlan(en_path, zh_path, en_raw, None, zh_raw, {}, None, skip=True)
    old_blocks = split_blocks(split_frontmatter(old_raw)[1])
    new_blocks = split_blocks(split_frontmatter(en_raw)[1])
    zh_blocks = split_blocks(split_frontmatter(zh_raw)[1])
    old_to_zh = align_blocks(old_blocks, zh_blocks)
    reuse: Dict[int, str] = {}
    matcher = difflib.SequenceMatcher(a=old_blocks, b=new_blocks, autojunk=False)
    for tag, i1, i2, j1, _ in matcher.get_opcodes():
        if tag != "equal":
            continue
        for offset in range(i2 - i1):
            if i1 + offset in old_to_zh:
                reuse[j1 + offset] = zh_blocks[old_to_zh[i1 + offset]]
    note = None
    if not reuse:
        note = "zh 与旧 en 分块对不上，整篇重译"
    return DocPlan(en_path, zh_path, en_raw, old_raw, zh_raw, reuse, note)


ALIGN_TOKEN = re.compile(r"[A-Za-z][A-Za-z0-9_.\-/]+|\d+")
STRONG_MATCH = 0.6


def block_features(block: str) -> Tuple[tuple, frozenset]:
    """块类型 + 跨语言保留下来的 ASCII 标识符 / 数字 / 链接路径（锚点、语言段、截图后缀归一）。"""
    first = block.lstrip()
    head = first.split("\n", 1)[0]
    heading = HEADING.match(head)
    if first.startswith(("```", "~~~")):
        kind: tuple = ("code", head.strip("`~ ").split(" ")[0])
    elif heading:
        kind = ("h", heading.group(1).strip())
    elif first.startswith("|"):
        kind = ("t", len(_split_row(head)))
    elif LIST_ITEM.match(head):
        kind = ("l", "o" if head[0].isdigit() else "u")
    elif first.startswith(">"):
        kind = ("q",)
    elif first.startswith("<"):
        kind = ("html",)
    else:
        kind = ("p",)
    normalized = LOCALIZED_ASSET.sub(r".en.\1", LOCALIZED_DOCS.sub("/docs/en/", re.sub(r"#[^)\s]*\)", ")", block)))
    return kind, frozenset(ALIGN_TOKEN.findall(normalized))


def align_blocks(source: List[str], zh: List[str]) -> Dict[int, int]:
    """旧 en 块 → zh 块下标。

    先做保序加权对齐找强锚点：同类块里 zh 保留的 ASCII 标识符 / 数字 / 链接大多也在 en 里
    （包含率 ≥ 0.6）。锚点之间的空档若两边块数相同，同类块按位置一一对应；块数不同说明
    人工译文在这里增删过块，无法确定对应关系，宁可重译也不把译文错配给相邻段落。
    """
    features_en = [block_features(b) for b in source]
    features_zh = [block_features(b) for b in zh]
    n, m = len(source), len(zh)

    def strong(i: int, j: int) -> float:
        (kind_en, tokens_en), (kind_zh, tokens_zh) = features_en[i], features_zh[j]
        common = tokens_en & tokens_zh
        # 至少两个共同记号：只含一个「FluxDown」之类的块会和大量段落「强匹配」。
        if kind_en != kind_zh or len(common) < 2:
            return 0.0
        score = len(common) / len(tokens_zh)
        return score if score >= STRONG_MATCH else 0.0

    best = [[0.0] * (m + 1) for _ in range(n + 1)]
    for i in range(1, n + 1):
        for j in range(1, m + 1):
            score = strong(i - 1, j - 1)
            diagonal = best[i - 1][j - 1] + score if score > 0 else -1.0
            best[i][j] = max(diagonal, best[i - 1][j], best[i][j - 1])
    anchors: List[Tuple[int, int]] = []
    i, j = n, m
    while i > 0 and j > 0:
        score = strong(i - 1, j - 1)
        if score > 0 and best[i][j] == best[i - 1][j - 1] + score:
            anchors.append((i - 1, j - 1))
            i, j = i - 1, j - 1
        elif best[i][j] == best[i - 1][j]:
            i -= 1
        else:
            j -= 1
    anchors.reverse()
    mapping = dict(anchors)
    bounded = [(-1, -1)] + anchors + [(n, m)]
    for (a1, b1), (a2, b2) in zip(bounded, bounded[1:]):
        if a2 - a1 != b2 - b1:
            continue
        for offset in range(1, a2 - a1):
            if features_en[a1 + offset][0] == features_zh[b1 + offset][0]:
                mapping[a1 + offset] = b1 + offset
    return mapping


def doc_lang(path: Path) -> str:
    """`docs/<lang>/…` 的语言段。"""
    return path.relative_to(DOCS).parts[0]


def render_doc(plan: DocPlan, config: LocaleConfig, translate: Translate, workers: int) -> Tuple[str, Dict[str, str], List[int]]:
    """返回（译文全文，失败片段，新译块下标）。"""
    rel = str(plan.zh_path.relative_to(DOCS))
    en_fm, en_body = split_frontmatter(plan.en_raw)
    new_blocks = split_blocks(en_body)
    pool = MemoryPool([(new_blocks[j], zh) for j, zh in plan.reuse.items() if "\n" not in zh])
    draft = DocDraft(config, pool, rel, doc_lang(plan.zh_path))

    zh_fields = field_values(split_frontmatter(plan.zh_raw)[0]) if plan.zh_raw else {}
    old_fields = field_values(split_frontmatter(plan.old_en_raw)[0]) if plan.old_en_raw else {}
    fm_renderers: List[Renderer] = []
    for line in en_fm:
        match = FIELD.match(line)
        if not match or match.group(1) == "sourceHash":
            if match is None:
                fm_renderers.append(_const(line))
            continue
        key, value = match.group(1), _unquote(match.group(2).strip())
        if key not in TRANSLATED_FIELDS:
            fm_renderers.append(_const(line))
        elif key in zh_fields and old_fields.get(key) == value:
            fm_renderers.append(_const(f"{key}: {yaml_scalar(zh_fields[key])}"))
        else:
            text = draft.segment(value)
            fm_renderers.append(lambda results, key=key, text=text: f"{key}: {yaml_scalar(text(results))}")

    block_renderers: List[Renderer] = []
    fresh: List[int] = []
    for index, block in enumerate(new_blocks):
        if index in plan.reuse:
            block_renderers.append(_const(plan.reuse[index]))
        else:
            block_renderers.append(draft.block(block))
            fresh.append(index)

    results, failures = run_units(translate, draft.units, workers)
    front = [r(results) for r in fm_renderers] + [f'sourceHash: "{source_hash(plan.en_raw)}"']
    blocks = [r(results) for r in block_renderers]
    return "---\n" + "\n".join(front) + "\n---\n\n" + "\n\n".join(blocks) + "\n", failures, fresh


def map_anchors(raw: str, fresh: List[int], own_en: str, outputs: Dict[Path, str], lang: str) -> str:
    """把新译块里 en 标题锚点换成目标语言文档同序标题的锚点；标题数对不上就保持原样。"""
    fm_lines, body = split_frontmatter(raw)
    blocks = split_blocks(body)
    own_zh = body

    def zh_and_en(target: Optional[str]) -> Tuple[Optional[str], Optional[str]]:
        if not target:
            return own_zh, split_frontmatter(own_en)[1]
        rel = target.strip("/")[len(f"docs/{lang}/") :].strip("/")
        zh_path = DOCS / lang / f"{rel}.md"
        en_path = DOCS / "en" / f"{rel}.md"
        if not en_path.is_file():
            return None, None
        zh_text = outputs.get(zh_path) or (zh_path.read_text(encoding="utf-8") if zh_path.is_file() else None)
        en_text = en_path.read_text(encoding="utf-8")
        return (split_frontmatter(zh_text)[1] if zh_text else None), split_frontmatter(en_text)[1]

    def sub(match: re.Match) -> str:
        zh_body, en_body = zh_and_en(match.group(1))
        if zh_body is None or en_body is None:
            return match.group(0)
        en_slugs, zh_slugs = heading_slugs(en_body), heading_slugs(zh_body)
        anchor = match.group(2)
        if len(en_slugs) != len(zh_slugs) or anchor not in en_slugs:
            return match.group(0)
        return f"]({match.group(1) or ''}#{zh_slugs[en_slugs.index(anchor)]})"

    for index in fresh:
        if index < len(blocks):
            blocks[index] = anchor_link(lang).sub(sub, blocks[index])
    return "---\n" + "\n".join(fm_lines) + "\n---\n\n" + "\n\n".join(blocks) + "\n"


def sync_docs(
    translate: Optional[Translate],
    report: Report,
    limit: Optional[int],
    workers: int,
    wanted: Optional[List[str]] = None,
) -> bool:
    """所有站点语言（en 以外，含手写的 zh）都从 en 文档起草 / 增量更新；limit = 每语言每轮篇数。"""
    changed = False
    for lang in website_langs():
        if lang == "en" or (wanted and lang not in wanted):
            continue
        changed |= _sync_docs_lang(lang, site_locale_config(lang), translate, report, limit, workers)
    return changed


def _sync_docs_lang(
    lang: str,
    config: LocaleConfig,
    translate: Optional[Translate],
    report: Report,
    limit: Optional[int],
    workers: int,
) -> bool:
    target_dir = DOCS / lang
    plans: List[DocPlan] = []
    for en_path in sorted((DOCS / "en").rglob("*.md")):
        plan = plan_doc(en_path, target_dir / en_path.relative_to(DOCS / "en"))
        if plan is not None:
            plans.append(plan)
    for path in sorted(target_dir.rglob("*.md")) if target_dir.is_dir() else []:
        if not (DOCS / "en" / path.relative_to(target_dir)).is_file():
            report.note(f"`{path.relative_to(ROOT)}` 没有对应 en 原文（未删除，需人工处理）")
    for plan in plans:
        if plan.skip:
            report.note(
                f"`{plan.zh_path.relative_to(ROOT)}` 的 sourceHash 在 en 历史里找不到，无法增量对齐；"
                "为免覆盖人工译文已跳过，请人工更新后运行 `npm run docs:hash`"
            )
    plans = [p for p in plans if not p.skip]
    deferred = 0
    if limit is not None and len(plans) > limit:
        deferred = len(plans) - limit
        plans = plans[:limit]
    if translate is None:
        drafts = sum(1 for plan in plans if plan.zh_raw is None)
        for plan in plans:
            if plan.zh_raw is None:
                continue
            total = len(split_blocks(split_frontmatter(plan.en_raw)[1]))
            note = f"沿用 {len(plan.reuse)}/{total} 块" + (f"；{plan.note}" if plan.note else "")
            report.note(f"（演练）`{plan.zh_path.relative_to(ROOT)}`：{note}")
        if drafts:
            report.note(f"（演练）`docs/{lang}` 新起草 {drafts} 篇")
        report.add("docs（演练，译出=待译篇数）", lang, {"translated": len(plans), "deferred": deferred}, {})
        return False
    outputs: Dict[Path, str] = {}
    fresh_blocks: Dict[Path, List[int]] = {}
    failures: Dict[str, str] = {}
    for plan in plans:
        text, failed, fresh = render_doc(plan, config, translate, workers)
        if failed:
            # 有片段没译成就整篇不落盘：否则 sourceHash 已刷新，英文残留不会再被重试。
            failures.update(failed)
            report.note(f"`{plan.zh_path.relative_to(ROOT)}` 有 {len(failed)} 个片段失败，本轮未写入")
            continue
        if plan.note:
            report.note(f"`{plan.zh_path.relative_to(ROOT)}`：{plan.note}")
        outputs[plan.zh_path] = text
        fresh_blocks[plan.zh_path] = fresh
    by_path = {plan.zh_path: plan for plan in plans}
    for path, text in outputs.items():
        final = map_anchors(text, fresh_blocks[path], by_path[path].en_raw, outputs, lang)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(final, encoding="utf-8")
    report.add("docs", lang, {"translated": len(outputs), "failed": len(failures), "deferred": deferred}, failures)
    return bool(outputs)
