# Cursor 薄适配层

**唯一事实源是 `.omp/`**（外加根目录 `AGENTS.md`）。本目录只让 Cursor 加载同一套契约，**不复制规则 / skill / 命令正文**。

## 映射

| Cursor | 指向 |
|---|---|
| `rules/sticky-redlines.mdc`（alwaysApply） | `.omp/RULES.md` + config 里的跨仓根目录 |
| `rules/<name>.mdc` | `.omp/rules/<name>.md`（description / globs 在适配器；正文在 omp） |
| `hooks/omp-guard.mjs` + `hooks.json` | `.omp/hooks/lib/check_bash.mjs`（与 omp `pre/guard.ts` 共用判定；共享模块勿放 `pre/`） |
| `skills/<name>/SKILL.md` | `.omp/skills/<name>/SKILL.md`（触发 description 复制，正文指向） |
| `skills/build-verifier/SKILL.md` | `.omp/agents/build-verifier.md` |
| `commands/verify.md` | `.omp/commands/verify.md` |

知识附录仍按需读 `.omp/knowledge/*`（见 `AGENTS.md` §0），不在此镜像。

## 维护（omp ↔ Cursor **双向**同改）

改映射面**任一端**，同回合必须更新另一端；只改 `.omp` 或只改 `.cursor` = 未完成。契约正文只写 `.omp/`，禁止在本目录单独发明新规则。

1. **新增** `.omp/rules/foo.md` → 新增 `.cursor/rules/foo.mdc`（抄 description；把 omp `globs` / 路径型 `condition` 转成 Cursor `globs`；正文只写「先 Read `.omp/rules/foo.md`」）。反向：新增 `.mdc` 必须先有对应 omp 规则。
2. **改** 任一侧规则的 description / globs → 同步另一侧（omp frontmatter ↔ `.mdc`）。
3. **删** 任一侧规则 → 删另一侧对应文件。
4. **改** skill / command / agent 触发描述（任一侧）→ 同步 `.cursor/skills|commands` 与 `.omp/skills|commands|agents`。
5. **改** 命令守卫判定 → 只改 `.omp/hooks/lib/check_bash.mjs`；若改 hook 协议面，omp `pre/guard.ts` 与本目录 `hooks/omp-guard.mjs` / `hooks.json` 一起看（`pre/` 下只能有扩展工厂）。
6. **改** `.omp/RULES.md` 或 `config.yml` 的 `additionalDirectories` → 同步 `rules/sticky-redlines.mdc`。

细则与同改矩阵见 `rule://agent-docs-maintenance`（`.omp/rules/agent-docs-maintenance.md`）。

## 刻意不对齐

- omp TTSR 流式打断、advisor / `WATCHDOG`、approvalMode、lsp、compaction：Cursor 无对等能力；靠软规则 + Shell hook + clippy / CI。
- 不要把 `.omp/knowledge` 或规则全文拷进 `.cursor/`。
