// 回归测试：浏览器扩展右键下载未按文件分类归入对应保存目录。
//
// 根因：SettingsProvider.resolveCategorySaveDir(fileName, {url}) 依赖 fileName
// 做扩展名匹配；但扩展右键下载等场景常常只有 URL、没有已解析的文件名（或文件名
// 不含扩展名），导致分类规则永远命不中，全部落到默认目录。修复方案：当
// fileName 为空或不含 '.' 时，改用 URL 路径末段（新增私有方法
// SettingsProvider._fileNameFromUrl）派生文件名参与匹配。
//
// == 可测面评估（本文件为何走"纯逻辑镜像"路径而非直接调用 SettingsProvider）==
//
// 已用探针实测确认：在 `flutter test`（无 Rinf/Rust 引擎）环境下，
// SettingsProvider 无法安全构造或注入：
//   1) 构造函数本身会调用 _syncAutoStartupState()，其内部经
//      launch_at_startup 的 LaunchAtStartup.isEnabled() 落到
//      AppAutoLauncherImplNoop（该包只有显式调用 setup() 才会切到真实平台实
//      现，测试环境从未调用），noop 实现直接抛 UnsupportedError，属于构造期
//      异步未捕获异常，会使任意实例化都失败。
//   2) 即便绕过 (1)，任何会落盘的 setter（如 setCustomCategories）都会调用
//      _saveToRust → SaveConfig(...).sendSignalToRust() → rinf 尝试
//      dlopen 原生库 `hub.dll`，测试环境未编译/链接 Rust 引擎，
//      直接抛 "Failed to load dynamic library 'hub.dll'"。
//   3) _customCategories 是私有字段，唯一不经过 sendSignalToRust 的写入路径是
//      _onConfigLoaded(RustSignalPack<ConfigLoaded>)，需要构造一个真实的
//      Rinf 信号包（含底层 bincode/FFI 元数据），在纯 Dart 单测里不可行。
//
// 结论：resolveCategorySaveDir 本身是纯函数（不触发任何 _saveToRust 调用），
// 但托管它的实例不可安全构造。因此本文件转而覆盖"同等语义"的可达逻辑：
//   A. CustomCategory.matches —— 100% 真实生产代码，直接调用，验证扩展名/
//      正则匹配、大小写、all/other 内置类型的匹配契约。
//   B. URL → 文件名派生契约 —— _fileNameFromUrl 是 private static 方法，
//      测试文件无法直接调用；这里按其文档化契约（源码doc注释，
//      settings_provider.dart 中"从 URL 中提取文件名（取最后一段路径，须含
//      '.'），失败返回 ''"）在测试文件内镜像出等价算法 _deriveFileNameFromUrl，
//      逐条钉住其边界行为（含一个通过实测发现的真实细节：Uri.pathSegments 已
//      对合法百分号编码做过一次解码，再调用 Uri.decodeComponent 二次解码非
//      ASCII 文本会抛异常，被外层 try/catch 吞掉、返回 ''）。
//   C. resolveCategorySaveDir 的分类选择编排 —— 同样因实例不可构造而无法直接
//      调用，这里用 _resolveSaveDir 镜像其纯编排逻辑（复用 A 的真实
//      CustomCategory.matches 与 B 的派生函数），驱动验收标准 (a)-(e) 的
//      五个端到端场景。若未来该函数的分类选择顺序/回退规则发生变化而
//      _resolveSaveDir 未同步更新，这组测试无法感知——这是环境限制下的已知
//      取舍，已在此处明确记录。

import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/models/custom_category.dart';

/// 镜像 SettingsProvider._fileNameFromUrl（settings_provider.dart 约
/// 255-265 行）的文档化契约：取 URL 路径最后一段，URL 解码后若含 '.' 则
/// 返回，否则（含解析异常）返回 ''。
String _deriveFileNameFromUrl(String url) {
  try {
    final uri = Uri.parse(url.trim());
    final segments = uri.pathSegments;
    if (segments.isNotEmpty) {
      final last = Uri.decodeComponent(segments.last);
      if (last.contains('.')) return last;
    }
  } catch (_) {}
  return '';
}

/// 镜像 SettingsProvider.resolveCategorySaveDir（settings_provider.dart 约
/// 227-252 行）的纯编排逻辑：[categories] 对应真实实现里已完成
/// 可见性过滤+position 排序的 visibleCategories。
String _resolveSaveDir(
  String fileName,
  List<CustomCategory> categories, {
  String url = '',
}) {
  var name = fileName;
  if ((name.isEmpty || !name.contains('.')) && url.isNotEmpty) {
    final derived = _deriveFileNameFromUrl(url);
    if (derived.isNotEmpty) name = derived;
  }
  if (name.isEmpty) return '';
  final normals = categories
      .where((c) => c.builtinType != 'all' && c.builtinType != 'other')
      .toList();
  for (final cat in normals) {
    if (cat.saveDir.isNotEmpty && cat.matches(name)) {
      return cat.saveDir;
    }
  }
  final otherCat = categories
      .where((c) => c.builtinType == 'other')
      .firstOrNull;
  if (otherCat != null &&
      otherCat.saveDir.isNotEmpty &&
      !normals.any((c) => c.matches(name))) {
    return otherCat.saveDir;
  }
  return '';
}

void main() {
  group('CustomCategory.matches — 真实生产代码，验证 URL 派生文件名的匹配契约', () {
    final archive = CustomCategory.defaultCategories().firstWhere(
      (c) => c.builtinType == 'archive',
    );
    final video = CustomCategory.defaultCategories().firstWhere(
      (c) => c.builtinType == 'video',
    );
    final other = CustomCategory.defaultCategories().firstWhere(
      (c) => c.builtinType == 'other',
    );
    final all = CustomCategory.defaultCategories().firstWhere(
      (c) => c.builtinType == 'all',
    );

    test('压缩包分类命中 URL 派生的 .zip / .tar.gz 文件名', () {
      expect(archive.matches('file.zip'), isTrue);
      expect(archive.matches('a/b/c.tar.gz'.split('/').last), isTrue);
    });

    test('扩展名匹配忽略大小写', () {
      expect(archive.matches('FILE.ZIP'), isTrue);
    });

    test('视频分类命中 mp4，但压缩包分类不命中同一文件名', () {
      expect(video.matches('movie.mp4'), isTrue);
      expect(archive.matches('movie.mp4'), isFalse);
    });

    test('无扩展名或以点结尾的文件名不命中任何扩展名分类', () {
      expect(archive.matches('noext'), isFalse);
      expect(archive.matches('trailingdot.'), isFalse);
      expect(archive.matches(''), isFalse);
    });

    test("builtinType == 'other' 永不通过 matches 命中（由调用方专门处理排除逻辑）", () {
      expect(other.matches('anything.zip'), isFalse);
      expect(other.matches(''), isFalse);
    });

    test("builtinType == 'all' 命中任意文件名", () {
      expect(all.matches('movie.mp4'), isTrue);
      expect(all.matches('noext'), isTrue);
      expect(all.matches(''), isTrue);
    });

    test('正则模式：自定义分类按 regexPattern 匹配文件名', () {
      const screenshot = CustomCategory(
        id: 'c1',
        name: 'screenshots',
        matchMode: MatchMode.regex,
        regexPattern: r'^screenshot.*\.png$',
        saveDir: '/tmp/screenshots',
      );
      expect(screenshot.matches('screenshot_2024.png'), isTrue);
      expect(screenshot.matches('SCREENSHOT_final.png'), isTrue); // 大小写不敏感
      expect(screenshot.matches('photo.png'), isFalse);
    });
  });

  group('URL → 文件名派生契约（镜像 _fileNameFromUrl 文档化算法）', () {
    test('取路径末段作为文件名', () {
      expect(
        _deriveFileNameFromUrl('https://cdn.example.com/files/report.pdf'),
        'report.pdf',
      );
    });

    test('忽略 query string', () {
      expect(
        _deriveFileNameFromUrl(
          'https://cdn.example.com/movie.mp4?token=abc&x=1',
        ),
        'movie.mp4',
      );
    });

    test('对合法百分号编码做 URL 解码（ASCII，如空格）', () {
      expect(
        _deriveFileNameFromUrl('https://cdn.example.com/space%20name.iso'),
        'space name.iso',
      );
    });

    test('末段含多个点时保留完整文件名（如 .tar.gz）', () {
      expect(
        _deriveFileNameFromUrl('https://cdn.example.com/a/b/c.tar.gz'),
        'c.tar.gz',
      );
    });

    test('末段不含 "." 时返回空（无法判断扩展名）', () {
      expect(_deriveFileNameFromUrl('https://cdn.example.com/download'), '');
    });

    test('无路径段（根路径或裸域名）时返回空', () {
      expect(_deriveFileNameFromUrl('https://cdn.example.com/'), '');
      expect(_deriveFileNameFromUrl('https://cdn.example.com'), '');
    });

    test('URL 无法解析时返回空（不抛异常）', () {
      expect(_deriveFileNameFromUrl('not a valid url ::: %%%'), '');
    });

    test(
      '末段含非 ASCII 百分号编码文件名时返回空（Uri.pathSegments 已解码一次，'
      '再次 decodeComponent 抛异常并被吞掉——真实实现的既有边界行为）',
      () {
        expect(
          _deriveFileNameFromUrl(
            'https://cdn.example.com/%E6%96%87%E4%BB%B6.zip',
          ),
          '',
        );
      },
    );
  });

  group('resolveCategorySaveDir 语义镜像 —— 验收场景 (a)-(e)', () {
    List<CustomCategory> categoriesWithSaveDirs({
      String archiveDir = '',
      String videoDir = '',
      String documentDir = '',
      String otherDir = '',
    }) {
      return CustomCategory.defaultCategories().map((c) {
        switch (c.builtinType) {
          case 'archive':
            return c.copyWith(saveDir: archiveDir);
          case 'video':
            return c.copyWith(saveDir: videoDir);
          case 'document':
            return c.copyWith(saveDir: documentDir);
          case 'other':
            return c.copyWith(saveDir: otherDir);
          default:
            return c;
        }
      }).toList();
    }

    test('(a) fileName 为空 + url 带 .zip → 命中压缩包分类 saveDir', () {
      final categories = categoriesWithSaveDirs(archiveDir: '/downloads/zip');
      final result = _resolveSaveDir(
        '',
        categories,
        url: 'https://cdn.example.com/pack.zip',
      );
      expect(result, '/downloads/zip');
    });

    test('(b) fileName 无扩展名 + url 带 .mp4 → 命中视频分类 saveDir', () {
      final categories = categoriesWithSaveDirs(videoDir: '/downloads/video');
      final result = _resolveSaveDir(
        'blob', // 扩展下载常见的无扩展名占位文件名
        categories,
        url: 'https://cdn.example.com/movie.mp4',
      );
      expect(result, '/downloads/video');
    });

    test('(c) fileName 已带扩展名时以 fileName 为准，忽略 url', () {
      final categories = categoriesWithSaveDirs(
        videoDir: '/downloads/video',
        documentDir: '/downloads/doc',
      );
      // url 会派生出 movie.mp4（命中视频分类），但 fileName 本身已含扩展名
      // .docx（命中文档分类），必须以 fileName 为准。
      final result = _resolveSaveDir(
        'report.docx',
        categories,
        url: 'https://cdn.example.com/movie.mp4',
      );
      expect(result, '/downloads/doc');
    });

    test(
      '(c2) fileName 带扩展名但不命中任何分类时，即使 url 能命中也不回退到 url',
      () {
        final categories = categoriesWithSaveDirs(videoDir: '/downloads/video');
        final result = _resolveSaveDir(
          'data.xyz', // 未知扩展名，不命中任何内置分类
          categories,
          url: 'https://cdn.example.com/movie.mp4',
        );
        expect(result, ''); // 不应回退到 video，因为 fileName 已含 '.'
      },
    );

    test('(d) 均不命中普通分类 + other 未设置 saveDir → 返回空字符串', () {
      final categories = categoriesWithSaveDirs(); // 全部 saveDir 为空
      final result = _resolveSaveDir(
        '',
        categories,
        url: 'https://cdn.example.com/data.xyz',
      );
      expect(result, '');
    });

    test('(e) 不命中普通分类，但 other 设置了 saveDir → 回退到 other', () {
      final categories = categoriesWithSaveDirs(otherDir: '/downloads/misc');
      final result = _resolveSaveDir(
        '',
        categories,
        url: 'https://cdn.example.com/data.xyz', // 派生出 data.xyz，不命中任何普通分类
      );
      expect(result, '/downloads/misc');
    });

    test('普通分类命中但未配置 saveDir 时，不会误回退到 other（即便 other 已配置）', () {
      // video 分类命中了 movie.mp4，但 video.saveDir 为空；由于"已命中但未配置
      // 目录"与"完全未命中"是不同语义，真实实现不会把它算作"未命中"去用
      // other 兜底。
      final categories = categoriesWithSaveDirs(otherDir: '/downloads/misc');
      final result = _resolveSaveDir(
        '',
        categories,
        url: 'https://cdn.example.com/movie.mp4',
      );
      expect(result, '');
    });

    test('fileName 与 url 都为空时直接返回空字符串', () {
      final categories = categoriesWithSaveDirs(otherDir: '/downloads/misc');
      expect(_resolveSaveDir('', categories), '');
    });
  });
}
