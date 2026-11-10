---
description: 按改动范围跑 FluxDown 的门禁（fmt、改动行严格 lint、crate clippy、精准测试、镜像契约、CI 路径），报告实际跑了什么
---

# /verify（Cursor 薄适配）

完整流程见 [`.omp/commands/verify.md`](.omp/commands/verify.md)。

在 Cursor 中执行 `/verify` 时：**先 Read 该文件，并按其中步骤在主会话逐条串行执行**（不要派子代理并行跑 cargo / gradle / xcodebuild）。参数语义与 omp `$ARGUMENTS` 相同（留空 = 自 origin/main 分叉以来的全部改动）。本适配器不复制正文。
