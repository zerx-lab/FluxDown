import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/models/custom_category.dart';
import 'package:flux_down/src/models/download_task.dart';

void main() {
  group('FileCategory.fromExtension', () {
    test('程序包扩展名归入 program 分类', () {
      for (final ext in [
        'exe', 'msi', 'msix', 'appx', 'apk', 'dmg', 'pkg',
        'deb', 'rpm', 'appimage', 'snap', 'flatpak', 'EXE', 'Dmg',
      ]) {
        expect(
          FileCategory.fromExtension(ext),
          FileCategory.program,
          reason: ext,
        );
      }
    });

    test('dmg 不再归入 archive；zip/iso 仍归 archive', () {
      expect(FileCategory.fromExtension('zip'), FileCategory.archive);
      expect(FileCategory.fromExtension('iso'), FileCategory.archive);
      expect(FileCategory.fromExtension('dmg'), isNot(FileCategory.archive));
    });
  });

  group('CustomCategory.defaultCategories', () {
    test('内置 program 分类位于 archive 之前且匹配安装包', () {
      final defaults = CustomCategory.defaultCategories();
      final program = defaults.firstWhere((c) => c.builtinType == 'program');
      final archive = defaults.firstWhere((c) => c.builtinType == 'archive');
      expect(program.position, lessThan(archive.position));
      expect(program.isBuiltin, isTrue);
      expect(program.matches('FluxDown-0.1.58-windows-x64-setup.exe'), isTrue);
      expect(program.matches('FluxDown-0.1.58-macos-arm64.dmg'), isTrue);
      expect(program.matches('fluxdown_0.1.58_amd64.deb'), isTrue);
      expect(program.matches('photo.png'), isFalse);
      expect(archive.extensions, isNot(contains('dmg')));
    });
  });
}
