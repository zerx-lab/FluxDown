import 'dart:io';

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

/// 将旧版便携数据（≤ v0.2.x，散落在 exe 目录根层）迁移到新的
/// `portable_data/` 子目录。
///
/// 幂等：目标已存在则跳过。
void _migratePortableData(String exeDir, String newDir) {
  Directory(newDir).createSync(recursive: true);
  // KEEP IN SYNC with native/engine/src/data_dir.rs KNOWN_ITEMS
  const knownItems = [
    'flux_down.db',
    'flux_down.db-wal',
    'flux_down.db-shm',
    'settings.json',
    'logs',
    'bt_session',
    'plugins',
    'plugins-work',
    'bin',
  ];
  for (final name in knownItems) {
    final oldPath = '$exeDir${Platform.pathSeparator}$name';
    final newPath = '$newDir${Platform.pathSeparator}$name';
    final oldType = FileSystemEntity.typeSync(oldPath);
    if (oldType == FileSystemEntityType.notFound) continue;
    if (FileSystemEntity.typeSync(newPath) != FileSystemEntityType.notFound) continue;
    try {
      if (oldType == FileSystemEntityType.directory) {
        Directory(oldPath).renameSync(newPath);
      } else {
        File(oldPath).renameSync(newPath);
      }
    } catch (e) {
      // logger 未就绪 + log_service 反向依赖本文件，故用 stderr 而非 logError。
      stderr.writeln('[便携迁移] 移动失败 $oldPath → $newPath: $e');
    }
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
    _migratePortableData(exeDir, newDir);
    return newDir;
  }
  // Windows installed mode
  final localAppData = Platform.environment['LOCALAPPDATA'] ??
      Platform.environment['APPDATA'] ??
      File(Platform.resolvedExecutable).parent.path;
  return '$localAppData${Platform.pathSeparator}FluxDown';
}
