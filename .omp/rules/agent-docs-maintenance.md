---
description: 改 AGENTS.md / .omp/** / .cursor/** 时的分层归属、维护约定与同改矩阵（新知识该落在哪一层）
condition: '[\s\S]'
scope: "tool:edit, tool:write"
globs:
  - '**/AGENTS.md'
  - '**/.omp/**'
  - '**/.cursor/**'
interruptMode: never
---

你正在改 AI 工作契约（`AGENTS.md` / `.omp/**` / Cursor 薄适配 `.cursor/**`）。先判定内容该落在哪一层，再按同改矩阵把配套面一次改完。

## omp ↔ Cursor 双向同改（硬不变式）

- **`.omp/` 是契约正文唯一事实源**；`.cursor/` 只放薄适配（description / globs / hook 协议 / skill 触发 description；正文指向 `.omp/`，见 `.cursor/README.md`）。
- **改映射面的任一端，同回合必须更新另一端**；只改 `.omp` 或只改 `.cursor` = 未完成，不得交付。
- 方向（契约变更永远先落 omp，再跟适配器；禁止在 `.cursor` 单独发明新契约）：
  - 新增 / 改名 / 删 `.omp/rules/<名>.md`，或改其 `description` / `globs` → 同回合补 / 改 / 删 `.cursor/rules/<名>.mdc`（Cursor `globs` 由 omp 列表转换，含 brace 展开）。
  - 改 `.cursor/rules/<名>.mdc` 的 description / globs / 增删 → **必须**同步回 `.omp/rules/<名>.md`（或删掉无 omp 源的适配器）；不得让 `.mdc` 成为另一份事实源。
  - `.omp/skills/*` / `.omp/commands/*` / `.omp/agents/*` ↔ `.cursor/skills|commands`（`build-verifier`：omp agent → Cursor skill）同理：触发 description 与文件增删双侧对齐。
  - 命令守卫**判定**只改 `.omp/hooks/lib/check_bash.mjs`；若改 hook 协议面（入参 / 放行字段），同回合检查 omp `pre/guard.ts` 与 `.cursor/hooks/omp-guard.mjs` + `hooks.json`。
  - 改 `.omp/RULES.md` 或 `config.yml` 的 `workspace.additionalDirectories` → 同步 `.cursor/rules/sticky-redlines.mdc`。
- **刻意不对齐（不要求镜像）**：omp TTSR 流式打断、advisor / `WATCHDOG`、approvalMode、lsp、compaction；Cursor 靠软规则 + Shell hook + clippy / CI。
- **交付前自检**：本回合若动过 `.omp/{rules,skills,agents,commands}`、`.omp/RULES.md`、`.omp/config.yml`、`.omp/hooks/**` 或 `.cursor/{rules,skills,commands,hooks}`，确认对应另一侧已改（或属于上条例外）；`git status` 不应只出现单侧映射文件。

## 分层归属（按「何时需要在上下文里」选层，宁低勿高）

|层|载体|进上下文的时机|只放什么|
|---|---|---|---|
|粘性红线|`.omp/RULES.md`|每次请求（长会话也不掉）|不可逆操作门槛、分支模型、用户明确拍板的产品决定（写日期）。保持 ≤ 3 KB|
|核心契约|`AGENTS.md`|会话开头一次|架构缝、跨域硬不变式（每条一两行）、命令、单一事实源坐标、各层索引。保持约 20 KB；新增一条前先看能否下沉到路径规则或附录|
|路径规则|`.omp/rules/*.md`（`condition: '[\s\S]'` + `scope: "tool:edit, tool:write"` + `globs: ['**/路径']` + `interruptMode: never`）|编辑命中路径时注入一次|只在改某处时才需要的约束：镜像契约（`mirror-*`）、领域细则、补丁陷阱|
|流式拦截规则|`.omp/rules/*.md`（`condition` 正则 / `astCondition`，可打断）|输出命中模式时|机械可判的违规写法（unwrap、unsafe、禁用命令）|
|执行前拦截|`.omp/hooks/lib/check_bash.mjs` + `pre/guard.ts`（omp）/ `.cursor/hooks/omp-guard.mjs`（Cursor）|工具调用前|不可绕过的命令红线；判定只维护在 `hooks/lib/`（勿放进 `pre/`，omp 会当扩展加载）|
|知识附录|`.omp/knowledge/*.md`|按需 `read`|枚举细节、子系统说明、踩坑（`pitfalls.md`，带代码坐标）|
|Cursor 薄适配|`.cursor/rules|hooks|skills|commands`|Cursor 会话|仅 frontmatter / globs / hook 协议 / skill 触发 description；正文一律指向 `.omp/`（见 `.cursor/README.md`）|

- 不依赖 omp memory 后端存项目知识：它在本机 agent 目录、不随仓分发（当前 `memory.backend` 为 off）。项目知识一律进上面这几层，随仓分发、可审查。
- 新知识归属：用户拍板的产品 / 交互决定 → `RULES.md`（带日期）；有代码坐标的坑 → `pitfalls.md` 对应领域小节；跨端「改 A 必须改 B」→ 对应 `mirror-*` 规则（没有合适的就按路径新建一条）；其余事实 → 对应 knowledge 附录。**不要把契约正文写进 `.cursor/`。**
- 路径规则的 `globs` 一律写 `'**/<仓库相对路径>'`（相对 / 绝对路径都能命中）；只写具体路径不带通配符的 `condition` 会被当成正则匹配内容，**不会**按路径触发。兜底条件用 `'[\s\S]'` 而不是 `'.*'`。新增或改 glob 后用 `omp ttsr test --rule <规则> --source tool --tool edit --path <文件> 'x'` 正反例各验一次；Cursor 侧同步更新 `.cursor/rules/<名>.mdc` 的 `globs`。

## 维护约定

- 只有**架构 / 契约 / 不变式**变化才改 `AGENTS.md`；改一个普通文件不必回来更新它。
- 任何文档都**不**维护「完整文件清单 / 完整设置项清单」这类每次提交都漂移的枚举——源码是唯一事实源（`read <dir>` 看结构，`grep` 查事实）。
- 事实层（版本号、协议数、路由、env、DB 列、设置键）以代码为准，文档只给坐标。
- **知识文档只能放 `.omp/`**：`docs/` 与 `.agents/` 都在 `.gitignore` 里（本地目录、零文件入库、不随仓分发）；`docs/*.md` 只是本机设计草稿，别把契约写进去。`.cursor/` 只放薄适配，不放知识附录副本。

## 同改矩阵

|改了什么|同回合必须过一遍|
|---|---|
|架构缝 / 不变式（`AGENTS.md` §3–§4）|`.omp/knowledge/*` 对应附录；需要编辑期提醒的落路径规则，需要执行期拦截的进 `hooks/lib/check_bash.mjs`（及 omp `pre/guard.ts` / Cursor hook 薄壳）|
|镜像契约（`mirror-*` 规则）|规则的 `globs` 覆盖两侧所有文件；`AGENTS.md` §5 索引表同步增删；`.cursor/rules/mirror-*.mdc` 的 description / globs 同步|
|红线 / 授权 / 禁用命令|`.omp/RULES.md`、`.omp/rules/forbidden-commands.md`（TTSR 提示）、`.omp/hooks/lib/check_bash.mjs` + `pre/guard.ts`、`.cursor/hooks`（`omp-guard.mjs` / `hooks.json`）、`.cursor/rules/sticky-redlines.mdc`、`.omp/WATCHDOG.md`（严重级映射）|
|`.omp/rules/*.md` **或** `.cursor/rules/*.mdc`（增删改名 / description / globs）|另一侧对应文件同回合对齐；契约正文只写 omp，`.mdc` 仍只指向 omp|
|`.omp/skills|commands|agents` **或** `.cursor/skills|commands`|另一侧触发 description 与路径同回合对齐（`build-verifier`：omp agent ↔ Cursor skill）|
|`.omp/hooks/**` **或** `.cursor/hooks/**`|判定只维护在 `hooks/lib/check_bash.mjs`；协议面变更则 omp `pre/guard.ts` 与 Cursor `omp-guard.mjs` / `hooks.json` 一起看|
|CI 触发路径（`ci.yml` 的 `changes.filters`）、新增 crate / path 依赖 / `include_*!` / TS 跨目录引用|`python3 .github/scripts/check_ci_paths.py` 必须通过（CI `ci-paths` job 同跑）|
|`AGENTS.md` 章节编号 / 标题（`guard.ts`、skills、rules、knowledge 都按 `§N` 引用）|`grep "AGENTS.md"` 扫 `.omp/`、`.cursor/`、源码注释，修掉悬空引用|
