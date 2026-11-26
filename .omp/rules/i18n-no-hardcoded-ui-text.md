---
description: FluxDown 面向用户的文案必须走 i18n 查表，UI 代码里禁止硬编码中文字面量
condition:
  - '(?m)^(?!\s*(//|///|\*|#|<!--)).*\b(label|title|subtitle|placeholder|description|hint|hintText|helperText|errorText|tooltip|semanticLabel|confirmText|cancelText|buttonText|emptyText)\s*:\s*(const\s+)?[''"][^''"\n]*[\u4e00-\u9fff]'
  - '(?m)^(?!\s*(//|///|\*|#|<!--)).*\b(placeholder|title|label|aria-label|alt)\s*=\s*"[^"\n]*[\u4e00-\u9fff]'
  - '(?m)^(?!\s*(//|///|\*|#|<!--)).*>[^<>{}\n]{0,80}[\u4e00-\u9fff][^<>{}\n]{0,80}<'
globs:
  - 'web/src/**/*.tsx'
  - 'website-v2/src/**/*.{tsx,astro}'
  - 'fluxDown/**/*.{ts,tsx,html}'
repeatMode: after-gap
repeatGap: 3
---

你正在 FluxDown 的 UI 代码里写死中文文案。**所有面向用户的字段都必须走 i18n 查表**，否则该字段永远只有一种语言，社区也无从翻译。

按所在面替换：

| 面 | 写法 |
|---|---|
| GPUI / 原生移动端 | GPUI `ctx.t("key")`；Android 生成的 `R.string`；iOS `L("key")` |
| Web SPA（`web/`） | `const { t } = useI18n()` → `t('xxx')` |
| 官网（`website-v2/`） | `src/i18n/messages/<ns>.ts` 的 `defineMessages` 键；Astro 侧按页面 `lang` 取 `messages[lang]` |
| 扩展（`fluxDown/`） | `t('section.key')`，或 HTML 上挂 `data-i18n="section.key"` |


同一动作里把键补进**基线双语**：`en.json` + `zh.json`（官网 `defineMessages` 的 `en`/`zh`，扩展 `zh-CN.ts` + `en.ts`；扩展的 `ru.ts` / `zh-TW.ts` 因 tsc 强制只需原样复制 en 文本占位）；原生移动端与 GPUI / Web 消费共享基线。**`assets/i18n/` 的其余语言（zh-TW 等）不用补**（合入 main 后由 GitHub Actions 工作流 `i18n-mt.yml` 的 qwen-mt 自动翻译，包括扩展 ru/zh-TW 的占位；运行时按键级回退英文）——细则见规则 `i18n-baseline-zh-en`。

**可以硬编码的例外**（命中本规则时若属于以下情形，说明一句继续即可）：

- 快捷键 / 符号 / 单位 / 版本号（如 `'Ctrl+A'`、`'MB/s'`），以及品牌名、URL、协议名、CLI 子命令等专有名词。
- **语言自称**：语言选择器里的 `简体中文` / `English` / `日本語` 等条目（`NATIVE_NAMES`、`<option value="zh-CN">简体中文</option>`）本就不该被翻译。
- 非 UI 字符串：日志文案、异常 message、注释、测试断言、`debugPrint`。
- 纯中文管理后台（FluxCloud `admin/`）无 i18n 层，不适用本规则。
