---
name: pr-review
description: >-
  Review、补完并合并 FluxDown 的 GitHub PR：PR 拉到 .worktrees/pr-<N> 隔离检查， 从功能 / 性能 / 完整性三个角度审查后分级处理——方向错误直接关闭，实现逻辑错误评论退回， 格式化 / 冲突 / 缺少几条测试等小问题直接修好推回 PR 分支再合并，结束必清理 worktree。 关键词：PR, pull request, review, 审查, 代码评审, 合并 PR, merge, 处理 PR, 贡献者, contributor, 补测试, 解决冲突, 推回 PR 分支, maintainer edits
---

# pr-review（Cursor 薄适配）

完整 skill 正文见 [`.omp/skills/pr-review/SKILL.md`](.omp/skills/pr-review/SKILL.md)。

触发本技能时：**必须先 Read 该文件并遵守全文**（含其中引用的 scripts 路径，仍以 `.omp/skills/pr-review/` 为准）。本适配器只保留触发用 description，不复制正文。
