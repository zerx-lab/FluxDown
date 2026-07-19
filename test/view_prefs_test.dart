// Tests for ViewPrefs + ViewPrefsStore (lib/src/models/view_prefs.dart).
//
// Covers: default snapshot, copyWith/toJson/fromJson round trip + schema
// tolerance, isDefault, and the per-tab overlay persistence semantics
// (unmodified tabs fall back to factory defaults, never to another tab's
// last edit — design-proto-spec.md §1 `loadPrefs`).

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/models/view_prefs.dart';
import 'package:flux_down/src/services/kv_store.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  group('ViewPrefs', () {
    test('defaults match the current app behavior snapshot', () {
      final d = ViewPrefs.defaults();
      expect(d.form, ViewForm.list);
      expect(d.density, ViewDensity.comfortable);
      expect(d.groupBy, ViewGroupBy.smart);
      expect(d.sortKey, ViewSortKey.smart);
      expect(d.sortDir, SortDir.desc);
      expect(d.showCompleted, isTrue);
      expect(d.protocolBadges, isTrue);
      expect(d.columns, {
        TaskColumnId.progress,
        TaskColumnId.speed,
        TaskColumnId.eta,
        TaskColumnId.status,
      });
      expect(d.isDefault, isTrue);
    });

    test('copyWith only changes requested fields', () {
      final d = ViewPrefs.defaults();
      final next = d.copyWith(form: ViewForm.grid, sortDir: SortDir.asc);
      expect(next.form, ViewForm.grid);
      expect(next.sortDir, SortDir.asc);
      expect(next.density, d.density);
      expect(next.groupBy, d.groupBy);
      expect(next.isDefault, isFalse);
    });

    test('toJson/fromJson round trip preserves all fields', () {
      final original = ViewPrefs.defaults().copyWith(
        form: ViewForm.grid,
        density: ViewDensity.compact,
        groupBy: ViewGroupBy.site,
        sortKey: ViewSortKey.size,
        sortDir: SortDir.asc,
        showCompleted: false,
        protocolBadges: false,
        columns: {TaskColumnId.size, TaskColumnId.status},
      );
      final restored = ViewPrefs.fromJson(original.toJson());
      expect(restored, original);
    });

    test('fromJson tolerates missing/malformed fields by falling back to defaults', () {
      final restored = ViewPrefs.fromJson({
        'form': 'grid',
        'sortDir': 'not-a-real-value',
        'showCompleted': 'nope', // wrong type
        'columns': 'not-a-list', // wrong type
      });
      final d = ViewPrefs.defaults();
      expect(restored.form, ViewForm.grid); // valid field applied
      expect(restored.sortDir, d.sortDir); // invalid enum -> fallback
      expect(restored.showCompleted, d.showCompleted); // wrong type -> fallback
      expect(restored.columns, d.columns); // wrong type -> fallback
      expect(restored.density, d.density); // missing -> fallback
    });

    test('== / hashCode ignore Set iteration order', () {
      const a = ViewPrefs(
        form: ViewForm.list,
        density: ViewDensity.comfortable,
        groupBy: ViewGroupBy.smart,
        sortKey: ViewSortKey.smart,
        sortDir: SortDir.desc,
        showCompleted: true,
        protocolBadges: true,
        columns: {TaskColumnId.progress, TaskColumnId.speed},
      );
      const b = ViewPrefs(
        form: ViewForm.list,
        density: ViewDensity.comfortable,
        groupBy: ViewGroupBy.smart,
        sortKey: ViewSortKey.smart,
        sortDir: SortDir.desc,
        showCompleted: true,
        protocolBadges: true,
        columns: {TaskColumnId.speed, TaskColumnId.progress},
      );
      expect(a, b);
      expect(a.hashCode, b.hashCode);
    });
  });

  group('ViewPrefsStore', () {
    late Directory dir;
    late File file;

    setUp(() {
      KvStore.instance.debugReset();
      dir = Directory.systemTemp.createTempSync('view_prefs_store_test');
      file = File('${dir.path}/settings.json');
      KvStore.instance.debugInitPortable(file);
    });

    tearDown(() {
      KvStore.instance.debugReset();
      if (dir.existsSync()) dir.deleteSync(recursive: true);
    });

    test('unmodified tab resolves to factory defaults', () {
      final store = ViewPrefsStore();
      expect(store.resolve('all'), ViewPrefs.defaults());
      expect(store.isDefault('all'), isTrue);
      expect(store.hasOverride('all'), isFalse);
    });

    test('update() only affects the target tab, never other tabs', () {
      final store = ViewPrefsStore();
      store.update(
        'completed',
        (prefs) => prefs.copyWith(groupBy: ViewGroupBy.type, form: ViewForm.grid),
      );

      expect(store.hasOverride('completed'), isTrue);
      expect(store.resolve('completed').groupBy, ViewGroupBy.type);
      expect(store.resolve('completed').form, ViewForm.grid);
      expect(store.isDefault('completed'), isFalse);

      // 「downloading」页签从未被改动过 —— 必须仍是出厂默认，绝不继承
      // 「completed」页签刚做的改动（design-proto-spec §1 加粗强调的语义）。
      expect(store.hasOverride('downloading'), isFalse);
      expect(store.resolve('downloading'), ViewPrefs.defaults());
      expect(store.isDefault('downloading'), isTrue);
    });

    test('reset() clears only the target tab override', () {
      final store = ViewPrefsStore();
      store.update('error', (prefs) => prefs.copyWith(sortKey: ViewSortKey.name));
      store.update('paused', (prefs) => prefs.copyWith(sortKey: ViewSortKey.size));

      store.reset('error');

      expect(store.hasOverride('error'), isFalse);
      expect(store.resolve('error'), ViewPrefs.defaults());
      // 「paused」页签的覆盖层不受影响。
      expect(store.hasOverride('paused'), isTrue);
      expect(store.resolve('paused').sortKey, ViewSortKey.size);
    });

    test('overrides persist across store instances via KvStore', () {
      final store1 = ViewPrefsStore();
      store1.update(
        'all',
        (prefs) => prefs.copyWith(
          density: ViewDensity.compact,
          columns: {TaskColumnId.size, TaskColumnId.status},
        ),
      );

      // 模拟应用重启：新实例从同一份 KvStore 缓存重新加载。
      final store2 = ViewPrefsStore();
      final resolved = store2.resolve('all');
      expect(resolved.density, ViewDensity.compact);
      expect(resolved.columns, {TaskColumnId.size, TaskColumnId.status});
    });

    test('notifies listeners on update and reset', () {
      final store = ViewPrefsStore();
      var notifications = 0;
      store.addListener(() => notifications++);

      store.update('all', (prefs) => prefs.copyWith(form: ViewForm.grid));
      expect(notifications, 1);

      store.reset('all');
      expect(notifications, 2);

      // 重置一个从未覆盖过的页签是 no-op，不应重复通知。
      store.reset('all');
      expect(notifications, 2);
    });
  });
}
