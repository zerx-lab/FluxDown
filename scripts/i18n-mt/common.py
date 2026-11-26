"""qwen-mt 翻译自动化的共享部分：哈希、占位符、术语表配置、来源锁、并发执行与校验。"""

from __future__ import annotations

import fnmatch
import hashlib
import json
import re
import unicodedata
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Dict, List, Optional, Tuple

ROOT = Path(__file__).resolve().parents[2]
GLOSSARY_DIR = ROOT / "i18n" / "glossary"
CONFIG_HEADING = "## qwen-mt 机器翻译配置"

PLACEHOLDER = re.compile(r"\{([A-Za-z0-9_.]+)\}")
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
    """繁中译文里 Big5（含 HKSCS）编码不了的汉字：多半是简体字形或生僻异体。"""
    bad = []
    for ch in text:
        if not is_cjk(ch):
            continue
        try:
            ch.encode("big5hkscs")
        except UnicodeEncodeError:
            bad.append(ch)
    return bad


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
    _matchers: List[Tuple[re.Pattern, Dict[str, str]]] = field(default_factory=list, repr=False)

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
        """只传入文本里真正出现的术语：术语按 token 计费，也会干扰无关文本。"""
        return [term for pattern, term in self._matchers if pattern.search(text)]

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
        )
    except (KeyError, AttributeError, TypeError) as exc:
        raise ConfigError(f"{path.relative_to(ROOT)} 配置缺字段或类型不对：{exc!r}") from exc
    if config.source not in ("en", "zh"):
        raise ConfigError(f"{path.relative_to(ROOT)} 的 source 只能是 en 或 zh")
    return config


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

    results: List[Optional[str]] = []
    failures: Dict[str, str] = {}
    if not units:
        return results, failures
    with ThreadPoolExecutor(max_workers=max(1, workers)) as pool:
        for unit, (value, reason) in zip(units, pool.map(one, units)):
            results.append(value)
            if value is None:
                failures[unit.label] = reason or "unknown"
    return results, failures


def check_translation(source: str, translated: str, traditional_chinese: bool) -> Optional[str]:
    """译文通用校验：非空、占位符集合不变、不增删换行、繁中不出 Big5 外字形。"""
    if not translated.strip():
        return "译文为空"
    if placeholder_counts(source) != placeholder_counts(translated):
        return f"占位符不一致：{sorted(placeholder_counts(source))} → {sorted(placeholder_counts(translated))}"
    if source.count("\n") != translated.count("\n"):
        return "换行数不一致"
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
) -> CatalogPlan:
    """对一种语言算出：待译（缺失 / en 原文已变）、采纳（已有译文无锁记录，视为人工）、原样复制、孤儿。"""
    plan = CatalogPlan(pending=[], adopted=[], copies={}, orphans=[k for k in target if k not in source])
    for key, value in source.items():
        digest = short_hash(value)
        if key in protected:
            if key in target and lock.get(key) != digest:
                plan.adopted.append(key)
            continue
        if copy_key(key) or not has_letters(value):
            if target.get(key) != value or lock.get(key) != digest:
                plan.copies[key] = value
            continue
        if key not in target:
            plan.pending.append(key)
        elif key not in lock:
            plan.adopted.append(key)
        elif lock[key] != digest:
            plan.pending.append(key)
    if limit is not None and len(plan.pending) > limit:
        plan.deferred = len(plan.pending) - limit
        plan.pending = plan.pending[:limit]
    return plan


def numbered_mask(text: str) -> Tuple[str, Callable[[str], str]]:
    """把 `{name}` 换成 `{1}`、`{2}`：模型偶尔会翻译占位符里的英文名，数字名不会。"""
    names: List[str] = []

    def sub(match: re.Match) -> str:
        if match.group(1) not in names:
            names.append(match.group(1))
        return "{" + str(names.index(match.group(1)) + 1) + "}"

    masked = PLACEHOLDER.sub(sub, text)

    def restore(out: str) -> str:
        def back(match: re.Match) -> str:
            token = match.group(1)
            if token.isdigit() and 1 <= int(token) <= len(names):
                return "{" + names[int(token) - 1] + "}"
            return match.group(0)

        return PLACEHOLDER.sub(back, out)

    return masked, restore


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
