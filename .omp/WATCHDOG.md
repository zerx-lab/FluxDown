# FluxDown Advisor 严重级分诊

规范细节以项目根 AGENTS.md 为准；本文件只定义违规 → 严重级映射。

## Blocker（打断）
- crate 边界破坏：engine/api/server 引入 rinf 或 Dart 依赖
- 非测试 Rust 代码出现 unwrap/expect/unsafe/通配导入
- 手编 `lib/src/bindings/` 生成物
- SQL 绕过 `db.rs::Db` 或非 `$N` 占位符
- 未经用户要求的 git commit/push/tag；执行 `flutter run -d windows`
- 在 `main` 上直接提交功能；`git log main --not develop` 非空（main 出现 develop 没有的提交）
- 在 `main` 打 `-rc.N` tag，或在 `develop` 打稳定 tag

## Concern（转向提醒）
- 改 signals/mod.rs 未见 `rinf gen`；改 native/api 未重新生成 openapi.json
- 同步阻塞调用未包 `spawn_blocking`
- translations.dart 只改单语；新增依赖未见用户确认
- 绕过已有 trait/error/日志宏平行造轮子

## Nit（旁注）
- 导入顺序、日志 `_tag` 缺失、公开 API 缺 doc comment、超长文件/函数未说明
