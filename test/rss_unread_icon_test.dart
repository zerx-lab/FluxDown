import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/theme/app_colors.dart';
import 'package:flux_down/src/widgets/rss_unread_icon.dart';

void main() {
  Widget buildSubject(int unreadCount) => Directionality(
    textDirection: TextDirection.ltr,
    child: RssUnreadIcon(
      unreadCount: unreadCount,
      surfaceColor: const Color(0xFFFFFFFF),
      child: const Icon(Icons.rss_feed, size: 14),
    ),
  );

  testWidgets('shows a green corner dot when unread items exist', (
    tester,
  ) async {
    await tester.pumpWidget(buildSubject(1));

    final dot = tester.widget<Container>(
      find.byKey(const ValueKey('rss-unread-indicator')),
    );
    final decoration = dot.decoration! as BoxDecoration;
    expect(decoration.color, AppColors.green);
    expect(decoration.shape, BoxShape.circle);
    expect(find.byIcon(Icons.rss_feed), findsOneWidget);
  });

  testWidgets('hides the dot when there are no unread items', (tester) async {
    await tester.pumpWidget(buildSubject(0));

    expect(find.byKey(const ValueKey('rss-unread-indicator')), findsNothing);
    expect(find.byIcon(Icons.rss_feed), findsOneWidget);
  });
}
