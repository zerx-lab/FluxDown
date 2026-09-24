import 'package:flutter/material.dart';

import '../theme/app_colors.dart';

/// An icon with a green corner indicator when its RSS source has unread items.
class RssUnreadIcon extends StatelessWidget {
  final Widget child;
  final int unreadCount;
  final Color surfaceColor;

  const RssUnreadIcon({
    super.key,
    required this.child,
    required this.unreadCount,
    required this.surfaceColor,
  });

  @override
  Widget build(BuildContext context) {
    return Stack(
      clipBehavior: Clip.none,
      children: [
        child,
        if (unreadCount > 0)
          Positioned(
            top: -2,
            right: -3,
            child: Container(
              key: const ValueKey('rss-unread-indicator'),
              width: 6,
              height: 6,
              decoration: BoxDecoration(
                color: AppColors.green,
                shape: BoxShape.circle,
                border: Border.all(color: surfaceColor, width: 1),
              ),
            ),
          ),
      ],
    );
  }
}
