// Tests for the task column registry's pure logic:
// - width budget guard (tryToggleColumn)
// - canonical column order independence from toggle order
// - compact-density progress -> size column mapping (effectiveColumns)
//
// Source: lib/src/widgets/task_columns.dart

import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/i18n/i18n_store.dart';
import 'package:flux_down/src/i18n/translations.dart';
import 'package:flux_down/src/models/view_prefs.dart';
import 'package:flux_down/src/widgets/task_columns.dart';

void main() {
  // 列切换拒绝提示需要读 i18n 表；en.json 作为源语言表恒可用。
  final s = S.of(I18nStore.resolve('en'));

  group('column canonical order', () {
    test('kColumnCanonicalOrder matches design-proto-spec COL_ORDER', () {
      expect(kColumnCanonicalOrder, [
        TaskColumnId.progress,
        TaskColumnId.size,
        TaskColumnId.created,
        TaskColumnId.protocol,
        TaskColumnId.source,
        TaskColumnId.queue,
        TaskColumnId.speed,
        TaskColumnId.eta,
        TaskColumnId.status,
      ]);
    });

    test('every column def width matches the spec table', () {
      expect(kTaskColumns[TaskColumnId.progress]!.width, 150);
      expect(kTaskColumns[TaskColumnId.size]!.width, 80);
      expect(kTaskColumns[TaskColumnId.created]!.width, 104);
      expect(kTaskColumns[TaskColumnId.protocol]!.width, 64);
      expect(kTaskColumns[TaskColumnId.source]!.width, 148);
      expect(kTaskColumns[TaskColumnId.queue]!.width, 88);
      expect(kTaskColumns[TaskColumnId.speed]!.width, 90);
      expect(kTaskColumns[TaskColumnId.eta]!.width, 80);
      expect(kTaskColumns[TaskColumnId.status]!.width, 60);
    });
  });

  group('columnWidthBudget', () {
    test('default column set (progress+speed+eta+status = 150+90+80+60=380) '
        'fits comfortably within a typical list width', () {
      // 现状硬编码列宽总和（升级零感知的锚点）。
      final defaultWidth = columnsTotalWidth(ViewPrefs.defaultColumns);
      expect(defaultWidth, 150 + 90 + 80 + 60);
      expect(columnWidthBudget(800), 800 - 168);
      expect(defaultWidth, lessThan(columnWidthBudget(800)));
    });
  });

  group('tryToggleColumn', () {
    test('unchecking the last remaining column is rejected', () {
      final result = tryToggleColumn(
        current: {TaskColumnId.status},
        toggling: TaskColumnId.status,
        listWidth: 1000,
        s: s,
      );
      expect(result, s.viewColumnsAtLeastOne);
    });

    test('unchecking when >1 column remains is accepted (null)', () {
      final result = tryToggleColumn(
        current: {TaskColumnId.status, TaskColumnId.speed},
        toggling: TaskColumnId.status,
        listWidth: 1000,
        s: s,
      );
      expect(result, isNull);
    });

    test('adding a column that exceeds the width budget is rejected', () {
      // listWidth 200 -> budget = 200-168 = 32px，任何新增列都会超预算。
      final result = tryToggleColumn(
        current: {TaskColumnId.status},
        toggling: TaskColumnId.speed,
        listWidth: 200,
        s: s,
      );
      expect(result, s.viewColumnsBudgetExceeded);
    });

    test('adding a column within budget is accepted (null)', () {
      final result = tryToggleColumn(
        current: {TaskColumnId.status},
        toggling: TaskColumnId.speed,
        listWidth: 1000,
        s: s,
      );
      expect(result, isNull);
    });
  });

  group('effectiveColumns (compact density progress -> size mapping)', () {
    test('comfortable density returns columns unchanged, canonical order', () {
      final prefs = ViewPrefs.defaults(); // comfortable, columns={progress,speed,eta,status}
      expect(effectiveColumns(prefs), [
        TaskColumnId.progress,
        TaskColumnId.speed,
        TaskColumnId.eta,
        TaskColumnId.status,
      ]);
    });

    test('compact density maps progress -> size', () {
      final prefs = ViewPrefs.defaults().copyWith(density: ViewDensity.compact);
      expect(effectiveColumns(prefs), [
        TaskColumnId.size,
        TaskColumnId.speed,
        TaskColumnId.eta,
        TaskColumnId.status,
      ]);
    });

    test('compact density de-duplicates when both progress and size are checked', () {
      final prefs = ViewPrefs.defaults().copyWith(
        density: ViewDensity.compact,
        columns: {TaskColumnId.progress, TaskColumnId.size, TaskColumnId.speed},
      );
      final result = effectiveColumns(prefs);
      expect(result.where((c) => c == TaskColumnId.size), hasLength(1));
      expect(result, [TaskColumnId.size, TaskColumnId.speed]);
    });
  });

  group('fitColumnsToWidth (render-time auto-hide low-priority columns)', () {
    test('returns columns unchanged when within budget', () {
      final cols = [
        TaskColumnId.progress,
        TaskColumnId.speed,
        TaskColumnId.eta,
        TaskColumnId.status,
      ];
      // 380 needed; budget = 900 - 168 = 732.
      expect(fitColumnsToWidth(cols, 900), cols);
    });

    test('drops lowest-priority columns first until the set fits', () {
      // All 9 columns = 150+80+104+64+148+88+90+80+60 = 864 wide.
      final all = List<TaskColumnId>.from(kColumnCanonicalOrder);
      // Budget: 600 - 168 = 432 -> must drop source(148)/queue(88)/
      // protocol(64)/created(104) before size/eta/speed/status/progress.
      final fitted = fitColumnsToWidth(all, 600);
      expect(fitted.contains(TaskColumnId.source), isFalse);
      expect(fitted.contains(TaskColumnId.queue), isFalse);
      expect(fitted.contains(TaskColumnId.progress), isTrue);
      expect(fitted.contains(TaskColumnId.status), isTrue);
      expect(columnsTotalWidth(fitted), lessThanOrEqualTo(432));
      // Canonical order preserved among survivors.
      final canonicalFiltered = [
        for (final id in kColumnCanonicalOrder)
          if (fitted.contains(id)) id,
      ];
      expect(fitted, canonicalFiltered);
    });

    test('never drops below one column even when nothing fits', () {
      final cols = [TaskColumnId.progress, TaskColumnId.status];
      // Budget: 180 - 168 = 12 -> even one column exceeds it; keep exactly 1.
      final fitted = fitColumnsToWidth(cols, 180);
      expect(fitted.length, 1);
      expect(fitted.single, TaskColumnId.progress);
    });

    test('keep priority prefers progress/status over speed/eta', () {
      final cols = [
        TaskColumnId.progress,
        TaskColumnId.speed,
        TaskColumnId.eta,
        TaskColumnId.status,
      ];
      // Budget: 400 - 168 = 232 -> 380 needed; drop eta(80) then speed(90)
      // -> progress+status = 210 fits.
      final fitted = fitColumnsToWidth(cols, 400);
      expect(fitted, [TaskColumnId.progress, TaskColumnId.status]);
    });
  });
}
