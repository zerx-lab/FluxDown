// Tests for the manifest-select dialog's pure logic layer (v1.6 下钻导航版).
//
// Source: lib/src/models/manifest_selection.dart. Pure functions only — no
// Flutter widget pumping, no rinf FFI required (ManifestItemDto/
// GroupItemEntry are plain bincode-serializable classes, constructible
// without native init).
//
// 测试数据结构性子集移植自 design/desktop-task-views/manifest.js 的
// mockManifest：8 级深链（单链合并跳级）、每级带文件的分叉深层目录、
// 根级散件（大小未知/超长名）。

import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/bindings/bindings.dart';
import 'package:flux_down/src/models/manifest_breadcrumb.dart';
import 'package:flux_down/src/models/manifest_selection.dart';

/// 取路径的父目录段（模拟 items 里 path 是"相对子目录"字段，不含文件名）。
ManifestItemDto _fileAt(String id, String dirPath, String fileName, {int size = 100}) {
  return ManifestItemDto(
    id: id,
    name: fileName,
    path: dirPath,
    size: size,
    variants: const [],
  );
}

void main() {
  group('manifestItemVisible / manifestIsSearching', () {
    test('扩展名筛选：命中大写扩展名才可见，空筛选不过滤', () {
      final mkv = _fileAt('1', '', 'a.mkv');
      final srt = _fileAt('2', '', 'b.srt');
      expect(manifestItemVisible(mkv, extFilter: {}, search: ''), isTrue);
      expect(manifestItemVisible(mkv, extFilter: {'MKV'}, search: ''), isTrue);
      expect(manifestItemVisible(srt, extFilter: {'MKV'}, search: ''), isFalse);
    });

    test('搜索词大小写不敏感匹配文件名', () {
      final it = _fileAt('1', '', 'Show.S01E01.mkv');
      expect(manifestItemVisible(it, extFilter: {}, search: 'show'), isTrue);
      expect(manifestItemVisible(it, extFilter: {}, search: 'nomatch'), isFalse);
    });

    test('manifestIsSearching 对空白词判定为非搜索态', () {
      expect(manifestIsSearching(''), isFalse);
      expect(manifestIsSearching('   '), isFalse);
      expect(manifestIsSearching(' a '), isTrue);
    });
  });

  group('manifestTopExtensions — 频次 top7', () {
    test('按出现频次取前 N，计数相同按扩展名排序保证确定性', () {
      final items = [
        for (var i = 0; i < 5; i++) _fileAt('mkv$i', '', 'v$i.mkv'),
        for (var i = 0; i < 3; i++) _fileAt('srt$i', '', 's$i.srt'),
        _fileAt('nfo', '', 'x.nfo'),
      ];
      final top = manifestTopExtensions(items, limit: 2);
      expect(top, hasLength(2));
      expect(top[0].ext, 'MKV');
      expect(top[0].count, 5);
      expect(top[1].ext, 'SRT');
      expect(top[1].count, 3);
    });

    test('无扩展名文件归入 FILE', () {
      final items = [_fileAt('1', '', 'README')];
      final top = manifestTopExtensions(items);
      expect(top.single.ext, 'FILE');
    });
  });

  group('manifestRowsAt — 8 级深链单链合并 + 跳级进入/返回', () {
    // 对齐 mockManifest 的 "制作资料/原盘结构样例/BDMV/STREAM(/CLIPINF/META/DL)"：
    // 无中间文件的深链应合并为一行，进入一次跳到链尾；根级另有一个散件文件。
    final items = [
      _fileAt('m2ts', '制作资料/原盘结构样例/BDMV/STREAM', '00055.m2ts'),
      _fileAt(
        'clpi',
        '制作资料/原盘结构样例/BDMV/STREAM/CLIPINF/META/DL',
        '00001.clpi',
      ),
      _fileAt('root_nfo', '', 'readme.nfo'),
    ];

    test('根层：深链合并为一个目录行 + 一个根级文件行', () {
      final result = manifestRowsAt(
        items: items,
        cwd: '',
        selectedItemIds: {},
        extFilter: {},
        search: '',
        sortKey: ManifestSortKey.name,
      );
      expect(result.cwd, '');
      expect(result.rows, hasLength(2));
      final dirEntry = result.rows.whereType<ManifestDirRowEntry>().single;
      expect(dirEntry.row.labels, ['制作资料', '原盘结构样例', 'BDMV', 'STREAM']);
      expect(dirEntry.row.path, '制作资料/原盘结构样例/BDMV/STREAM');
      expect(dirEntry.row.count, 2); // m2ts + 深层 clpi 都在这条链子树下
      final fileEntry = result.rows.whereType<ManifestFileRowEntry>().single;
      expect(fileEntry.row.item.id, 'root_nfo');
    });

    test('进入合并行一次跳级到链尾，链尾还能再合并出下一段单链', () {
      final level1 = manifestRowsAt(
        items: items,
        cwd: '制作资料/原盘结构样例/BDMV/STREAM',
        selectedItemIds: {},
        extFilter: {},
        search: '',
        sortKey: ManifestSortKey.name,
      );
      expect(level1.cwd, '制作资料/原盘结构样例/BDMV/STREAM');
      // 该层：00055.m2ts 直属文件 + CLIPINF/META/DL 单链合并的子目录行
      expect(level1.rows, hasLength(2));
      final dirEntry = level1.rows.whereType<ManifestDirRowEntry>().single;
      expect(dirEntry.row.labels, ['CLIPINF', 'META', 'DL']);
      expect(
        dirEntry.row.path,
        '制作资料/原盘结构样例/BDMV/STREAM/CLIPINF/META/DL',
      );
    });

    test('返回上级：STREAM 自身有直属文件（00055.m2ts），是"实层"——只跳到这里，不再跳过', () {
      final up = manifestUpPath(
        items: items,
        cwd: '制作资料/原盘结构样例/BDMV/STREAM/CLIPINF/META/DL',
        extFilter: {},
      );
      // CLIPINF/META 均无直属文件、只有单个子目录（纯过渡层）→ 一路跳过；
      // STREAM 有直属文件 00055.m2ts → 是「实层」，停在这里。
      expect(up, '制作资料/原盘结构样例/BDMV/STREAM');
    });

    test('返回上级：全链均为纯过渡层（无任何直属文件）时一次跳回根', () {
      final pureChainItems = [
        _fileAt('leaf', 'a/b/c', 'leaf.mkv'),
      ];
      final up = manifestUpPath(
        items: pureChainItems,
        cwd: 'a/b/c',
        extFilter: {},
      );
      expect(up, '');
    });
  });

  group('manifestRowsAt — 每级带文件的分叉目录不合并', () {
    // 对齐 mockManifest 的 "制作资料/字幕工程(/分轨(/EP01(/中文)))"：字幕工程
    // 每一级都有直属文件，链条在每一步都中断，不发生单链合并；额外在
    // "制作资料" 下放一个旁支（其他/），确保制作资料本身也不会被并入
    // 字幕工程（否则「制作资料只有一个子目录」的合并条件会先于本组要
    // 验证的现象触发，掩盖测试意图）。
    final items = [
      _fileAt('a', '制作资料/字幕工程', '说明.txt'),
      _fileAt('b', '制作资料/字幕工程/分轨', '命名规范.txt'),
      _fileAt('c', '制作资料/字幕工程/分轨/EP01', '打轴笔记.txt'),
      _fileAt('d', '制作资料/字幕工程/分轨/EP01/中文', '样式表.ass'),
      _fileAt('e', '制作资料/其他', 'sibling.txt'),
    ];

    test('制作资料下有 2 个子目录（字幕工程/其他），不发生单链合并', () {
      final result = manifestRowsAt(
        items: items,
        cwd: '',
        selectedItemIds: {},
        extFilter: {},
        search: '',
        sortKey: ManifestSortKey.name,
      );
      final dirEntry = result.rows.whereType<ManifestDirRowEntry>().single;
      expect(dirEntry.row.labels, ['制作资料']);
      expect(dirEntry.row.path, '制作资料');
      expect(dirEntry.row.count, 5);
    });

    test('制作资料层：字幕工程/其他各自有直属文件，均不与下级合并', () {
      final level1 = manifestRowsAt(
        items: items,
        cwd: '制作资料',
        selectedItemIds: {},
        extFilter: {},
        search: '',
        sortKey: ManifestSortKey.name,
      );
      final dirs = level1.rows.whereType<ManifestDirRowEntry>().toList();
      expect(dirs.map((d) => d.row.labels), [
        ['其他'],
        ['字幕工程'],
      ]);
    });

    test('逐层进入都能看到该层直属文件 + 未合并的下一级目录行', () {
      final level1 = manifestRowsAt(
        items: items,
        cwd: '制作资料/字幕工程',
        selectedItemIds: {},
        extFilter: {},
        search: '',
        sortKey: ManifestSortKey.name,
      );
      expect(level1.rows, hasLength(2));
      final dir = level1.rows.whereType<ManifestDirRowEntry>().single;
      expect(dir.row.labels, ['分轨']); // 分轨自身也有文件，不会继续合并
      final file = level1.rows.whereType<ManifestFileRowEntry>().single;
      expect(file.row.item.id, 'a');
    });
  });

  group('manifestRowsAt — 根级散件', () {
    test('path 为空串的条目直接落在根层文件行', () {
      final items = [_fileAt('a', '', 'x.nfo'), _fileAt('b', '', 'y.txt')];
      final result = manifestRowsAt(
        items: items,
        cwd: '',
        selectedItemIds: {},
        extFilter: {},
        search: '',
        sortKey: ManifestSortKey.name,
      );
      expect(result.rows, hasLength(2));
      expect(result.rows.every((r) => r is ManifestFileRowEntry), isTrue);
    });
  });

  group('大小未知（size==0）参与统计', () {
    test('manifestSelectionStat：未知项计入 unknownCount，不计入 size', () {
      final items = [
        _fileAt('a', '', 'x.mkv', size: 1000),
        _fileAt('b', '', 'y.nfo', size: 0), // 未知
      ];
      final stat = manifestSelectionStat(items, {'a', 'b'});
      expect(stat.count, 2);
      expect(stat.size, 1000); // ≈ 前缀由 UI 层依据 unknownCount>0 决定是否加
      expect(stat.unknownCount, 1);
    });

    test('目录行统计（+ 后缀语义）：子树含未知项时 unknown=true，size 只累计已知项', () {
      final items = [
        _fileAt('a', 'd', 'x.mkv', size: 1000),
        _fileAt('b', 'd', 'y.nfo', size: 0),
      ];
      final result = manifestRowsAt(
        items: items,
        cwd: '',
        selectedItemIds: {'a', 'b'},
        extFilter: {},
        search: '',
        sortKey: ManifestSortKey.name,
      );
      final dir = result.rows.whereType<ManifestDirRowEntry>().single.row;
      expect(dir.count, 2);
      expect(dir.size, 1000);
      expect(dir.unknown, isTrue);
      expect(dir.selCnt, 2);
    });

    test('文件排序 size 键：未知（size==0）排到末尾', () {
      final items = [
        _fileAt('a', '', 'unknown.nfo', size: 0),
        _fileAt('b', '', 'big.mkv', size: 2000),
        _fileAt('c', '', 'small.mkv', size: 500),
      ];
      final result = manifestRowsAt(
        items: items,
        cwd: '',
        selectedItemIds: {},
        extFilter: {},
        search: '',
        sortKey: ManifestSortKey.size,
      );
      final ids = result.rows
          .whereType<ManifestFileRowEntry>()
          .map((r) => r.row.item.id)
          .toList();
      expect(ids, ['b', 'c', 'a']);
    });
  });

  group('筛选后 cwd 回退根', () {
    test('扩展名筛选把当前层筛空后，rowsAt 落回根并返回新 cwd', () {
      final items = [
        _fileAt('a', 'dirA', 'x.mkv'),
        _fileAt('b', '', 'root.srt'),
      ];
      final result = manifestRowsAt(
        items: items,
        cwd: 'dirA',
        selectedItemIds: {},
        extFilter: {'SRT'}, // 只留 srt，dirA 整层被筛空
        search: '',
        sortKey: ManifestSortKey.name,
      );
      expect(result.cwd, ''); // 回退根
      expect(result.rows, hasLength(1));
      expect(
        (result.rows.single as ManifestFileRowEntry).row.item.id,
        'b',
      );
    });

    test('cwd 本身路径在可见树里不存在（如已被删除的深层路径）同样回退根', () {
      final items = [_fileAt('a', '', 'root.mkv')];
      final result = manifestRowsAt(
        items: items,
        cwd: 'no/such/path',
        selectedItemIds: {},
        extFilter: {},
        search: '',
        sortKey: ManifestSortKey.name,
      );
      expect(result.cwd, '');
    });
  });

  group('搜索扁平模式行流', () {
    test('搜索态返回跨层级扁平结果，文件行 showPath=true', () {
      final items = [
        _fileAt('a', '深层/路径', 'Show.S01E01.mkv'),
        _fileAt('b', '', 'Show.S01E02.mkv'),
        _fileAt('c', '', 'other.txt'),
      ];
      final result = manifestRowsAt(
        items: items,
        cwd: '深层/路径', // 搜索态下 cwd 被忽略
        selectedItemIds: {},
        extFilter: {},
        search: 'show',
        sortKey: ManifestSortKey.name,
      );
      expect(result.rows, hasLength(2));
      expect(result.rows.every((r) => r is ManifestFileRowEntry), isTrue);
      for (final r in result.rows) {
        expect((r as ManifestFileRowEntry).row.showPath, isTrue);
      }
    });

    test('搜索结果计数（面包屑用）与可见集一致', () {
      final items = [
        _fileAt('a', '', 'match1.mkv'),
        _fileAt('b', '', 'match2.mkv'),
        _fileAt('c', '', 'other.txt'),
      ];
      final crumb = buildManifestBreadcrumb(
        items: items,
        cwd: '',
        extFilter: {},
        search: 'match',
      );
      expect(crumb.searching, isTrue);
      expect(crumb.searchResultCount, 2);
      expect(crumb.segments, isEmpty);
      expect(crumb.showUp, isFalse);
    });
  });

  group('面包屑折叠模型', () {
    test('≤4 段：全部平铺展示，无 ellipsis，overflow 为空', () {
      final crumb = buildManifestBreadcrumb(
        items: const [],
        cwd: 'a/b/c',
        extFilter: {},
        search: '',
      );
      expect(crumb.searching, isFalse);
      expect(crumb.showUp, isTrue);
      expect(crumb.segments.map((s) => s.kind), [
        ManifestCrumbKind.home,
        ManifestCrumbKind.segment,
        ManifestCrumbKind.segment,
        ManifestCrumbKind.segment,
      ]);
      expect(crumb.segments.map((s) => s.label).skip(1), ['a', 'b', 'c']);
      expect(crumb.segments.last.isLast, isTrue);
      expect(crumb.overflowSegments, isEmpty);
    });

    test('>4 段：折叠为 home / 首段 / ⋯ / 末两段，中间段进 overflow', () {
      final crumb = buildManifestBreadcrumb(
        items: const [],
        cwd: 'a/b/c/d/e/f',
        extFilter: {},
        search: '',
      );
      expect(crumb.segments.map((s) => s.kind), [
        ManifestCrumbKind.home,
        ManifestCrumbKind.segment, // a
        ManifestCrumbKind.ellipsis,
        ManifestCrumbKind.segment, // e
        ManifestCrumbKind.segment, // f
      ]);
      expect(crumb.segments[1].label, 'a');
      expect(crumb.segments[3].label, 'e');
      expect(crumb.segments[4].label, 'f');
      expect(crumb.segments[4].isLast, isTrue);
      // 隐藏中段：b/c/d
      expect(crumb.overflowSegments.map((s) => s.label), ['b', 'c', 'd']);
      expect(crumb.overflowSegments.map((s) => s.path), [
        'a/b',
        'a/b/c',
        'a/b/c/d',
      ]);
    });

    test('根目录：home 为 isLast，showUp=false', () {
      final crumb = buildManifestBreadcrumb(
        items: const [],
        cwd: '',
        extFilter: {},
        search: '',
      );
      expect(crumb.segments, hasLength(1));
      expect(crumb.segments.single.kind, ManifestCrumbKind.home);
      expect(crumb.segments.single.isLast, isTrue);
      expect(crumb.showUp, isFalse);
    });
  });

  group('全选 / 反选 / 清空 —— 作用域=全部可见文件', () {
    final items = [
      _fileAt('a', '', 'a.mkv'),
      _fileAt('b', '', 'b.srt'),
      _fileAt('c', '', 'c.mkv'),
    ];

    test('全选：替换为当前可见集合（筛选范围外的旧选中被丢弃）', () {
      final selected = manifestSelectAllVisible(
        items,
        extFilter: {'MKV'},
        search: '',
      );
      expect(selected, {'a', 'c'});
    });

    test('反选：可见集合内「此前未选」的条目，可见范围外旧选中同样被丢弃', () {
      final inverted = manifestInvertVisibleSelection(
        items,
        {'a', 'b'}, // b 在筛选范围外（不是 mkv）
        extFilter: {'MKV'},
        search: '',
      );
      expect(inverted, {'c'}); // a 已选被移除；b 因不可见被丢弃；c 未选变已选
    });

    test('清空：调用方直接置空集合（不需要模型函数）', () {
      final cleared = <String>{};
      expect(cleared, isEmpty);
    });
  });

  group('目录三态推导', () {
    test('由行统计推导 unchecked / checked / indeterminate', () {
      const none = ManifestDirRow(
        path: 'd',
        labels: ['d'],
        count: 3,
        size: 300,
        selCnt: 0,
        unknown: false,
      );
      const all = ManifestDirRow(
        path: 'd',
        labels: ['d'],
        count: 3,
        size: 300,
        selCnt: 3,
        unknown: false,
      );
      const partial = ManifestDirRow(
        path: 'd',
        labels: ['d'],
        count: 3,
        size: 300,
        selCnt: 1,
        unknown: false,
      );
      expect(manifestDirRowCheckState(none), ManifestCheckState.unchecked);
      expect(manifestDirRowCheckState(all), ManifestCheckState.checked);
      expect(manifestDirRowCheckState(partial), ManifestCheckState.indeterminate);
    });

    test('manifestToggleDirSubtree：整树全选中则整体取消，否则整体选中', () {
      final items = [
        _fileAt('1', 'd', '1.mkv'),
        _fileAt('2', 'd', '2.mkv'),
        _fileAt('3', 'd/sub', '3.mkv'),
      ];
      final selected = manifestToggleDirSubtree(
        items: items,
        dirPath: 'd',
        selectedItemIds: {},
        extFilter: {},
        search: '',
      );
      expect(selected, {'1', '2', '3'});
      final unselected = manifestToggleDirSubtree(
        items: items,
        dirPath: 'd',
        selectedItemIds: {'1', '2', '3', 'other'},
        extFilter: {},
        search: '',
      );
      expect(unselected, {'other'});
    });

    test('manifestDirFileIds 只收集可见条目', () {
      final items = [
        _fileAt('1', 'd', '1.mkv'),
        _fileAt('2', 'd', '2.srt'),
      ];
      final ids = manifestDirFileIds(
        items: items,
        dirPath: 'd',
        extFilter: {'MKV'},
        search: '',
      );
      expect(ids, {'1'});
    });
  });

  group('resolver_item / CreateTaskGroup.items 投影', () {
    test('buildManifestGroupItems：resolver_item 恒为 itemId（无 @variant 后缀）', () {
      final items = [
        _fileAt('i1', 'a', 'ep1.mkv', size: 999),
        _fileAt('i2', 'b', 'ep2.mkv', size: 300),
      ];
      final entries = buildManifestGroupItems(items, {'i1', 'i2'});
      expect(entries, hasLength(2));
      final e1 = entries.firstWhere((e) => e.resolverItem == 'i1');
      expect(e1.fileName, 'ep1.mkv');
      expect(e1.relPath, 'a');
      expect(e1.size, 999);
    });

    test('只包含选中集合内的条目', () {
      final items = [_fileAt('i1', '', 'x.mkv'), _fileAt('i2', '', 'y.mkv')];
      final entries = buildManifestGroupItems(items, {'i1'});
      expect(entries, hasLength(1));
      expect(entries.single.resolverItem, 'i1');
    });
  });

  group('组名默认值 / 来源站点', () {
    test('manifest.name 非空时直接使用（trim）', () {
      expect(manifestDefaultGroupName('  My Show  ', 'https://x/y'), 'My Show');
    });

    test('manifest.name 为空时退化用来源 URL 最后一段', () {
      expect(
        manifestDefaultGroupName('', 'https://example.com/share/MyFolder'),
        'MyFolder',
      );
    });

    test('都拿不到时返回空串', () {
      expect(manifestDefaultGroupName('', ''), '');
    });

    test('manifestSourceHost 解析域名，失败返回空串', () {
      expect(manifestSourceHost('https://pan.baidu.com/s/1abc'), 'pan.baidu.com');
      expect(manifestSourceHost('not a url'), '');
    });
  });

  group('高级选项 dirty 判定 / 请求头生效表', () {
    ManifestAdvancedOptions defaults() => const ManifestAdvancedOptions(
      proxyUrl: '',
      ignoreTlsErrors: false,
      uaInherit: true,
      userAgent: '',
      cookies: '',
      segments: 0,
      headers: [],
    );

    test('默认值不 dirty', () {
      expect(manifestAdvancedOptionsDirty(defaults()), isFalse);
    });

    test('任一字段偏离默认即 dirty', () {
      expect(
        manifestAdvancedOptionsDirty(
          ManifestAdvancedOptions(
            proxyUrl: 'socks5://127.0.0.1:1080',
            ignoreTlsErrors: defaults().ignoreTlsErrors,
            uaInherit: defaults().uaInherit,
            userAgent: defaults().userAgent,
            cookies: defaults().cookies,
            segments: defaults().segments,
            headers: defaults().headers,
          ),
        ),
        isTrue,
      );
      expect(
        manifestAdvancedOptionsDirty(
          ManifestAdvancedOptions(
            proxyUrl: '',
            ignoreTlsErrors: false,
            uaInherit: false, // 切自定义但输入框为空，不算 dirty
            userAgent: '',
            cookies: '',
            segments: 0,
            headers: const [],
          ),
        ),
        isFalse,
      );
      expect(
        manifestAdvancedOptionsDirty(
          ManifestAdvancedOptions(
            proxyUrl: '',
            ignoreTlsErrors: false,
            uaInherit: true,
            userAgent: '',
            cookies: '',
            segments: 4,
            headers: const [],
          ),
        ),
        isTrue,
      );
    });

    test('manifestEffectiveHeaders 丢弃空 key/value 行，同名后者覆盖前者', () {
      final result = manifestEffectiveHeaders(const [
        ManifestHeaderEntry(key: 'Referer', value: 'https://a'),
        ManifestHeaderEntry(key: '', value: 'ignored'),
        ManifestHeaderEntry(key: 'X-Empty', value: ''),
        ManifestHeaderEntry(key: 'Referer', value: 'https://b'),
      ]);
      expect(result, {'Referer': 'https://b'});
    });
  });
}
