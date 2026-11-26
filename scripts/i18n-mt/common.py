"""qwen-mt 翻译自动化的共享部分：哈希、占位符、术语表配置、来源锁、并发执行与校验。"""

from __future__ import annotations

import fnmatch
import hashlib
import json
import re
import sys
import time
import unicodedata
from collections import Counter
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import Callable, Dict, List, Optional, Tuple

ROOT = Path(__file__).resolve().parents[2]
GLOSSARY_DIR = ROOT / "i18n" / "glossary"
CONFIG_HEADING = "## qwen-mt 机器翻译配置"

PLACEHOLDER = re.compile(r"\{([A-Za-z0-9_.]+)\}")
HTML_TAG = re.compile(r"</?[A-Za-z][^<>]*>")
LATIN_WORD = re.compile(r"[A-Za-z][A-Za-z0-9'-]+")


class ConfigError(Exception):
    """仓库内配置（术语表、源文件、锁文件）不满足契约，整轮翻译必须中止。"""


class FatalApiError(Exception):
    """鉴权失败等无法逐条恢复的接口错误，整轮翻译必须中止。"""


class ItemError(Exception):
    """单条请求失败（内容审查拦截、参数被拒、重试耗尽），跳过该条继续。"""


def short_hash(text: str) -> str:
    """来源锁使用的哈希：UTF-8 sha256 十六进制前 12 位。"""
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:12]


def placeholder_counts(text: str) -> Counter:
    return Counter(PLACEHOLDER.findall(text))


def has_letters(text: str) -> bool:
    """去掉占位符后是否还有需要翻译的文字（纯数字、符号、占位符原样复制）。"""
    stripped = PLACEHOLDER.sub("", text)
    return any(unicodedata.category(ch).startswith("L") for ch in stripped)


def is_cjk(ch: str) -> bool:
    code = ord(ch)
    return (
        0x3400 <= code <= 0x4DBF
        or 0x4E00 <= code <= 0x9FFF
        or 0xF900 <= code <= 0xFAFF
        or 0x20000 <= code <= 0x2FA1F
    )


def non_big5_chars(text: str) -> List[str]:
    """繁中译文里 Big5（cp950）编码不了的汉字：多半是简体字形。现有 zh-TW 译文全部可编码。"""
    bad = []
    for ch in text:
        if not is_cjk(ch):
            continue
        try:
            ch.encode("cp950")
        except UnicodeEncodeError:
            bad.append(ch)
    return bad


def lower_first(text: str, locale: str) -> str:
    """首字母小写；土耳其语 İ → i、I → ı（str.lower 会把 İ 变成 i + 组合点）。"""
    if not text:
        return text
    head = text[0]
    if locale == "tr" and head in "İI":
        head = "i" if head == "İ" else "ı"
    else:
        head = head.lower()
    return head + text[1:]


WEBSITE_LANG_CONFIG = ROOT / "website-v2" / "src" / "i18n" / "config.ts"


def website_langs() -> List[str]:
    """读 `website-v2/src/i18n/config.ts` 的 `LANGS`（站点语言单一事实源）。"""
    text = WEBSITE_LANG_CONFIG.read_text(encoding="utf-8")
    match = re.search(r"export const LANGS = \[([^\]]*)\] as const", text)
    if not match:
        raise ConfigError(f"{WEBSITE_LANG_CONFIG.relative_to(ROOT)} 里找不到 `export const LANGS = [...] as const`")
    return re.findall(r'"([^"]+)"', match.group(1))


def glossary_locale(lang: str) -> str:
    """站点语言码（URL / 目录，小写）→ 术语表文件名：不区分大小写对应（zh-tw ↔ zh-TW、pt-br ↔ pt-BR）。"""
    for path in GLOSSARY_DIR.glob("*.md"):
        if path.stem.lower() == lang.lower():
            return path.stem
    return lang


@dataclass
class LocaleConfig:
    """`i18n/glossary/<locale>.md` 末尾「qwen-mt 机器翻译配置」JSON 块。"""

    locale: str
    target_lang: str
    source: str
    domains: str
    keep: List[str]
    terms: Dict[str, str]
    copy_keys: List[str]
    # 官网（界面文案 + 文档）用的领域提示；术语表没写时沿用 domains（那是软件界面口吻）。
    site_domains: str = ""
    _matchers: List[Tuple[re.Pattern, Dict[str, str]]] = field(default_factory=list, repr=False, init=False)

    def __post_init__(self) -> None:
        pairs = [(term, term) for term in self.keep] + list(self.terms.items())
        for src, tgt in pairs:
            if any(is_cjk(ch) for ch in src):
                pattern = re.compile(re.escape(src))
            else:
                pattern = re.compile(r"(?<![A-Za-z0-9])" + re.escape(src) + r"(?![A-Za-z0-9])", re.IGNORECASE)
            self._matchers.append((pattern, {"source": src, "target": tgt}))

    @property
    def source_lang(self) -> str:
        return "Chinese" if self.source == "zh" else "English"

    @property
    def traditional_chinese(self) -> bool:
        return self.target_lang == "Traditional Chinese"

    def terms_for(self, text: str) -> List[Dict[str, str]]:
        """只传入文本里真正出现的术语：术语按 token 计费，也会干扰无关文本。

        术语表按界面标签写成首字母大写（`Task` → `Tarea`），qwen-mt 会原样照搬；原文里该词
        小写出现时（句中），把术语两侧的首字母一起改成小写，避免句中冒出大写名词。"""
        found = []
        for pattern, term in self._matchers:
            match = pattern.search(text)
            if not match:
                continue
            occurrence = match.group(0)
            cased = term["source"][:1].isupper() and term["target"][:1].isupper()
            if term["source"] != term["target"] and cased and occurrence[:1].islower():
                found.append({"source": occurrence, "target": lower_first(term["target"], self.locale)})
            else:
                found.append(term)
        return found

    def is_copy_key(self, key: str) -> bool:
        return any(fnmatch.fnmatchcase(key, glob) for glob in self.copy_keys)


def load_locale_config(locale: str) -> LocaleConfig:
    path = GLOSSARY_DIR / f"{locale}.md"
    if not path.is_file():
        raise ConfigError(f"缺少术语表 {path.relative_to(ROOT)}（新增语言必须同时提供术语表）")
    text = path.read_text(encoding="utf-8")
    start = text.find(CONFIG_HEADING)
    if start < 0:
        raise ConfigError(f"{path.relative_to(ROOT)} 缺少「{CONFIG_HEADING}」一节")
    match = re.search(r"```json\n(.*?)\n```", text[start:], re.DOTALL)
    if not match:
        raise ConfigError(f"{path.relative_to(ROOT)} 的「{CONFIG_HEADING}」一节没有 json 代码块")
    try:
        raw = json.loads(match.group(1))
    except json.JSONDecodeError as exc:
        raise ConfigError(f"{path.relative_to(ROOT)} 配置不是合法 JSON：{exc}") from exc
    try:
        config = LocaleConfig(
            locale=locale,
            target_lang=str(raw["targetLang"]),
            source=str(raw["source"]),
            domains=str(raw["domains"]),
            keep=[str(v) for v in raw["keep"]],
            terms={str(k): str(v) for k, v in raw["terms"].items()},
            copy_keys=[str(v) for v in raw["copyKeys"]],
            site_domains=str(raw.get("siteDomains", "")),
        )
    except (KeyError, AttributeError, TypeError) as exc:
        raise ConfigError(f"{path.relative_to(ROOT)} 配置缺字段或类型不对：{exc!r}") from exc
    if config.source not in ("en", "zh"):
        raise ConfigError(f"{path.relative_to(ROOT)} 的 source 只能是 en 或 zh")
    return config


def site_locale_config(lang: str) -> LocaleConfig:
    """官网语言（`website_langs()` 里的码）的翻译配置：一律从 en 翻，领域提示用 siteDomains。

    术语表以 zh 为源的语言（zh-TW）：术语经 zh 术语表接成 en → zh → 目标语（zh 词条在目标语
    术语表里有精确对应的才保留），keep 取两表并集。"""
    base = load_locale_config(glossary_locale(lang))
    terms, keep = base.terms, base.keep
    if base.source == "zh":
        bridge = load_locale_config("zh")
        terms = {en: base.terms[zh] for en, zh in bridge.terms.items() if zh in base.terms}
        keep = list(dict.fromkeys(base.keep + bridge.keep))
    return replace(base, source="en", terms=terms, keep=keep, domains=base.site_domains or base.domains)


class SourceLock:
    """按语言、按键记录译文对应的 en 原文哈希（`i18n/source-lock.json` 同构）。"""

    def __init__(self, path: Path, source_rel: str) -> None:
        self.path = path
        if path.is_file():
            self.data = json.loads(path.read_text(encoding="utf-8"))
        else:
            self.data = {"version": 1, "source": source_rel, "hash": "sha256 前 12 位（UTF-8）", "locales": {}}
        if self.data.get("source") != source_rel:
            raise ConfigError(f"{path.relative_to(ROOT)} 的 source 应为 {source_rel}")

    def locale(self, locale: str) -> Dict[str, str]:
        return self.data["locales"].setdefault(locale, {})

    def save(self) -> None:
        self.path.write_text(json.dumps(self.data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def _tm_tokens(text: str) -> set:
    words = {w.lower() for w in LATIN_WORD.findall(text) if len(w) >= 3}
    cjk = [ch for ch in text if is_cjk(ch)]
    return words | {a + b for a, b in zip(cjk, cjk[1:])}


class MemoryPool:
    """翻译记忆：从已有的人工 / 已审译文里挑与待译文本最相近的几对，作为 `tm_list` 统一风格。"""

    def __init__(self, pairs: List[Tuple[str, str]], max_len: int = 240) -> None:
        self.entries = [
            (src, tgt, _tm_tokens(src)) for src, tgt in pairs if src and tgt and len(src) <= max_len
        ]

    def select(self, text: str, k: int = 3) -> List[Dict[str, str]]:
        tokens = _tm_tokens(text)
        if not tokens:
            return []
        scored = []
        for src, tgt, other in self.entries:
            if src == text or not other:
                continue
            overlap = len(tokens & other)
            if overlap:
                scored.append((overlap / len(tokens | other), src, tgt))
        scored.sort(key=lambda item: item[0], reverse=True)
        return [{"source": src, "target": tgt} for score, src, tgt in scored[:k] if score >= 0.2]


@dataclass
class Attempt:
    """一次请求：送出的文本、术语、记忆，以及把模型输出还原成最终译文的函数。"""

    text: str
    terms: List[Dict[str, str]]
    tm: List[Dict[str, str]]
    restore: Callable[[str], str] = lambda out: out


@dataclass
class Unit:
    label: str
    source_lang: str
    target_lang: str
    domains: str
    attempts: List[Attempt]
    check: Callable[[str], Optional[str]]


Translate = Callable[[str, str, str, List[Dict[str, str]], List[Dict[str, str]], str], str]


# 批量请求：qwen-mt 每次只收一段文本，按每分钟请求数（RPM）限流；把多条单行短文本用换行拼成一次请求，
# 按行拆回后逐条还原、校验，行数对不上或单条校验不过的退回逐条请求。默认关（1），由 main 的 --batch 打开。
_BATCH = {"items": 1, "chars": 1200}


def configure_batching(items: int, chars: int) -> None:
    _BATCH["items"] = max(1, items)
    _BATCH["chars"] = max(1, chars)


def _merge_unique(groups: List[List[Dict[str, str]]], limit: int) -> List[Dict[str, str]]:
    seen = set()
    merged: List[Dict[str, str]] = []
    for group in groups:
        for item in group:
            marker = (item["source"], item["target"])
            if marker not in seen:
                seen.add(marker)
                merged.append(item)
    return merged[:limit]


def _plan_batches(units: List[Unit]) -> List[List[int]]:
    """按出现顺序把同语种、同领域提示的单行文本攒成批；多行文本单独成批。"""
    batches: List[List[int]] = []
    open_batches: Dict[Tuple[str, str, str], Tuple[List[int], int]] = {}
    for index, unit in enumerate(units):
        text = unit.attempts[0].text if unit.attempts else ""
        if _BATCH["items"] <= 1 or "\n" in text or not text.strip():
            batches.append([index])
            continue
        key = (unit.source_lang, unit.target_lang, unit.domains)
        current, size = open_batches.get(key, ([], 0))
        if current and (len(current) >= _BATCH["items"] or size + len(text) > _BATCH["chars"]):
            current, size = [], 0
        if not current:
            batches.append(current)
        current.append(index)
        open_batches[key] = (current, size + len(text) + 1)
    return batches


def run_units(translate: Translate, units: List[Unit], workers: int) -> Tuple[List[Optional[str]], Dict[str, str]]:
    """并发翻译；每条按 attempts 顺序尝试，校验不过换下一种，全失败记入 failures 并返回 None。"""

    def one(unit: Unit) -> Tuple[Optional[str], Optional[str]]:
        reason = "no attempt"
        for attempt in unit.attempts:
            try:
                raw = translate(attempt.text, unit.source_lang, unit.target_lang, attempt.terms, attempt.tm, unit.domains)
            except ItemError as exc:
                reason = str(exc)
                continue
            result = attempt.restore(raw.strip())
            problem = unit.check(result)
            if problem is None:
                return result, None
            reason = problem
        return None, reason

    def batch(indices: List[int]) -> List[Tuple[int, Optional[str], Optional[str]]]:
        group = [units[index] for index in indices]
        if len(group) == 1:
            return [(indices[0], *one(group[0]))]
        firsts = [unit.attempts[0] for unit in group]
        lines: List[str] = []
        try:
            raw = translate(
                "\n".join(attempt.text for attempt in firsts),
                group[0].source_lang,
                group[0].target_lang,
                _merge_unique([attempt.terms for attempt in firsts], 60),
                _merge_unique([attempt.tm for attempt in firsts], 8),
                group[0].domains,
            )
            lines = raw.strip("\n").split("\n")
            if len(lines) != len(group):
                lines = [line for line in lines if line.strip()]
        except ItemError:
            lines = []
        out = []
        for position, (index, unit, attempt) in enumerate(zip(indices, group, firsts)):
            if len(lines) == len(group):
                result = attempt.restore(lines[position].strip())
                if unit.check(result) is None:
                    out.append((index, result, None))
                    continue
            out.append((index, *one(unit)))
        return out

    results: List[Optional[str]] = [None] * len(units)
    failures: Dict[str, str] = {}
    if not units:
        return results, failures
    total = len(units)
    title = f"{units[0].source_lang} → {units[0].target_lang}"
    step = max(1, min(50, total // 20))
    next_report = step
    batches = _plan_batches(units)
    started = time.monotonic()
    progress(f"[{title}] 开始：{total} 条，{len(batches)} 次请求，并发 {max(1, workers)}")
    done = 0
    with ThreadPoolExecutor(max_workers=max(1, workers)) as pool:
        futures = [pool.submit(batch, indices) for indices in batches]
        for future in as_completed(futures):
            for index, value, reason in future.result():
                done += 1
                results[index] = value
                if value is None:
                    failures[units[index].label] = reason or "unknown"
                    progress(f"[{title}] 失败 {units[index].label}：{reason or 'unknown'}")
            if done >= next_report or done == total:
                next_report = done + step
                elapsed = time.monotonic() - started
                eta = elapsed / done * (total - done)
                progress(f"[{title}] {done}/{total}（{done * 100 // total}%），失败 {len(failures)}，用时 {elapsed:.0f}s，预计剩余 {eta:.0f}s")
    return results, failures


def progress(message: str) -> None:
    """进度与逐条错误写 stderr 并立即刷新（Actions 日志 / 重定向到文件时实时可见）；汇总表仍走 stdout。"""
    print(f"{time.strftime('%H:%M:%S')} {message}", file=sys.stderr, flush=True)


def check_translation(source: str, translated: str, traditional_chinese: bool) -> Optional[str]:
    """译文通用校验：非空、占位符集合不变、不增删换行、繁中不出 Big5 外字形。"""
    if not translated.strip():
        return "译文为空"
    if placeholder_counts(source) != placeholder_counts(translated):
        return f"占位符不一致：{sorted(placeholder_counts(source))} → {sorted(placeholder_counts(translated))}"
    if source.count("\n") != translated.count("\n"):
        return "换行数不一致"
    if Counter(HTML_TAG.findall(source)) != Counter(HTML_TAG.findall(translated)):
        return "HTML 标签不一致"
    if traditional_chinese:
        bad = non_big5_chars(translated)
        if bad:
            return f"Big5 外字形：{''.join(sorted(set(bad)))}"
    return None


class Report:
    """汇总每个面 / 语言的结果，输出 Markdown（写入 $GITHUB_STEP_SUMMARY）。"""

    def __init__(self) -> None:
        self.rows: List[Tuple[str, str, Dict[str, int]]] = []
        self.failures: List[Tuple[str, str, str, str]] = []
        self.notes: List[str] = []

    def add(self, surface: str, locale: str, counts: Dict[str, int], failures: Dict[str, str]) -> None:
        self.rows.append((surface, locale, counts))
        for label, reason in failures.items():
            self.failures.append((surface, locale, label, reason))

    def note(self, text: str) -> None:
        self.notes.append(text)

    def markdown(self) -> str:
        lines = ["## qwen-mt 翻译同步", ""]
        if self.rows:
            lines += ["|面|语言|译出|复制|采纳既有|删除孤儿|失败|顺延|", "|---|---|---|---|---|---|---|---|"]
            for surface, locale, c in self.rows:
                lines.append(
                    f"|{surface}|{locale}|{c.get('translated', 0)}|{c.get('copied', 0)}|{c.get('adopted', 0)}"
                    f"|{c.get('removed', 0)}|{c.get('failed', 0)}|{c.get('deferred', 0)}|"
                )
        if self.failures:
            lines += ["", "### 失败（保持原状，下一轮重试）", ""]
            for surface, locale, label, reason in self.failures[:200]:
                lines.append(f"- `{surface}` `{locale}` `{label}`：{reason}")
            if len(self.failures) > 200:
                lines.append(f"- …另有 {len(self.failures) - 200} 条")
        if self.notes:
            lines += ["", "### 说明", ""] + [f"- {n}" for n in self.notes]
        return "\n".join(lines) + "\n"


@dataclass
class CatalogPlan:
    pending: List[str]
    adopted: List[str]
    copies: Dict[str, str]
    orphans: List[str]
    deferred: int = 0


def plan_catalog(
    source: Dict[str, str],
    target: Dict[str, str],
    lock: Dict[str, str],
    copy_key: Callable[[str], bool],
    protected: Tuple[str, ...],
    limit: Optional[int],
    untranslated: Tuple[Dict[str, str], ...] = (),
) -> CatalogPlan:
    """对一种语言算出：待译（缺失 / en 原文已变）、采纳（已有译文无锁记录，视为人工）、原样复制、孤儿。

    untranslated：值与其中某个源文件原样相同、又无锁记录的条目视为占位（如为过 tsc 先抄的 en 原文），
    按待译处理而不是采纳。"""
    plan = CatalogPlan(pending=[], adopted=[], copies={}, orphans=[k for k in target if k not in source])
    for key, value in source.items():
        digest = short_hash(value)
        if key in protected:
            if key in target and lock.get(key) != digest:
                plan.adopted.append(key)
            continue
        if copy_key(key):
            if target.get(key) != value or lock.get(key) != digest:
                plan.copies[key] = value
            continue
        if key not in target or (key in lock and lock[key] != digest):
            # 没有可译文字（纯占位符 / 符号）的条目原样复制，不调接口；已有的本地化标点由人工维护。
            if has_letters(value):
                plan.pending.append(key)
            else:
                plan.copies[key] = value
        elif key not in lock:
            if has_letters(value) and any(target[key] == other.get(key) for other in untranslated):
                plan.pending.append(key)
            else:
                plan.adopted.append(key)
    if limit is not None and len(plan.pending) > limit:
        plan.deferred = len(plan.pending) - limit
        plan.pending = plan.pending[:limit]
    return plan


def numbered_mask(text: str) -> Tuple[str, Callable[[str], str]]:
    """把 `{name}` 与 HTML 标签（含 `<version>` 这类尖括号字面量）换成 `{1}`、`{2}`：
    模型偶尔会翻译占位符里的英文名或标签名，数字名不会。"""
    originals: List[str] = []

    def sub(match: re.Match) -> str:
        token = match.group(0)
        if token not in originals:
            originals.append(token)
        return "{" + str(originals.index(token) + 1) + "}"

    masked = _MASKABLE.sub(sub, text)

    def restore(out: str) -> str:
        def back(match: re.Match) -> str:
            token = match.group(1)
            if token.isdigit() and 1 <= int(token) <= len(originals):
                return originals[int(token) - 1]
            return match.group(0)

        return PLACEHOLDER.sub(back, out)

    return masked, restore


_MASKABLE = re.compile(PLACEHOLDER.pattern + "|" + HTML_TAG.pattern)


def translate_catalog(
    translate: Translate,
    config: LocaleConfig,
    keys: List[str],
    source_text: Dict[str, Tuple[str, str]],
    pool: MemoryPool,
    workers: int,
) -> Tuple[Dict[str, str], Dict[str, str]]:
    """把 keys 对应的（源文本，源语言）译成目标语言，返回（成功译文，失败原因）。"""
    units = []
    for key in keys:
        text, source_lang = source_text[key]
        terms = config.terms_for(text) if source_lang == config.source_lang else []
        tm = pool.select(text)
        masked, restore = numbered_mask(text)
        attempts = [Attempt(text, terms, tm)]
        if masked != text:
            attempts.append(Attempt(masked, terms, [], restore))
        else:
            attempts.append(Attempt(text, terms, []))
        units.append(
            Unit(
                label=key,
                source_lang=source_lang,
                target_lang=config.target_lang,
                domains=config.domains,
                attempts=attempts,
                check=lambda out, text=text: check_translation(text, out, config.traditional_chinese),
            )
        )
    results, failures = run_units(translate, units, workers)
    done = {key: value for key, value in zip(keys, results) if value is not None}
    return done, failures
