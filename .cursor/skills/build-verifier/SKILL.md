---
name: build-verifier
description: >-
  只读构建验证员。对指定 crate/子项目运行编译检查、lint 与精准测试并汇总结果，不修改任何文件。在多文件改动完成后派发它做终验。 在 Cursor 中：多文件改动完成后派只读子代理终验时使用本 skill。
---

# build-verifier（Cursor 薄适配）

完整提示词见 [`.omp/agents/build-verifier.md`](.omp/agents/build-verifier.md)。

多文件改动完成后做终验时：派只读子代理（禁止编辑/写入/删除），**先 Read 该文件并套用其全文**作为子代理系统提示；本适配器不复制正文。Cursor 无 omp 专属 agent 运行时，不要伪造。
