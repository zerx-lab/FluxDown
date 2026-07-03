/// 悬浮球离屏渲染器 — 三变体 + 动态内容层（方案 A2/A7）。
///
/// 静态变体（可缓存）：idle / dragTarget；
/// 动态层：active（速度文本 + 环形进度 + 角标，数据驱动重绘）。
///
/// 输出双格式（A6）：
/// - Windows：premultiplied BGRA（UpdateLayeredWindow）
/// - macOS/Linux：straight-alpha RGBA（channel pushBitmap）
library;

import 'dart:io';
import 'dart:math' as math;
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show rootBundle;
import 'package:shadcn_ui/shadcn_ui.dart';

import '../../theme/flux_theme_tokens.dart';
import '../app_icon_service.dart';
import '../log_service.dart';
import '../native_overlay/offscreen_rasterizer.dart';

// =============================================================================
// Logo 预解码（idle 态球心图标）
// =============================================================================

/// 已解码的 logo 位图；null = 尚未加载（渲染时回退箭头图标）。
ui.Image? ballLogoImage;

/// 当前已加载的 logo 来源标识（`asset` 或 `custom#<revision>`），
/// 用于在应用图标切换后触发重载。
String? _loadedLogoKey;

/// 预解码 logo（按来源幂等）。FloatingBallService.enable() 前 await 一次；
/// 应用图标切换后再次调用即重载为新来源。
///
/// 来源跟随「设置-外观-应用图标」：自定义图标启用且预览 PNG 存在时用预览
/// （256px），内置「闪电」启用时用其打包资源，否则用内置 asset logo。
Future<void> ensureBallLogoLoaded() async {
  final iconSvc = AppIconService.instance;
  final customPath = iconSvc.isCustom ? iconSvc.previewPngPath : null;
  final String key;
  if (iconSvc.isBolt) {
    key = 'bolt';
  } else if (customPath != null) {
    key = 'custom#${iconSvc.previewRevision}';
  } else {
    key = 'asset';
  }
  if (ballLogoImage != null && _loadedLogoKey == key) return;
  try {
    final Uint8List bytes;
    if (customPath != null) {
      bytes = await File(customPath).readAsBytes();
    } else {
      final asset = iconSvc.isBolt
          ? AppIconService.builtinBoltAsset
          : 'assets/logo/fluxdown_logo.png';
      final data = await rootBundle.load(asset);
      bytes = data.buffer.asUint8List();
    }
    final codec = await ui.instantiateImageCodec(
      bytes,
      // 按最大 3x DPI 预留解码尺寸，避免上采样发糊
      targetWidth: ((kBallDiameter - 10) * 3).round(),
    );
    // 旧位图不主动 dispose：在途渲染可能仍引用，交由 GC finalizer 回收
    ballLogoImage = (await codec.getNextFrame()).image;
    _loadedLogoKey = key;
  } catch (e) {
    logError('BallRenderer', 'logo decode failed, fallback icon', e);
  }
}

// =============================================================================
// 规格常量（逻辑像素）— A7
// =============================================================================

/// 球体直径
const double kBallDiameter = 44;

/// 窗口逻辑尺寸（含 6px 阴影出血 × 2）
const double kBallWindowSize = 56;

/// 阴影出血
const double kBallShadowPad = (kBallWindowSize - kBallDiameter) / 2;

/// 圆形命中半径（逻辑像素，物理侧按 DPI scale 换算）
const double kBallHitRadius = kBallDiameter / 2;

// =============================================================================
// 渲染输入 / 输出
// =============================================================================

/// 球体视觉变体
enum BallVariant { idle, active, dragTarget }

/// 单帧位图（straight-alpha RGBA + premultiplied BGRA 双份按需产出）
class BallImage {
  final int width;
  final int height;

  /// straight-alpha RGBA（macOS/Linux channel 用）
  final Uint8List rgba;

  const BallImage(this.width, this.height, this.rgba);

  /// premultiplied BGRA（Windows UpdateLayeredWindow 用）
  Uint8List toBgraPremultiplied() => rgbaToPremultipliedBgra(rgba);
}

/// active 态渲染参数
class BallActiveSpec {
  /// 速度文本（如 "12.4M/s"，空串 = 不显示）
  final String speedText;

  /// 活跃任务数（角标）
  final int activeCount;

  /// 聚合进度 0..1；null = 不确定（环形显示不确定样式）
  final double? aggregateProgress;

  const BallActiveSpec({
    required this.speedText,
    required this.activeCount,
    required this.aggregateProgress,
  });

  @override
  bool operator ==(Object other) =>
      other is BallActiveSpec &&
      other.speedText == speedText &&
      other.activeCount == activeCount &&
      other.aggregateProgress == aggregateProgress;

  @override
  int get hashCode => Object.hash(speedText, activeCount, aggregateProgress);
}

// =============================================================================
// 渲染入口
// =============================================================================

/// 渲染一帧悬浮球位图。
///
/// [variant]=active 时必须提供 [activeSpec]。
/// `scale` 为目标显示器 DPI/96（Windows）或 backingScaleFactor（macOS）。
Future<BallImage> renderBallImage({
  required BallVariant variant,
  required FluxThemeTokens tokens,
  required double scale,
  BallActiveSpec? activeSpec,
}) async {
  assert(
    variant != BallVariant.active || activeSpec != null,
    'active variant requires activeSpec',
  );
  final (w, h, rgba) = await rasterizeWidgetRgba(
    _BallWidget(variant: variant, tokens: tokens, activeSpec: activeSpec),
    logicalSize: const Size(kBallWindowSize, kBallWindowSize),
    scale: scale,
  );
  return BallImage(w, h, rgba);
}

// =============================================================================
// 球体 widget
// =============================================================================

class _BallWidget extends StatelessWidget {
  final BallVariant variant;
  final FluxThemeTokens tokens;
  final BallActiveSpec? activeSpec;

  const _BallWidget({
    required this.variant,
    required this.tokens,
    this.activeSpec,
  });

  @override
  Widget build(BuildContext context) {
    final accent = tokens.accent;
    final bg = tokens.surface1;
    final isDragTarget = variant == BallVariant.dragTarget;
    final spec = activeSpec;
    final logo = ballLogoImage;
    // idle 态且 logo 可用：logo 直接铺满整球（无底色圈/边框）
    final logoFillsBall = variant == BallVariant.idle && logo != null;

    return Center(
      child: SizedBox(
        width: kBallDiameter,
        height: kBallDiameter,
        child: Stack(
          clipBehavior: Clip.none,
          children: [
            // ── 球体主体 ──
            Container(
              width: kBallDiameter,
              height: kBallDiameter,
              decoration: BoxDecoration(
                color: logoFillsBall
                    ? null
                    : (isDragTarget ? accent.withValues(alpha: 0.92) : bg),
                shape: BoxShape.circle,
                border: logoFillsBall
                    ? null
                    : Border.all(
                        color: isDragTarget
                            ? accent
                            : tokens.border.withValues(alpha: 0.8),
                        width: isDragTarget ? 2 : 1,
                      ),
                boxShadow: [
                  BoxShadow(
                    color: Colors.black.withValues(alpha: 0.25),
                    blurRadius: 8,
                    offset: const Offset(0, 2),
                  ),
                ],
              ),
              child: logoFillsBall
                  ? ClipOval(
                      child: RawImage(
                        image: logo,
                        width: kBallDiameter,
                        height: kBallDiameter,
                        fit: BoxFit.cover,
                        filterQuality: FilterQuality.medium,
                      ),
                    )
                  : _buildContent(isDragTarget, accent),
            ),
            // ── 进度环（active 态）──
            if (variant == BallVariant.active)
              Positioned.fill(
                child: CustomPaint(
                  painter: _ProgressRingPainter(
                    progress: spec?.aggregateProgress,
                    color: accent,
                  ),
                ),
              ),
            // ── 活跃数角标 ──
            if (variant == BallVariant.active && (spec?.activeCount ?? 0) > 0)
              Positioned(
                top: -4,
                right: -4,
                child: Container(
                  padding: const EdgeInsets.symmetric(
                    horizontal: 5,
                    vertical: 1,
                  ),
                  decoration: BoxDecoration(
                    color: accent,
                    borderRadius: BorderRadius.circular(9),
                    border: Border.all(color: bg, width: 1.5),
                  ),
                  constraints: const BoxConstraints(minWidth: 18),
                  child: Text(
                    '${spec!.activeCount > 99 ? '99+' : spec.activeCount}',
                    textAlign: TextAlign.center,
                    style: TextStyle(
                      fontFamily: 'MiSans',
                      fontSize: 10,
                      fontWeight: FontWeight.w600,
                      color: _contrastOn(accent),
                      height: 1.3,
                    ),
                  ),
                ),
              ),
          ],
        ),
      ),
    );
  }

  Widget _buildContent(bool isDragTarget, Color accent) {
    if (isDragTarget) {
      return Icon(
        LucideIcons.plus,
        size: 20,
        color: _contrastOn(accent),
      );
    }
    final spec = activeSpec;
    if (variant == BallVariant.active && spec != null) {
      // 速度文本居中（形如 "12.4M"，去掉 "/s" 省空间）
      final compact = spec.speedText
          .replaceAll('/s', '')
          .replaceAll(' ', '')
          .replaceAll('B', '');
      return Center(
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 4),
          child: FittedBox(
            fit: BoxFit.scaleDown,
            child: Text(
              compact,
              maxLines: 1,
              style: TextStyle(
                fontFamily: 'MiSans',
                fontSize: 13,
                fontWeight: FontWeight.w600,
                color: tokens.textPrimary,
                height: 1.0,
              ),
            ),
          ),
        ),
      );
    }
    // idle 无 logo 时走兜底（logo 可用时已在 build 里整球填充，不进本方法）
    // logo 未就绪的兜底（首帧竞态）
    return Icon(
      LucideIcons.arrowDownToLine,
      size: 18,
      color: tokens.textMuted,
    );
  }

  /// 强调色上的对比前景色
  static Color _contrastOn(Color c) =>
      c.computeLuminance() > 0.5 ? const Color(0xFF18181B) : Colors.white;
}

/// 环形进度画笔 — progress==null 时画 3/4 圆弧表示不确定态
class _ProgressRingPainter extends CustomPainter {
  final double? progress;
  final Color color;

  _ProgressRingPainter({required this.progress, required this.color});

  @override
  void paint(Canvas canvas, Size size) {
    final center = Offset(size.width / 2, size.height / 2);
    final radius = kBallDiameter / 2 - 1.25;
    final paint = Paint()
      ..style = PaintingStyle.stroke
      ..strokeWidth = 2.5
      ..strokeCap = StrokeCap.round
      ..color = color;

    final p = progress;
    final sweep = p == null
        ? math.pi * 1.5 // 不确定态：3/4 弧
        : (p.clamp(0.0, 1.0)) * math.pi * 2;
    if (sweep <= 0) return;
    canvas.drawArc(
      Rect.fromCircle(center: center, radius: radius),
      -math.pi / 2,
      sweep,
      false,
      paint,
    );
  }

  @override
  bool shouldRepaint(_ProgressRingPainter old) =>
      old.progress != progress || old.color != color;
}
