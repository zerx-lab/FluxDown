import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/services/platform_utils.dart';

/// 便携数据迁移（旧根层布局 → portable_data/）行为测试。
/// 与 native/engine/src/data_dir.rs 的 tests 覆盖同一组语义。
void main() {
  late Directory root;
  late String exeDir;
  late String newDir;
  final sep = Platform.pathSeparator;

  setUp(() {
    root = Directory.systemTemp.createTempSync('fluxdown_portable_migrate_');
    exeDir = root.path;
    newDir = '$exeDir${sep}portable_data';
  });

  tearDown(() {
    try {
      root.deleteSync(recursive: true);
    } catch (_) {}
  });

  void write(String relative, String content) {
    final file = File('$exeDir$sep$relative');
    file.parent.createSync(recursive: true);
    file.writeAsStringSync(content);
  }

  String read(String path) => File(path).readAsStringSync();

  test('首迁：DB 三件套 + 清单条目全部移入，非数据文件原地保留', () {
    write('flux_down.db', 'db');
    write('flux_down.db-wal', 'wal');
    write('flux_down.db-shm', 'shm');
    write('settings.json', '{}');
    write('icons${sep}custom_icon.ico', 'ico');
    write('logs${sep}fluxdown_2026-01-01.log', 'log');
    write('flux_down.exe', 'bin');

    migratePortableData(exeDir, newDir);

    expect(read('$newDir${sep}flux_down.db'), 'db');
    expect(read('$newDir${sep}flux_down.db-wal'), 'wal');
    expect(read('$newDir${sep}flux_down.db-shm'), 'shm');
    expect(File('$newDir${sep}icons${sep}custom_icon.ico').existsSync(), isTrue);
    expect(
      File('$newDir${sep}logs${sep}fluxdown_2026-01-01.log').existsSync(),
      isTrue,
    );
    expect(File('$exeDir${sep}flux_down.db').existsSync(), isFalse);
    expect(Directory('$exeDir${sep}icons').existsSync(), isFalse);
    // exe 根层的非数据文件不在清单内，必须原地保留。
    expect(File('$exeDir${sep}flux_down.exe').existsSync(), isTrue);
  });

  test('幂等：第二次运行为 no-op', () {
    write('flux_down.db', 'db');
    write('settings.json', '{}');
    migratePortableData(exeDir, newDir);
    migratePortableData(exeDir, newDir);
    expect(read('$newDir${sep}flux_down.db'), 'db');
  });

  test('目标已存在：新数据保留，旧文件原地不动', () {
    write('settings.json', 'old');
    write('portable_data${sep}settings.json', 'new');
    migratePortableData(exeDir, newDir);
    expect(read('$newDir${sep}settings.json'), 'new');
    expect(read('$exeDir${sep}settings.json'), 'old');
  });

  test('孤儿 WAL 绝不单独搬运', () {
    write('flux_down.db-wal', 'wal');
    migratePortableData(exeDir, newDir);
    expect(File('$exeDir${sep}flux_down.db-wal').existsSync(), isTrue);
    expect(File('$newDir${sep}flux_down.db-wal').existsSync(), isFalse);
  });

  test('新主库已存在：旧三件套整组原地保留，不混搬 WAL', () {
    write('flux_down.db', 'old-db');
    write('flux_down.db-wal', 'old-wal');
    write('portable_data${sep}flux_down.db', 'new-db');
    migratePortableData(exeDir, newDir);
    expect(read('$newDir${sep}flux_down.db'), 'new-db');
    expect(File('$exeDir${sep}flux_down.db').existsSync(), isTrue);
    expect(File('$exeDir${sep}flux_down.db-wal').existsSync(), isTrue);
    expect(File('$newDir${sep}flux_down.db-wal').existsSync(), isFalse);
  });
}
