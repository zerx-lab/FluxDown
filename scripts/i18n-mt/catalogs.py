"""键值文案面：`assets/i18n/*.json`（GPUI / Web / Android / iOS）、浏览器扩展 `fluxDown/utils/locales/*.ts`
与官网界面文案（`website-v2/src/i18n/messages/*.ts` 抽出的翻译单元）。"""

from __future__ import annotations

import json
import re
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Dict, List, Optional, Tuple

from common import (
    ROOT,
    CatalogPlan,
    ConfigError,
    LocaleConfig,
    MemoryPool,
    Report,
    SourceLock,
    Translate,
    load_locale_config,
    plan_catalog,
    short_hash,
    site_locale_config,
    translate_catalog,
    website_langs,
)

APP_DIR = ROOT / "assets" / "i18n"
APP_LOCK = ROOT / "i18n" / "source-lock.json"
EXT_DIR = ROOT / "fluxDown" / "utils" / "locales"
EXT_LOCK = ROOT / "i18n" / "extension-lock.json"
# 每种语言自称，新增语言时人工写入；机器不翻、不覆盖。
APP_PROTECTED = ("languageNativeName",)
WEB_DIR = ROOT / "website-v2"
WEB_STORE_DIR = WEB_DIR / "src" / "i18n" / "locales"
WEB_LOCK = ROOT / "i18n" / "website-lock.json"


def merge_order(existing: List[str], added: List[str], removed: set, source_order: List[str]) -> List[str]:
    """保留目标文件现有键序，新键插在源文件里离它最近的前一个已存在键之后。"""
    order = [key for key in existing if key not in removed]
    present = set(order)
    position = {key: index for index, key in enumerate(source_order)}
    for key in sorted(added, key=lambda k: position[k]):
        if key in present:
            continue
        anchor = -1
        for prev in reversed(source_order[: position[key]]):
            if prev in present:
                anchor = order.index(prev)
                break
        order.insert(anchor + 1, key)
        present.add(key)
    return order


@dataclass
class Surface:
    """一个文案面：怎么读写某语言、源与基线、来源锁、受保护键与原样复制规则。"""

    name: str
    lock: SourceLock
    source: Dict[str, str]
    zh_source: Dict[str, str]
    locales: List[str]
    read: Callable[[str], Dict[str, str]]
    write: Callable[[str, Dict[str, str], List[str]], None]
    protected: Tuple[str, ...]
    use_copy_keys: bool
    config_for: Callable[[str], LocaleConfig] = load_locale_config
    # 翻译（非演练）结束后调用：官网据 store 重新生成 TS 模块。
    after: Optional[Callable[[], None]] = None
    # 额外翻译记忆（参数为术语表语言名）：官网借用软件界面的已有译文统一用词。
    memory: Optional[Callable[[str], List[Tuple[str, str]]]] = None


def sync_surface(
    surface: Surface,
    translate: Optional[Translate],
    report: Report,
    wanted: Optional[List[str]],
    limit: Optional[int],
    workers: int,
) -> bool:
    """同步一个面的全部目标语言；translate 为 None 时只演练（不调接口、不写文件）。返回是否有改动。"""
    changed = False
    for locale in surface.locales:
        if wanted and locale.lower() not in wanted:
            continue
        config = surface.config_for(locale)
        target = surface.read(locale)
        lock = surface.lock.locale(locale)
        copy_key = config.is_copy_key if surface.use_copy_keys else (lambda key: False)
        untranslated = (surface.source, surface.zh_source) if config.source == "zh" else (surface.source,)
        plan = plan_catalog(surface.source, target, lock, copy_key, surface.protected, limit, untranslated)
        for key in surface.protected:
            if key not in target:
                report.note(f"`{surface.name}` `{locale}` 缺少 `{key}`（需人工填写该语言自称）")
        counts = {
            "copied": len(plan.copies),
            "adopted": len(plan.adopted),
            "removed": len(plan.orphans),
            "deferred": plan.deferred,
        }
        if translate is None:
            counts["translated"] = len(plan.pending)
            report.add(surface.name + "（演练，译出=待译）", locale, counts, {})
            continue
        done, failures = _translate_plan(surface, config, plan, target, lock, translate, workers)
        counts["translated"] = len(done)
        counts["failed"] = len(failures)
        report.add(surface.name, locale, counts, failures)
        if not (done or plan.copies or plan.adopted or plan.orphans or _stale_lock_keys(surface, lock)):
            continue
        existing_order = list(target)
        updates = {**plan.copies, **done}
        for key, value in updates.items():
            target[key] = value
            lock[key] = short_hash(surface.source[key])
        for key in plan.adopted:
            lock[key] = short_hash(surface.source[key])
        for key in plan.orphans:
            target.pop(key, None)
        for key in _stale_lock_keys(surface, lock):
            lock.pop(key, None)
        order = merge_order(existing_order, list(updates), set(plan.orphans), list(surface.source))
        surface.write(locale, target, order)
        changed = True
    if changed:
        surface.lock.save()
    if translate is not None and surface.after is not None:
        surface.after()
    return changed


def _stale_lock_keys(surface: Surface, lock: Dict[str, str]) -> List[str]:
    return [key for key in lock if key not in surface.source]


def _translate_plan(
    surface: Surface,
    config: LocaleConfig,
    plan: CatalogPlan,
    target: Dict[str, str],
    lock: Dict[str, str],
    translate: Translate,
    workers: int,
) -> Tuple[Dict[str, str], Dict[str, str]]:
    source_text: Dict[str, Tuple[str, str]] = {}
    for key in plan.pending:
        zh_value = surface.zh_source.get(key, "")
        if config.source == "zh" and zh_value.strip():
            source_text[key] = (zh_value, "Chinese")
        else:
            source_text[key] = (surface.source[key], "English")
    pending = set(plan.pending)
    pairs = []
    for key, value in target.items():
        if key in pending or key not in surface.source or lock.get(key) != short_hash(surface.source[key]):
            continue
        origin = surface.zh_source.get(key, "") if config.source == "zh" else surface.source[key]
        pairs.append((origin, value))
    if surface.memory is not None:
        pairs.extend(surface.memory(config.locale))
    return translate_catalog(translate, config, plan.pending, source_text, MemoryPool(pairs), workers)


# ── assets/i18n（JSON）──────────────────────────────────────────────


def _read_json(path: Path) -> Dict[str, str]:
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or not all(isinstance(v, str) for v in data.values()):
        raise ConfigError(f"{path.relative_to(ROOT)} 必须是 string → string 的扁平对象")
    return data


def app_surface() -> Surface:
    def read(locale: str) -> Dict[str, str]:
        return _read_json(APP_DIR / f"{locale}.json")

    def write(locale: str, values: Dict[str, str], order: List[str]) -> None:
        ordered = {key: values[key] for key in order}
        (APP_DIR / f"{locale}.json").write_text(json.dumps(ordered, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    locales = sorted(p.stem for p in APP_DIR.glob("*.json") if p.stem not in ("en", "zh"))
    return Surface(
        name="app",
        lock=SourceLock(APP_LOCK, "assets/i18n/en.json"),
        source=read("en"),
        zh_source=read("zh"),
        locales=locales,
        read=read,
        write=write,
        protected=APP_PROTECTED,
        use_copy_keys=True,
    )


# ── 官网界面文案（bun 抽取 / 生成）────────────────────────────────────


def _web_messages(command: str) -> str:
    """`website-v2/scripts/i18n-messages.ts`：extract 打印翻译单元，generate 据 store 生成 TS 模块。"""
    try:
        result = subprocess.run(
            ["bun", "scripts/i18n-messages.ts", command], cwd=WEB_DIR, capture_output=True, text=True, check=False
        )
    except FileNotFoundError as exc:
        raise ConfigError("官网文案需要 bun（并在 website-v2 下 bun install）") from exc
    if result.returncode != 0:
        raise ConfigError(f"bun scripts/i18n-messages.ts {command} 失败：{result.stderr.strip()[-2000:]}")
    return result.stdout


def website_surface() -> Surface:
    try:
        units = json.loads(_web_messages("extract"))["units"]
    except (json.JSONDecodeError, KeyError, TypeError) as exc:
        raise ConfigError(f"i18n-messages.ts extract 输出不是 {{\"units\": {{…}}}}：{exc!r}") from exc
    if not isinstance(units, dict) or not all(isinstance(v, str) for v in units.values()):
        raise ConfigError("i18n-messages.ts extract 的 units 必须是 string → string")

    def read(lang: str) -> Dict[str, str]:
        path = WEB_STORE_DIR / f"{lang}.json"
        return _read_json(path) if path.is_file() else {}

    def write(lang: str, values: Dict[str, str], order: List[str]) -> None:
        WEB_STORE_DIR.mkdir(parents=True, exist_ok=True)
        ordered = {key: values[key] for key in order}
        (WEB_STORE_DIR / f"{lang}.json").write_text(json.dumps(ordered, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    def generate() -> None:
        _web_messages("generate")

    def app_memory(locale: str) -> List[Tuple[str, str]]:
        path = APP_DIR / f"{locale}.json"
        if not path.is_file():
            return []
        english = _read_json(APP_DIR / "en.json")
        return [(english[key], value) for key, value in _read_json(path).items() if english.get(key, value) != value]

    return Surface(
        name="website",
        lock=SourceLock(WEB_LOCK, "website-v2/src/i18n/messages"),
        source=units,
        zh_source={},
        locales=[lang for lang in website_langs() if lang not in ("en", "zh")],
        read=read,
        write=write,
        protected=(),
        use_copy_keys=False,
        config_for=site_locale_config,
        after=generate,
        memory=app_memory,
    )


# ── 浏览器扩展（TS 对象字面量）─────────────────────────────────────

_STR = r'"(?:[^"\\]|\\.)*"'
_ONE_LINE = re.compile(r'^(\s*)(' + _STR + r'):\s*(' + _STR + r'),?\s*$')
_KEY_LINE = re.compile(r'^(\s*)(' + _STR + r'):\s*$')
_VALUE_LINE = re.compile(r'^\s*(' + _STR + r'),?\s*$')
# 对象字面量的首尾：`const x: Record<MessageKey, string> = {` / `const zhCN = {`，
# `};` / `} as const;` / `} as const satisfies Record<MessageKey, string>;`。
_OPEN = re.compile(r"^const \w+(?::[^=]+)? = \{\s*$")
_CLOSE = re.compile(r"^\}(?: as const)?(?: satisfies [^;]+)?;\s*$")
PRINT_WIDTH = 80


@dataclass
class TsCatalog:
    """扩展语言文件：保留注释、空行与原有布局，只按条改写。"""

    lines: List[str]
    spans: Dict[str, Tuple[int, int]]
    values: Dict[str, str]

    @classmethod
    def parse(cls, text: str, name: str) -> "TsCatalog":
        lines = text.split("\n")
        start = next((i for i, line in enumerate(lines) if _OPEN.match(line)), None)
        if start is None:
            raise ConfigError(f"{name}: 找不到 `const x: Record<MessageKey, string> = {{`")
        spans: Dict[str, Tuple[int, int]] = {}
        values: Dict[str, str] = {}
        index = start + 1
        while index < len(lines):
            line = lines[index]
            stripped = line.strip()
            if _CLOSE.match(stripped):
                return cls(lines, spans, values)
            one = _ONE_LINE.match(line)
            key_only = _KEY_LINE.match(line)
            if one:
                key, value, end = json.loads(one.group(2)), json.loads(one.group(3)), index
            elif key_only and index + 1 < len(lines) and _VALUE_LINE.match(lines[index + 1]):
                key = json.loads(key_only.group(2))
                value = json.loads(_VALUE_LINE.match(lines[index + 1]).group(1))
                end = index + 1
            elif not stripped or stripped.startswith("//"):
                index += 1
                continue
            else:
                raise ConfigError(f"{name}:{index + 1}: 无法解析的条目（只支持双引号字符串字面量）：{stripped[:80]}")
            if key in spans:
                raise ConfigError(f"{name}:{index + 1}: 重复键 {key}")
            spans[key] = (index, end)
            values[key] = value
            index = end + 1
        raise ConfigError(f"{name}: 对象没有以 `}};` 结束")

    @staticmethod
    def entry(key: str, value: str) -> List[str]:
        """按 prettier 默认 80 列习惯：放得下写一行，否则值换到下一行。"""
        k = json.dumps(key, ensure_ascii=False)
        v = json.dumps(value, ensure_ascii=False)
        one = f"  {k}: {v},"
        if len(one) <= PRINT_WIDTH:
            return [one]
        return [f"  {k}:", f"    {v},"]

    def render(self, values: Dict[str, str], order: List[str], changed: set) -> str:
        """changed 里的键重写，order 里没有的旧键删除，新键插在 order 中紧邻的前一个键之后。"""
        keep = set(order)
        # order 是线性序：每个新键只有一个直接前驱（None = 对象开头）。
        follow: Dict[Optional[str], str] = {}
        previous: Optional[str] = None
        for key in order:
            if key not in self.spans:
                follow[previous] = key
            previous = key
        out: List[str] = []

        def emit_after(anchor: Optional[str]) -> None:
            while anchor in follow:
                added = follow.pop(anchor)
                out.extend(self.entry(added, values[added]))
                anchor = added

        span_start = {start: key for key, (start, _) in self.spans.items()}
        index = 0
        while index < len(self.lines):
            key = span_start.get(index)
            if key is None:
                out.append(self.lines[index])
                if _OPEN.match(self.lines[index]):
                    emit_after(None)
                index += 1
                continue
            start, end = self.spans[key]
            if key in keep:
                out.extend(self.entry(key, values[key]) if key in changed else self.lines[start : end + 1])
                emit_after(key)
            index = end + 1
        return "\n".join(out)


def extension_surface() -> Surface:
    cache: Dict[str, TsCatalog] = {}

    def load(locale: str) -> TsCatalog:
        if locale not in cache:
            path = EXT_DIR / f"{locale}.ts"
            cache[locale] = TsCatalog.parse(path.read_text(encoding="utf-8"), str(path.relative_to(ROOT)))
        return cache[locale]

    def read(locale: str) -> Dict[str, str]:
        return dict(load(locale).values)

    def write(locale: str, values: Dict[str, str], order: List[str]) -> None:
        catalog = load(locale)
        changed = {key for key in order if catalog.values.get(key) != values[key]}
        text = catalog.render(values, order, changed)
        name = f"fluxDown/utils/locales/{locale}.ts"
        # 写回前自检：重新解析的结果必须与期望完全一致（键序、值），否则宁可中止也不推坏文件。
        if list(TsCatalog.parse(text, name).values.items()) != [(key, values[key]) for key in order]:
            raise ConfigError(f"{name}: 重写结果与期望不一致，已中止写入")
        (EXT_DIR / f"{locale}.ts").write_text(text, encoding="utf-8")
        cache.pop(locale, None)

    locales = sorted(p.stem for p in EXT_DIR.glob("*.ts") if p.stem not in ("en", "zh-CN"))
    return Surface(
        name="extension",
        lock=SourceLock(EXT_LOCK, "fluxDown/utils/locales/en.ts"),
        source=read("en"),
        zh_source=read("zh-CN"),
        locales=locales,
        read=read,
        write=write,
        protected=(),
        use_copy_keys=False,
    )
