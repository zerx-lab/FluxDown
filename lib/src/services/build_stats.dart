// FluxDown 构建期统计信息。
//
// Release CI（.github/workflows/release.yml）在 `flutter build` 时经
// `--dart-define` 从 git 历史动态注入以下三个值：
//   STATS_FIRST_COMMIT_DATE  首次 commit 日期（YYYY-MM-DD）
//   STATS_RELEASE_COUNT      版本 tag（v*）总数
//   STATS_COMMIT_COUNT       commit 总数
//
// 未注入时（本地开发构建）使用下方兜底默认值，保证无网络也能展示。
// 默认值来自仓库 git 历史快照，可随发版顺手更新，无需精确。

/// 首次 commit 日期（ISO `YYYY-MM-DD`）。
const String statsFirstCommitDate = String.fromEnvironment(
  'STATS_FIRST_COMMIT_DATE',
  defaultValue: '2026-02-09',
);

/// 累计发布版本数（`v*` tag 数）。
const int statsReleaseCount = int.fromEnvironment(
  'STATS_RELEASE_COUNT',
  defaultValue: 59,
);

/// 累计代码提交次数。
const int statsCommitCount = int.fromEnvironment(
  'STATS_COMMIT_COUNT',
  defaultValue: 426,
);
