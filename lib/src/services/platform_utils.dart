import 'dart:io';

import 'package:meta/meta.dart';

/// Marker file name — a zero-byte file placed next to the exe by the portable
/// ZIP distribution.  Matches the Rust-side `PORTABLE_MARKER` constant.
const portableMarker = 'portable';

/// 便携数据子目录名，位于 exe 所在目录内。
const _portableDataDir = 'portable_data';

/// Whether the current Windows installation is portable mode.
///
/// Portable mode is detected by the presence of a `portable` marker file
/// next to the executable.  On non-Windows platforms this always returns false.
bool isPortableMode() {
  if (!Platform.isWindows) return false;
  try {
    final exeDir = File(Platform.resolvedExecutable).parent.path;
    return File('$exeDir${Platform.pathSeparator}$portableMarker').existsSync();
  } catch (_) {
    return false;
  }
}

/// SQLite 主库文件名；`-wal` / `-shm` 为其伴生文件（见 `_migrateDbGroup`）。
const _dbFile = 'flux_down.db';
const _dbWal = 'flux_down.db-wal';
const _dbShm = 'flux_down.db-shm';

/// 独立迁移项（不含 DB 三件套——那组走 `_migrateDbGroup` 原子迁移）。
// KEEP IN SYNC with native/engine/src/data_dir.rs KNOWN_ITEMS
const _knownItems = [
  'settings.json',
  'logs',
  'icons',
  'bt_session',
  'plugins',
  'plugins-work',
  'bin',
];

/// 迁移是否已在本进程内执行过（短路重复的路径解析调用，避免每次
/// [resolveDataDir] 都做同步文件系统探测）。
bool _portableMigrationDone = false;

/// 将旧版便携数据（≤ v0.2.x，散落在 exe 目录根层）迁移到新的
/// `portable_data/` 子目录。与 Rust 侧 `migrate_portable_layout` 语义一致：
///
/// - 幂等：目标已存在则跳过；
/// - DB 三件套作为原子组迁移（WAL 持有未 checkpoint 的事务，必须与主库
///   同进退，WAL 失败时回滚主库）；
/// - 失败条目原地保留并落盘 `<newDir>/migration_errors.log`（GUI 进程
///   stderr 不可见，且 LogService 尚未就绪——它反向依赖本文件解析日志目录），
///   下次启动自动重试。
@visibleForTesting
void migratePortableData(String exeDir, String newDir) {
  final failures = <String>[];
  try {
    Directory(newDir).createSync(recursive: true);
  } catch (e) {
    stderr.writeln('[便携迁移] 创建目录失败 $newDir: $e');
    return;
  }
  _migrateDbGroup(exeDir, newDir, failures);
  for (final name in _knownItems) {
    final oldPath = '$exeDir${Platform.pathSeparator}$name';
    final newPath = '$newDir${Platform.pathSeparator}$name';
    if (_exists(oldPath) && !_exists(newPath)) {
      _rename(oldPath, newPath, failures);
    }
  }
  if (failures.isEmpty) return;
  for (final msg in failures) {
    stderr.writeln('[便携迁移] $msg');
  }
  _persistFailures(newDir, failures);
}

bool _exists(String path) =>
    FileSystemEntity.typeSync(path) != FileSystemEntityType.notFound;

/// rename 单个文件/目录；失败时记入 [failures] 并返回 false。
bool _rename(String oldPath, String newPath, List<String> failures) {
  try {
    if (FileSystemEntity.typeSync(oldPath) == FileSystemEntityType.directory) {
      Directory(oldPath).renameSync(newPath);
    } else {
      File(oldPath).renameSync(newPath);
    }
    return true;
  } catch (e) {
    failures.add('移动失败 $oldPath → $newPath: $e');
    return false;
  }
}

/// SQLite 三件套（主库 / WAL / SHM）原子组迁移。
///
/// - 旧主库不存在（含孤儿 WAL）或新主库已存在 → 整组跳过，绝不单独搬 WAL；
/// - 主库 rename 失败 → 整组放弃；
/// - WAL rename 失败 → 已移动的主库回滚回原位，下次启动重试整组；
/// - SHM 是共享内存索引，SQLite 会按需重建——失败仅记录、不回滚。
void _migrateDbGroup(String exeDir, String newDir, List<String> failures) {
  final sep = Platform.pathSeparator;
  final oldDb = '$exeDir$sep$_dbFile';
  final newDb = '$newDir$sep$_dbFile';
  if (!_exists(oldDb) || _exists(newDb)) return;
  if (!_rename(oldDb, newDb, failures)) return;
  final oldWal = '$exeDir$sep$_dbWal';
  if (_exists(oldWal) && !_rename(oldWal, '$newDir$sep$_dbWal', failures)) {
    // WAL 搬不动 → 主库回滚，保持三件套同处一地。
    _rename(newDb, oldDb, failures);
    return;
  }
  final oldShm = '$exeDir$sep$_dbShm';
  final newShm = '$newDir$sep$_dbShm';
  if (_exists(oldShm) && !_exists(newShm)) {
    _rename(oldShm, newShm, failures);
  }
}

/// 迁移失败信息落盘：`<newDir>/migration_errors.log`（追加）。
///
/// 放数据目录根层而非 `logs/`——迁移失败时若在此处预创建 `logs/` 目录，
/// 会让下次启动误判 `logs` 已迁移而永久跳过它。
void _persistFailures(String newDir, List<String> failures) {
  try {
    final ts = DateTime.now().toIso8601String();
    File('$newDir${Platform.pathSeparator}migration_errors.log').writeAsStringSync(
      failures.map((m) => '$ts [便携迁移] $m\n').join(),
      mode: FileMode.append,
      flush: true,
    );
  } catch (_) {
    // 尽力而为：失败记录本身失败时已无处可写。
  }
}

/// Resolve the application data directory.
///
/// | Platform        | Mode      | Directory                                      |
/// |-----------------|-----------|-------------------------------------------------|
/// | Windows         | Portable  | `<exe_dir>/portable_data/`                      |
/// | Windows         | Installed | `%LOCALAPPDATA%\FluxDown\`                      |
/// | Linux           | —         | `$XDG_DATA_HOME/fluxdown/`                      |
/// | macOS           | —         | `~/Library/Application Support/fluxdown/`        |
String resolveDataDir() {
  if (Platform.isAndroid) {
    return '/data/data/com.fluxdown.app/files/fluxdown';
  }
  if (Platform.isLinux) {
    final xdgData = Platform.environment['XDG_DATA_HOME'] ??
        '${Platform.environment['HOME']}/.local/share';
    return '$xdgData/fluxdown';
  }
  if (Platform.isMacOS) {
    final home = Platform.environment['HOME'] ?? '';
    return '$home/Library/Application Support/fluxdown';
  }
  // Windows
  if (Platform.isWindows && isPortableMode()) {
    final exeDir = File(Platform.resolvedExecutable).parent.path;
    final newDir = '$exeDir${Platform.pathSeparator}$_portableDataDir';
    if (!_portableMigrationDone) {
      _portableMigrationDone = true;
      migratePortableData(exeDir, newDir);
    }
    return newDir;
  }
  // Windows installed mode
  final localAppData = Platform.environment['LOCALAPPDATA'] ??
      Platform.environment['APPDATA'] ??
      File(Platform.resolvedExecutable).parent.path;
  return '$localAppData${Platform.pathSeparator}FluxDown';
}
