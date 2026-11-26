---
description: FluxDown 多语言以英文 + 简体中文为开发基线；assets/i18n 的其余语言（zh-TW 等）、扩展 ru/zh-TW、官网文档 zh 由 GitHub Actions「i18n MT」（qwen-mt）自动翻译，开发中只补基线
condition:
  - 'assets/i18n/*.json'
  - 'web/src/i18n/*'
  - 'website-v2/src/i18n/messages/*'
  - 'fluxDown/utils/locales/*'
interruptMode: never
---

你正在改 FluxDown 的翻译文件。**开发中只维护 en + zh 两条基线**；`assets/i18n/` 下的其余语言（目前 `es`/`ru`/`tr`/`zh-TW`）、扩展的 `ru.ts`/`zh-TW.ts`、官网文档 `zh/` 页由 GitHub Actions 工作流 `i18n-mt.yml`（阿里 qwen-mt）在合入 main 后自动翻译（见下文「非基线语言」），开发者新增/改文案时**不必、也不要**手译它们。

## 三个 i18n 文件面各自的基线对（缺一不可）

| 面 | 基线文件 | 备注 |
|---|---|---|
| GPUI（`crates/*`）+ Web SPA（`web/`）+ 原生 Android/iOS（`mobile/`） | `assets/i18n/en.json` + `assets/i18n/zh.json` | 四端共用同一 camelCase 键：GPUI `ctx.t("key")`；Web `useT()`；Android 构建期生成 `R.string`；iOS `L("key")`。删键前检查 `crates/`、`web/src/` 与 `mobile/` 的消费者 |
| 官网（`website-v2/`） | `website-v2/src/i18n/messages/<ns>.ts` | 每个模块用 `defineMessages({ en, zh })` 同时声明两语，zh 结构必须与 en 完全一致，缺键/多键 `astro check` 直接报错；只有 en + zh，无社区语言 |
| 浏览器扩展 | `fluxDown/utils/locales/zh-CN.ts` + `en.ts` + `ru.ts` + `zh-TW.ts` | 反过来：`MessageKey` 类型由 **`zh-CN.ts`** 推导，`en.ts`、`ru.ts`、`zh-TW.ts` 都受 `Record<MessageKey, string>` 约束，少一个键直接 tsc 报错。所以新增键时 `zh-CN.ts` + `en.ts` 写好正式文案，**`ru.ts` / `zh-TW.ts` 先原样复制 en 文本当占位**（tsc 才过）；合入 main 后自动化把占位译掉（与 en 相同的值视为未翻译） |

官网文档另算：`website-v2/src/content/docs/{en,zh}/`。en 是源，改 en 正文合入 main 后，工作流按 en 正文哈希 ≠ zh `sourceHash` 自动起草 / 增量更新 zh 页（只重译新增或变化的块，写回新 `sourceHash`）；手工翻译 zh 仍然有效，在 `website-v2/` 下跑 `npm run docs:hash <zh 文件>` 刷新 `sourceHash`。en-only 页会自动回退，不必为其造 zh 占位。官网 UI 文案（`website-v2/src/i18n/messages`）仍手写 en + zh，不在自动化范围。

## 加/改一个 UI 字符串的完整动作

1. `en.json` 与 `zh.json`（官网为同一 `defineMessages` 的 `en`/`zh` 两支；扩展为 `zh-CN.ts` + `en.ts`，另 `ru.ts` + `zh-TW.ts` 复制 en 文本占位）**同名键、同时补上、都非空**。
2. 各端通过已有 i18n 查表/生成入口消费共享键，参数化文案使用 `{name}` 占位；不要建立平行翻译表。
3. 删键同理两边一起删；改键名等于删旧加新。
4. **不要手补 `assets/i18n/` 的非基线语言**：缺键在运行时按键级回退英文，不是 bug；合入 main 后自动化发现并直接补齐。人工修正某条已有译文可以直接改（锁文件里有记录的译文，只在 en 原文变化时才重译；无记录的现存译文视为人工译文，不覆盖）；同类术语问题优先改 `i18n/glossary/<locale>.md`。
5. GPUI（`build.rs`）、Web（Vite 插件 / bun 预加载生成 `virtual:i18n-tables`，见 `web/i18n-tables.ts`）、Android（`GenerateI18nResources` + `localeFilters`）与 iOS 运行时都自动发现 `assets/i18n/*.json`；唯一手动项是 iOS 的 `mobile/FluxDown/Config/Info.plist` `CFBundleLocalizations` 与工程 `knownRegions`（否则系统 App 语言设置不列出），`mobile/FluxDown/scripts/check-i18n.py` 会报未声明的语言。新增一门语言 = 加 JSON（至少含 `languageNativeName`）+ iOS 两处声明 + `i18n/glossary/<locale>.md`（含 `## qwen-mt 机器翻译配置` JSON 块，缺则该语言不被翻译），之后自动化负责补全。官网 v2 的语言集固定在 `website-v2/src/i18n/config.ts::LANGS`（en + zh），UI 文案不做翻译。
6. 原生移动端：Android 构建期由共享 JSON 生成 `R.string.<键>`，**禁止**在 `res/` 手写同名文案；iOS 经 FluxKit 符号链接打包同一 JSON、`L("键")` 查表，`mobile/FluxDown/scripts/check-i18n.py` 校验漏键。

## 非基线语言（自动化维护）

- 范围：①`assets/i18n/` 下除 `en.json` / `zh.json` 外的语言文件（GPUI / Web / Android / iOS 共用；新增 JSON 自动发现，需有 `i18n/glossary/<locale>.md`）；②扩展 `fluxDown/utils/locales/{ru,zh-TW}.ts`；③官网文档 `website-v2/src/content/docs/zh/**`。官网 UI 文案不在范围。
- 流程：GitHub Actions 工作流 `.github/workflows/i18n-mt.yml`（「i18n MT」，脚本 `scripts/i18n-mt/`，仅 Python 标准库，调用阿里 qwen-mt）。触发：push 到 main 且改动了 en/zh 基线、扩展 en/zh-CN、`docs/en/**`、`i18n/glossary/**`、`scripts/i18n-mt/**` 或工作流本身；每日 06:30 UTC+8 定时重试；`workflow_dispatch`（target all/app/extension/docs、locales、limit、dry_run）。跑完离线单测 → 翻译 → 改了 `assets/i18n` 则跑 `cargo test -p fluxdown_ui_i18n --lib translated_locales_keep_baseline_placeholders`、改了文档则跑 `node scripts/docs-lint.mjs` → 以 github-actions[bot] 提交 `chore(i18n): qwen-mt 自动翻译同步` 并**直接推送 main**（用户 2026-11-26 决定：不开 PR、无人工审核）。密钥 `DASHSCOPE_API_KEY`（必需）、`I18N_BOT_TOKEN`（可选 PAT / App token：让推送能触发下游 CI、绕过 main 分支保护；缺省退回 `GITHUB_TOKEN`，其推送不触发其他工作流）；变量 `QWEN_MT_MODEL`（默认 `qwen-mt-plus`）、`DASHSCOPE_BASE_URL`（默认北京站）。
- 待译判定（应用与扩展）：锁文件按语言、按键记录译文对应的 en 原文 sha256 前 12 位——应用 `i18n/source-lock.json`，扩展 `i18n/extension-lock.json`；只由自动化写，不要手改。缺键或 en 哈希变化即待译；有值但无锁记录视为人工译文而采纳，**但与源文本（en；zh-TW 为 zh）完全相同的值算未翻译占位**；孤儿键自动删；`languageNativeName` 永不机翻；术语表 `copyKeys` 匹配的键与不含字母的值原样复制。zh-TW 由简体 `zh.json` / `zh-CN.ts` 翻译而非 en，且含 Big5（cp950）外字符的输出会被拒绝；占位符集必须与源一致，失败项保持原状、下轮重试；每轮每语言上限 1500 条。
- 文档：en 正文哈希 ≠ zh `sourceHash` 时，在 git 历史里找回该哈希对应的旧 en 版本，对齐块后复用未变块的现有 zh 译文，只译新增 / 变化块并写新 `sourceHash`；zh 页缺失则整页起草；`sourceHash` 在历史里找不到则跳过并提示（需人工）；任一段失败则本轮不写该文档。
- 术语表：`i18n/glossary/<locale>.md`（`es`/`ru`/`tr`/`zh-TW`，另有仅供文档用的 `zh.md`）末尾 `## qwen-mt 机器翻译配置` JSON 块含 `targetLang`、`source`（en|zh）、`domains`、`keep`、`terms`、`copyKeys`。qwen-mt 无 system prompt，术语 / 翻译记忆 / 领域经 `translation_options` 传入，只发送文本中出现的术语。
- 兜底：`crates/i18n` 单测 `translated_locales_keep_baseline_placeholders` 要求非基线语言的占位符不超出 en ∪ zh、不丢 en ∩ zh 都有的占位符，人工改译文同样受约束。

## 自检（在 FluxDown 仓根执行）

```bash
node -e "const a=require('./assets/i18n/en.json'),b=require('./assets/i18n/zh.json');const A=Object.keys(a),B=Object.keys(b);console.log('zh 缺/空:',A.filter(k=>!(k in b)||!b[k]));console.log('zh 多余:',B.filter(k=>!(k in a)))"
```

上面的脚本只适用于 `assets/i18n` 基线对：官网由 `cd website-v2 && bun run check`（`astro check`）把关；扩展侧由 `tsc` 把关，跑 `cd fluxDown && npx tsc --noEmit` 或直接看 IDE 报错（`ru.ts` / `zh-TW.ts` 缺键同样报错，补 en 占位即可）。非基线语言的缺键不用检查。
