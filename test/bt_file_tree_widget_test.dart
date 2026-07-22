import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/bindings/bindings.dart';
import 'package:flux_down/src/i18n/locale_provider.dart';
import 'package:flux_down/src/theme/app_theme.dart';
import 'package:flux_down/src/theme/flux_theme_tokens.dart';
import 'package:flux_down/src/widgets/bt_file_list_widget.dart';
import 'package:flux_down/src/widgets/bt_file_selection_shared.dart'
    show BtCheckbox, toggleBtFileSelection;
import 'package:flux_down/src/widgets/bt_file_selection_view.dart';
import 'package:flux_down/src/widgets/bt_file_tree_widget.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

const _files = [
  BtFileEntry(index: 0, path: 'Example/Season 1/Episode 1.mkv', size: 100),
  BtFileEntry(index: 1, path: r'Example\Season 1\Episode 2.mkv', size: 200),
  BtFileEntry(index: 2, path: 'Example/Season 2/Episode 3.mkv', size: 300),
  BtFileEntry(index: 3, path: 'Example/readme.txt', size: 20),
];

Widget _wrap(Widget child) {
  final tokens = FluxThemeTokens.defaultDark();
  final theme = buildThemeFromTokens(tokens);
  return FluxThemeScope(
    tokens: tokens,
    child: ShadTheme(
      data: theme,
      child: LocaleScope(
        s: S.of('zh'),
        child: Directionality(
          textDirection: TextDirection.ltr,
          child: DefaultTextStyle(
            style: theme.textTheme.p.copyWith(
              color: theme.colorScheme.foreground,
            ),
            child: WidgetsApp(
              color: theme.colorScheme.primary,
              debugShowCheckedModeBanner: false,
              home: Center(child: SizedBox(width: 560, child: child)),
              pageRouteBuilder: <T>(RouteSettings settings, WidgetBuilder b) {
                return PageRouteBuilder<T>(
                  settings: settings,
                  pageBuilder: (context, _, _) => b(context),
                );
              },
            ),
          ),
        ),
      ),
    ),
  );
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(I18nStore.load);

  test('buildBtFileTree preserves nested hierarchy for both separators', () {
    final roots = buildBtFileTree(_files);
    final example = roots.singleWhere((node) => node.name == 'Example');
    final season1 = example.children.singleWhere(
      (node) => node.name == 'Season 1',
    );

    expect(example.isDirectory, isTrue);
    expect(example.descendantFiles.map((file) => file.index), {0, 1, 2, 3});
    expect(season1.descendantFiles.map((file) => file.index), {0, 1});
    expect(season1.children.map((node) => node.name), [
      'Episode 1.mkv',
      'Episode 2.mkv',
    ]);
  });

  test('toggleBtFileSelection toggles a whole branch atomically', () {
    expect(toggleBtFileSelection({0, 1, 2, 3}, [0, 1]), {2, 3});
    expect(toggleBtFileSelection({2, 3}, [0, 1]), {0, 1, 2, 3});
    expect(toggleBtFileSelection({0, 2, 3}, [0, 1]), {0, 1, 2, 3});
  });

  testWidgets('directory rows cascade selection and expose partial state', (
    tester,
  ) async {
    var selected = <int>{0, 1, 2, 3};

    await tester.pumpWidget(
      _wrap(
        StatefulBuilder(
          builder: (context, setState) => BtFileSelectionView(
            files: _files,
            selectedIndices: selected,
            onSelectionChanged: (value) {
              setState(() => selected = value);
            },
          ),
        ),
      ),
    );

    expect(find.text('Episode 1.mkv'), findsOneWidget);
    await tester.tap(
      find.byKey(const ValueKey('bt-tree-select:Example/Season 1')),
    );
    await tester.pump();

    expect(selected, {2, 3});
    final rootCheckbox = tester.widget<BtCheckbox>(
      find.descendant(
        of: find.byKey(const ValueKey('bt-tree-dir:Example')),
        matching: find.byType(BtCheckbox),
      ),
    );
    expect(rootCheckbox.checked, isFalse);
    expect(rootCheckbox.indeterminate, isTrue);

    await tester.tap(
      find.byKey(const ValueKey('bt-tree-expand:Example/Season 1')),
    );
    await tester.pump();
    expect(find.text('Episode 1.mkv'), findsNothing);
    expect(selected, {2, 3});

    await tester.tap(
      find.byKey(const ValueKey('bt-tree-open:Example/Season 1')),
    );
    await tester.pump();
    expect(find.text('Episode 1.mkv'), findsOneWidget);

    await tester.tap(
      find.byKey(const ValueKey('bt-tree-select:Example/Season 1')),
    );
    await tester.pump();
    expect(selected, {0, 1, 2, 3});
    expect(tester.takeException(), isNull);
  });

  testWidgets('same-level directory and file checkboxes are aligned', (
    tester,
  ) async {
    await tester.pumpWidget(
      _wrap(
        BtFileTreeWidget(
          files: _files,
          selectedIndices: const {0, 1, 2, 3},
          onSelectionChanged: (_) {},
        ),
      ),
    );

    final directoryCheckbox = find.descendant(
      of: find.byKey(const ValueKey('bt-tree-dir:Example/Season 1')),
      matching: find.byType(BtCheckbox),
    );
    final fileCheckbox = find.descendant(
      of: find.byKey(const ValueKey('bt-tree-file:3')),
      matching: find.byType(BtCheckbox),
    );

    expect(
      tester.getTopLeft(directoryCheckbox).dx,
      tester.getTopLeft(fileCheckbox).dx,
    );
    expect(tester.takeException(), isNull);
  });

  testWidgets('defaults to tree view and can switch back to flat list', (
    tester,
  ) async {
    await tester.pumpWidget(
      _wrap(
        BtFileSelectionView(
          files: _files,
          selectedIndices: const {0, 1, 2, 3},
          onSelectionChanged: (_) {},
        ),
      ),
    );

    expect(find.byKey(const ValueKey('bt-tree-dir:Example')), findsOneWidget);
    expect(find.byType(BtFileTile), findsNothing);

    await tester.tap(find.byKey(const ValueKey('bt-view-list')));
    await tester.pump();

    expect(find.byKey(const ValueKey('bt-tree-dir:Example')), findsNothing);
    expect(find.byType(BtFileTile), findsNWidgets(_files.length));
    expect(find.text('Season 1'), findsWidgets);

    await tester.tap(find.byKey(const ValueKey('bt-view-tree')));
    await tester.pump();
    expect(find.byKey(const ValueKey('bt-tree-dir:Example')), findsOneWidget);
    expect(tester.takeException(), isNull);
  });
}
