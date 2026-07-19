// Tests for the manifest-select dialog's pure logic layer.
//
// Source: lib/src/models/manifest_selection.dart. Pure functions only — no
// Flutter widget pumping, no rinf FFI required (ManifestItemDto/
// ManifestVariantDto/GroupItemEntry are plain bincode-serializable classes,
// constructible without native init).

import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/bindings/bindings.dart';
import 'package:flux_down/src/models/download_task.dart';
import 'package:flux_down/src/models/manifest_selection.dart';

ManifestItemDto _item(
  String id,
  String path, {
  int size = 100,
  List<ManifestVariantDto> variants = const [],
}) {
  return ManifestItemDto(
    id: id,
    name: path.split('/').last,
    path: path,
    size: size,
    variants: variants,
  );
}

ManifestVariantDto _variant(String id, String label, {int size = 100}) =>
    ManifestVariantDto(id: id, label: label, size: size);

void main() {
  group('buildManifestTree', () {
    test('顶层混合目录与文件：目录排前、各自按名排序', () {
      final items = [
        _item('m', 'movie.mkv'),
        _item('f2', 'Extras/featurette.mkv'),
        _item('s', 'Extras/sample.mkv'),
      ];
      final roots = buildManifestTree(items);
      expect(roots, hasLength(2));
      expect(roots[0], isA<ManifestDirNode>());
      expect((roots[0] as ManifestDirNode).name, 'Extras');
      expect((roots[0] as ManifestDirNode).children, hasLength(2));
      expect((roots[0] as ManifestDirNode).children.map((c) => c.name), [
        'featurette.mkv',
        'sample.mkv',
      ]);
      expect(roots[1], isA<ManifestFileNode>());
      expect(roots[1].name, 'movie.mkv');
    });

    test('单链目录折叠：a/b/c 无分叉无文件时合并为一行', () {
      final items = [_item('f', 'a/b/c/file.mkv')];
      final roots = buildManifestTree(items);
      expect(roots, hasLength(1));
      final dir = roots.single as ManifestDirNode;
      expect(dir.name, 'a/b/c');
      expect(dir.path, 'a/b/c');
      expect(dir.children, hasLength(1));
      expect(dir.children.single.name, 'file.mkv');
    });

    test('分叉目录不折叠：出现多子目录时链条中断', () {
      final items = [
        _item('x', 'a/b/x.mkv'),
        _item('y', 'a/b/y.mkv'),
        _item('z', 'a/other/z.mkv'),
      ];
      final roots = buildManifestTree(items);
      expect(roots, hasLength(1));
      final a = roots.single as ManifestDirNode;
      expect(a.name, 'a'); // 'a' 本身有 2 个子目录，不与它们合并
      expect(a.children.map((c) => c.name), ['b', 'other']);
      final b = a.children[0] as ManifestDirNode;
      expect(b.children, hasLength(2));
    });

    test('同级出现文件时链条中断（即使只有一个子目录）', () {
      final items = [
        _item('a', 'root/sibling.mkv'),
        _item('b', 'root/nested/deep.mkv'),
      ];
      final roots = buildManifestTree(items);
      final root = roots.single as ManifestDirNode;
      expect(root.name, 'root'); // root 下有 sibling.mkv 文件，不与 nested 合并
      expect(root.children.map((c) => c.name), ['nested', 'sibling.mkv']);
    });
  });

  group('flattenManifestTree — 缩进封顶 + 灰色父目录前缀', () {
    // 手工搭一条 7 层深的链（每层用真实节点，绕开 buildManifestTree 的单链
    // 折叠，专测封顶与前缀计算本身）。
    ManifestFileNode leaf(int depth) => ManifestFileNode(
      item: _item('leaf', 'a/b/c/d/e/f/file.mkv'),
      path: 'a/b/c/d/e/f/file.mkv',
      depth: depth,
    );
    ManifestDirNode chain() {
      final f = ManifestDirNode(
        name: 'f',
        path: 'a/b/c/d/e/f',
        depth: 5,
        children: [leaf(6)],
      );
      final e = ManifestDirNode(
        name: 'e',
        path: 'a/b/c/d/e',
        depth: 4,
        children: [f],
      );
      final d = ManifestDirNode(
        name: 'd',
        path: 'a/b/c/d',
        depth: 3,
        children: [e],
      );
      final c = ManifestDirNode(
        name: 'c',
        path: 'a/b/c',
        depth: 2,
        children: [d],
      );
      final b = ManifestDirNode(
        name: 'b',
        path: 'a/b',
        depth: 1,
        children: [c],
      );
      return ManifestDirNode(name: 'a', path: 'a', depth: 0, children: [b]);
    }

    test('深度 ≤ 封顶级数：缩进随深度递增，无灰色前缀', () {
      final rows = flattenManifestTree([chain()], {});
      // a(0) b(1) c(2) d(3) e(4) f(5,capped=4) file(6,capped=4)
      expect(rows.map((r) => r.node.name).toList(), [
        'a',
        'b',
        'c',
        'd',
        'e',
        'f',
        'file.mkv',
      ]);
      expect(rows.map((r) => r.indent).toList(), [0, 1, 2, 3, 4, 4, 4]);
    });

    test('深度 > 封顶级数：缩进封顶在 4，附带 "…/父目录/" 灰色前缀', () {
      final rows = flattenManifestTree([chain()], {});
      final f = rows.firstWhere((r) => r.node.name == 'f');
      final file = rows.firstWhere((r) => r.node.name == 'file.mkv');
      final e = rows.firstWhere((r) => r.node.name == 'e');
      expect(e.greyPrefix, ''); // depth 4 == cap，未超限
      expect(f.greyPrefix, '…/e/'); // depth 5 > cap，父目录名 = e
      expect(file.greyPrefix, '…/f/'); // depth 6 > cap，父目录名 = f
    });

    test('折叠的目录不展开子树，但自身仍占一行', () {
      final root = chain();
      final rows = flattenManifestTree([root], {'a'});
      expect(rows, hasLength(1));
      expect(rows.single.node.name, 'a');
    });
  });

  group('三态目录勾选', () {
    late ManifestDirNode dir;
    setUp(() {
      dir = ManifestDirNode(
        name: 'd',
        path: 'd',
        depth: 0,
        children: [
          ManifestFileNode(
            item: _item('1', 'd/1.mkv'),
            path: 'd/1.mkv',
            depth: 1,
          ),
          ManifestFileNode(
            item: _item('2', 'd/2.mkv'),
            path: 'd/2.mkv',
            depth: 1,
          ),
          ManifestFileNode(
            item: _item('3', 'd/3.mkv'),
            path: 'd/3.mkv',
            depth: 1,
          ),
        ],
      );
    });

    test('全未选中 → unchecked；全选中 → checked；部分选中 → indeterminate', () {
      expect(manifestDirCheckState(dir, {}), ManifestCheckState.unchecked);
      expect(
        manifestDirCheckState(dir, {'1', '2', '3'}),
        ManifestCheckState.checked,
      );
      expect(
        manifestDirCheckState(dir, {'1'}),
        ManifestCheckState.indeterminate,
      );
    });

    test('勾选目录 = 递归选中全部叶子；取消勾选 = 递归清空', () {
      final selected = toggleManifestDirSelection(dir, {}, true);
      expect(selected, {'1', '2', '3'});
      final unselected = toggleManifestDirSelection(dir, {
        '1',
        '2',
        '3',
        'other',
      }, false);
      expect(unselected, {'other'});
    });

    test('collectManifestItemIds 收集整棵子树的叶子 id', () {
      expect(collectManifestItemIds(dir), {'1', '2', '3'});
    });

    test('manifestNodeTotalSize 聚合子树 Σsize', () {
      expect(manifestNodeTotalSize(dir), 300); // 3 个文件各 size:100（默认值）
    });
  });

  group('扩展名筛选 / 意图聚合', () {
    final items = [
      _item('v1', 'a.mkv', size: 1000),
      _item('v2', 'b.mp4', size: 2000),
      _item('a1', 'c.mp3', size: 300),
      _item('d1', 'd.pdf', size: 50),
    ];

    test('filterManifestItemsByCategory 按类型过滤，all 不过滤', () {
      expect(filterManifestItemsByCategory(items, FileCategory.all), items);
      expect(
        filterManifestItemsByCategory(
          items,
          FileCategory.video,
        ).map((i) => i.id),
        ['v1', 'v2'],
      );
    });

    test('aggregateManifestByCategory 计数 + Σsize，仅含实际出现的类型', () {
      final agg = aggregateManifestByCategory(items);
      expect(agg.map((a) => a.category), isNot(contains(FileCategory.all)));
      final video = agg.firstWhere((a) => a.category == FileCategory.video);
      expect(video.count, 2);
      expect(video.totalSize, 3000);
      expect(video.itemIds, {'v1', 'v2'});
      expect(agg.any((a) => a.category == FileCategory.image), isFalse);
    });

    test('全选 / 反选', () {
      final all = allManifestItemIds(items);
      expect(all, {'v1', 'v2', 'a1', 'd1'});
      final inverted = invertManifestSelection(items, {'v1', 'v2'});
      expect(inverted, {'a1', 'd1'});
    });

    test('manifestTotalSize / manifestSelectedSize', () {
      expect(manifestTotalSize(items), 3350);
      expect(manifestSelectedSize(items, {'v1', 'a1'}), 1300);
    });
  });

  group('规格策略：最高 / 1080P / 720P / 最省 + 精确档回退计数', () {
    late List<ManifestItemDto> items;
    setUp(() {
      items = [
        _item(
          'exact',
          'ep1.mkv',
          variants: [
            _variant('v2160', '2160p'),
            _variant('v1080', '1080p'),
            _variant('v480', '480p'),
          ],
        ),
        _item(
          'noexact',
          'ep2.mkv',
          variants: [_variant('v2160b', '2160p'), _variant('v480b', '480p')],
        ),
        _item('single', 'readme.txt'), // 无 variants，不受策略影响
      ];
    });

    test('highest 选分辨率最大的 variant', () {
      final result = applyManifestQualityPolicy(
        items,
        ManifestQualityPolicy.highest,
      );
      final map = result.asMap;
      expect(map['exact'], 'v2160');
      expect(map['noexact'], 'v2160b');
      expect(map['single'], isNull);
      expect(result.fallbackCount, 0); // highest 没有"精确档"概念
    });

    test('lowest 选分辨率最小的 variant', () {
      final result = applyManifestQualityPolicy(
        items,
        ManifestQualityPolicy.lowest,
      );
      final map = result.asMap;
      expect(map['exact'], 'v480');
      expect(map['noexact'], 'v480b');
    });

    test('p1080 精确命中时不回退；未命中时选最接近的并计入回退', () {
      final result = applyManifestQualityPolicy(
        items,
        ManifestQualityPolicy.p1080,
      );
      final map = result.asMap;
      expect(map['exact'], 'v1080'); // 精确命中
      // noexact 只有 2160/480，距 1080 最近的是 2160（差 1080）vs 480（差 600）→ 480 更近
      expect(map['noexact'], 'v480b');
      expect(result.fallbackCount, 1);
      final exactChoice = result.choices.firstWhere((c) => c.itemId == 'exact');
      final noexactChoice = result.choices.firstWhere(
        (c) => c.itemId == 'noexact',
      );
      expect(exactChoice.isFallback, isFalse);
      expect(noexactChoice.isFallback, isTrue);
    });

    test('p720 两个条目都没有精确档，都回退且计数为 2', () {
      final result = applyManifestQualityPolicy(
        items,
        ManifestQualityPolicy.p720,
      );
      expect(result.fallbackCount, 2);
    });

    test('resolveEffectiveManifestVariants：per-item 覆盖优先于策略基准', () {
      final policyResult = applyManifestQualityPolicy(
        items,
        ManifestQualityPolicy.highest,
      );
      final effective = resolveEffectiveManifestVariants(policyResult, {
        'exact': 'v480', // 手动覆盖成最低画质
      });
      expect(effective['exact'], 'v480');
      expect(effective['noexact'], 'v2160b'); // 未覆盖，沿用策略基准
    });
  });

  group('resolver_item 拼接', () {
    test('无 variantId（null 或空串）只用 itemId；否则 itemId@variantId', () {
      expect(buildManifestResolverItem('item1', null), 'item1');
      expect(buildManifestResolverItem('item1', ''), 'item1');
      expect(buildManifestResolverItem('item1', 'v2'), 'item1@v2');
    });

    test('buildManifestGroupItems：命中 variant 时 size 取 variant.size', () {
      final items = [
        _item(
          'i1',
          'a/ep1.mkv',
          size: 999,
          variants: [_variant('v1', '1080p', size: 500)],
        ),
        _item('i2', 'b/ep2.mkv', size: 300),
      ];
      final entries = buildManifestGroupItems(
        items,
        {'i1', 'i2'},
        {'i1': 'v1', 'i2': null},
      );
      expect(entries, hasLength(2));
      final e1 = entries.firstWhere((e) => e.resolverItem == 'i1@v1');
      expect(e1.size, 500);
      expect(e1.relPath, 'a/ep1.mkv');
      final e2 = entries.firstWhere((e) => e.resolverItem == 'i2');
      expect(e2.size, 300);
    });

    test('buildManifestGroupItems 只包含选中集合内的条目', () {
      final items = [_item('i1', 'x.mkv'), _item('i2', 'y.mkv')];
      final entries = buildManifestGroupItems(
        items,
        {'i1'},
        {'i1': null, 'i2': null},
      );
      expect(entries, hasLength(1));
      expect(entries.single.resolverItem, 'i1');
    });
  });

  group('组名默认值', () {
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
  });

  group('剧集智能建议启发式', () {
    test('高置信度编号模式 → 正片 + 匹配字幕建议', () {
      final items = [
        _item('e1', 'Show S01E01.mkv'),
        _item('e2', 'Show S01E02.mkv'),
        _item('e3', 'Show S01E03.mkv'),
        _item('s1', 'Show S01E01.srt'),
        _item('nfo', 'Show.nfo'),
      ];
      final suggestion = detectManifestEpisodeSuggestion(items);
      expect(suggestion, isNotNull);
      expect(suggestion!.itemIds, {'e1', 'e2', 'e3', 's1'});
      expect(suggestion.count, 4);
    });

    test('视频条目不足 2 个 → 置信度不足，返回 null', () {
      final items = [_item('e1', 'Show S01E01.mkv')];
      expect(detectManifestEpisodeSuggestion(items), isNull);
    });

    test('命中比例过低（< 60%）→ 返回 null', () {
      final items = [
        _item('e1', 'Show S01E01.mkv'),
        _item('e2', 'random_name.mkv'),
        _item('e3', 'another_clip.mkv'),
      ];
      expect(detectManifestEpisodeSuggestion(items), isNull);
    });

    test('排除花絮/预告后无剩余正片 → 返回 null', () {
      final items = [
        _item('t1', 'Trailer 01.mkv'),
        _item('t2', 'Trailer 02.mkv'),
      ];
      expect(detectManifestEpisodeSuggestion(items), isNull);
    });
  });
}
