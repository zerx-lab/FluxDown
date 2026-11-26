"""离线单测（假翻译器，不调接口）：python3 -m unittest discover -s scripts/i18n-mt -p 'test_*.py'"""

from __future__ import annotations

import unittest

from catalogs import TsCatalog, merge_order
from common import LocaleConfig, MemoryPool, check_translation, numbered_mask, plan_catalog, short_hash
from docs import DOCS, DocDraft, DocPlan, align_blocks, heading_slugs, map_anchors, render_doc


def fake_translate(text, source_lang, target_lang, terms, tm, domains):
    return "译" + text


def config(**overrides):
    values = dict(
        locale="zh", target_lang="Chinese", source="en", domains="", keep=["FluxDown"], terms={}, copy_keys=["searchKeywords*"]
    )
    values.update(overrides)
    return LocaleConfig(**values)


class CatalogPlanTest(unittest.TestCase):
    def test_classifies_keys(self):
        source = {
            "languageNativeName": "English",
            "fresh": "Hello",
            "stale": "Changed text",
            "manual": "Manual",
            "missing": "Missing {name}",
            "searchKeywordsA": "a b",
            "symbols": "{n}%",
        }
        target = {"languageNativeName": "Español", "fresh": "Hola", "stale": "Viejo", "manual": "Manual es", "gone": "x"}
        lock = {"fresh": short_hash("Hello"), "stale": short_hash("Old text")}
        cfg = config()
        plan = plan_catalog(source, target, lock, cfg.is_copy_key, ("languageNativeName",), None)
        self.assertEqual(plan.pending, ["stale", "missing"])
        self.assertEqual(plan.adopted, ["languageNativeName", "manual"])
        self.assertEqual(plan.copies, {"searchKeywordsA": "a b", "symbols": "{n}%"})
        self.assertEqual(plan.orphans, ["gone"])

    def test_limit_defers_rest(self):
        source = {f"k{i}": f"text {i}" for i in range(5)}
        plan = plan_catalog(source, {}, {}, lambda key: False, (), 2)
        self.assertEqual((plan.pending, plan.deferred), (["k0", "k1"], 3))

    def test_merge_order_keeps_target_order_and_inserts_after_source_neighbour(self):
        order = merge_order(["b", "a", "old"], ["c", "z"], {"old"}, ["z", "a", "b", "c"])
        self.assertEqual(order, ["z", "b", "c", "a"])


class CheckTest(unittest.TestCase):
    def test_placeholders_newlines_and_big5(self):
        self.assertIsNone(check_translation("Hi {name}", "嗨 {name}", True))
        self.assertIn("占位符", check_translation("Hi {name}", "嗨 {nombre}", False))
        self.assertIn("换行", check_translation("a\nb", "ab", False))
        self.assertIn("Big5", check_translation("Files", "文件设置", True))

    def test_numbered_mask_roundtrip(self):
        masked, restore = numbered_mask("{count} of {total} from {count}")
        self.assertEqual(masked, "{1} of {2} from {1}")
        self.assertEqual(restore("{2} 中的 {1}"), "{total} 中的 {count}")

    def test_memory_prefers_overlap(self):
        pool = MemoryPool([("Pause all downloads", "全部暂停"), ("Open settings", "打开设置")])
        self.assertEqual(pool.select("Pause selected downloads")[0]["target"], "全部暂停")

    def test_terms_match_whole_words_only(self):
        cfg = config(terms={"Task": "任务"})
        self.assertEqual(cfg.terms_for("New task"), [{"source": "Task", "target": "任务"}])
        self.assertEqual(cfg.terms_for("Multitasking"), [])


TS = """/**
 * Russian
 */
import type { MessageKey } from "./zh-CN";

const ru: Record<MessageKey, string> = {
  // Header
  "a": "A",
  "long":
    "A long value that was wrapped by prettier because it does not fit in eighty columns",

  "c": "C",
};

export default ru;
"""


class TsCatalogTest(unittest.TestCase):
    def test_roundtrip_without_changes(self):
        catalog = TsCatalog.parse(TS, "ru.ts")
        self.assertEqual(catalog.values["long"][:6], "A long")
        self.assertEqual(catalog.render(catalog.values, list(catalog.values), set()), TS)

    def test_insert_rewrite_delete(self):
        catalog = TsCatalog.parse(TS, "ru.ts")
        values = dict(catalog.values, a='Новый "A"', b="B", first="F")
        rendered = catalog.render(values, ["first", "a", "b", "long"], {"a", "b", "first"})
        self.assertIn('= {\n  "first": "F",\n  // Header\n  "a": "Новый \\"A\\"",\n  "b": "B",\n  "long":\n', rendered)
        self.assertNotIn('"c"', rendered)
        self.assertEqual(TsCatalog.parse(rendered, "ru.ts").values["a"], 'Новый "A"')


class DocsTest(unittest.TestCase):
    def test_segment_masks_code_and_rewrites_links(self):
        draft = DocDraft(config(), MemoryPool([]), "doc")
        render = draft.segment("Run `fluxdown add` then see [the API](/docs/en/api/overview/#auth) or https://x.dev/a.")
        from common import run_units

        results, failures = run_units(fake_translate, draft.units, 1)
        self.assertEqual(failures, {})
        self.assertEqual(
            render(results), "译Run `fluxdown add` then see [译the API](/docs/zh/api/overview/#auth) or https://x.dev/a."
        )

    def test_block_kinds(self):
        draft = DocDraft(config(), MemoryPool([]), "doc")
        table = draft.block("| Field | Rules |\n|---|---|\n| `id` | Must be `x \\| y` |")
        code = draft.block("```bash\nfluxdown add url\n```")
        listing = draft.block("- **Bold** item\n  1. nested")
        from common import run_units

        results, _ = run_units(fake_translate, draft.units, 1)
        self.assertEqual(table(results), "| 译Field | 译Rules |\n|---|---|\n| `id` | 译Must be `x \\| y` |")
        self.assertEqual(code(results), "```bash\nfluxdown add url\n```")
        self.assertEqual(listing(results), "- 译**Bold** item\n  1. 译nested")

    def test_heading_slugs_follow_github_slugger(self):
        body = "# Hi\n## MCP（Model Context Protocol）\n```\n# not heading\n```\n## curl 示例\n## Hi"
        self.assertEqual(heading_slugs(body), ["hi", "mcpmodel-context-protocol", "curl-示例", "hi-1"])

    def test_render_reuses_unchanged_blocks_and_maps_anchors(self):
        en = "---\ntitle: Guide\norder: 2\n---\n\n## Setup\n\nKeep me.\n\nSee [setup](#setup).\n"
        old = "---\ntitle: Guide\norder: 2\n---\n\n## Setup\n\nKeep me.\n"
        zh = '---\ntitle: 指南\norder: 2\nsourceHash: "old"\n---\n\n## 安装\n\n保留我。\n'
        plan = DocPlan(DOCS / "en" / "x.md", DOCS / "zh" / "x.md", en, old, zh, {0: "## 安装", 1: "保留我。"}, None)
        text, failures, fresh = render_doc(plan, config(), fake_translate, 1)
        self.assertEqual(failures, {})
        self.assertEqual(fresh, [2])
        final = map_anchors(text, fresh, en, {})
        self.assertIn("title: 指南\norder: 2\nsourceHash:", final)
        self.assertTrue(final.endswith("## 安装\n\n保留我。\n\n译See [译setup](#安装).\n"))

    def test_align_blocks_skips_extra_zh_block_by_signature(self):
        en = ["## Install", "Run `a`.", "```sh\nx\n```", "See [b](/docs/en/b/#top)."]
        zh = ["## 安装", "运行 `a`。", "补充说明。", "```sh\nx\n```", "参见 [b](/docs/zh/b/#顶部)。"]
        self.assertEqual(align_blocks(en, zh), {0: 0, 1: 1, 2: 3, 3: 4})


if __name__ == "__main__":
    unittest.main()
