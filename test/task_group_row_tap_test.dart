// 组行点击语义回归（2026-07 行为拆分）：
//
// 契约：行点击 = 查看组详情（onTap 选中组），展开/收起仅由左侧 chevron
// 子手势触发（onToggleExpand），二者互不连带。回归防线：曾经整行 onTap
// 同时 toggleGroupExpanded + onGroupTap，点任何区域都会展开。
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/i18n/locale_provider.dart';
import 'package:flux_down/src/models/list_entity.dart';
import 'package:flux_down/src/models/task_group.dart';
import 'package:flux_down/src/models/view_prefs.dart';
import 'package:flux_down/src/theme/app_theme.dart';
import 'package:flux_down/src/theme/flux_theme_tokens.dart';
import 'package:flux_down/src/widgets/task_group_card.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

Widget _harness(Widget home) {
  final tokens = FluxThemeTokens.defaultDark();
  final theme = buildThemeFromTokens(tokens);
  return LocaleScope(
    s: S.of('zh'),
    child: FluxThemeScope(
      tokens: tokens,
      child: ShadTheme(
        data: theme,
        child: Directionality(
          textDirection: TextDirection.ltr,
          child: DefaultTextStyle(
            style: theme.textTheme.p,
            child: WidgetsApp(
              color: theme.colorScheme.primary,
              debugShowCheckedModeBanner: false,
              home: home,
              pageRouteBuilder: <T>(RouteSettings s, WidgetBuilder b) =>
                  PageRouteBuilder<T>(
                    settings: s,
                    pageBuilder: (context, _, _) => b(context),
                  ),
            ),
          ),
        ),
      ),
    ),
  );
}

void main() {
  testWidgets('组行：图标向左区域展开收起，其余区域选中查看详情', (tester) async {
    const groupName = '千项压测组';
    var tapCount = 0;
    var toggleCount = 0;
    final epoch = DateTime.fromMillisecondsSinceEpoch(0);

    final entity = GroupEntity(
      groupId: 'g1',
      groupName: groupName,
      sourceUrl: 'https://example.com/share',
      saveDir: 'C:/downloads/g1',
      groupCreatedAt: epoch,
      groupQueueId: 'main',
    );
    final downloadGroup = DownloadGroup(
      id: 'g1',
      name: groupName,
      sourceUrl: 'https://example.com/share',
      saveDir: 'C:/downloads/g1',
      createdAt: epoch,
    );

    await tester.pumpWidget(
      _harness(
        TaskGroupRow(
          group: entity,
          downloadGroup: downloadGroup,
          expanded: false,
          isSelected: false,
          density: ViewDensity.comfortable,
          onTap: () => tapCount++,
          onToggleExpand: () => toggleCount++,
          onPauseAll: () {},
          onResumeAll: () {},
          onOpenFolder: () {},
          onCopySource: () {},
          onDelete: ({required bool deleteFiles}) {},
        ),
      ),
    );

    // chevron 点击 → 仅 onToggleExpand。
    await tester.tap(find.byIcon(LucideIcons.chevronRight));
    expect(toggleCount, 1);
    expect(tapCount, 0);

    // 组图标点击 → 同属展开命中区，仅 onToggleExpand。
    await tester.tap(find.byIcon(LucideIcons.layers));
    expect(toggleCount, 2);
    expect(tapCount, 0);

    // 行内其余区域（组名文本）点击 → 仅 onTap。
    await tester.tap(find.text(groupName));
    expect(tapCount, 1);
    expect(toggleCount, 2);
  });
}
