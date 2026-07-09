import 'package:flutter/widgets.dart';

import 'flux_metric_tokens.dart';
import 'flux_theme_tokens.dart';

/// 主题感知的非颜色设计变量门面 — 通过 `AppMetrics.of(context)` 获取。
///
/// 与 [AppColors] 平行：圆角 / 间距 / 描边 / 按钮尺寸 / 透明度 / 移动端几何
/// 均从 [FluxThemeTokens.metric]（[FluxMetricTokens]）读取，主题可独立覆盖
/// 每个命名角色。透明度经「基色 + alpha 角色」派生（`soft(color)` 等），
/// 消灭组件里的 magic number。
class AppMetrics {
  final FluxMetricTokens _m;

  const AppMetrics._(this._m);

  factory AppMetrics.of(BuildContext context) =>
      AppMetrics._(FluxThemeScope.of(context).metric);

  /// 直接从 token 构造（供不依赖 context 的场景使用）。
  factory AppMetrics.fromTokens(FluxThemeTokens tokens) =>
      AppMetrics._(tokens.metric);

  /// 原始 metric 访问（供高级场景使用）。
  FluxMetricTokens get metric => _m;

  // ═══════════════════════════════════════════════════════════
  //  圆角（double）
  // ═══════════════════════════════════════════════════════════
  double get progress => _m.radiusProgress;
  double get xs => _m.radiusXs;
  double get segmentCell => _m.radiusSegmentCell;
  double get sm => _m.radiusSm;
  double get md => _m.radiusMd;
  double get input => _m.radiusInput;
  double get card => _m.radiusCard;
  double get iconTile => _m.radiusIconTile;
  double get dialog => _m.radiusDialog;
  double get fieldMobile => _m.radiusFieldMobile;
  double get chipLg => _m.radiusChipLg;
  double get chipXl => _m.radiusChipXl;
  double get badge => _m.radiusBadge;
  double get pill => _m.radiusPill;
  double get sheet => _m.radiusSheet;

  // ═══════════════════════════════════════════════════════════
  //  BorderRadius 便捷 getter（减少 callsite 噪音）
  // ═══════════════════════════════════════════════════════════
  BorderRadius get brProgress => BorderRadius.circular(_m.radiusProgress);
  BorderRadius get brXs => BorderRadius.circular(_m.radiusXs);
  BorderRadius get brSegmentCell => BorderRadius.circular(_m.radiusSegmentCell);
  BorderRadius get brSm => BorderRadius.circular(_m.radiusSm);
  BorderRadius get brMd => BorderRadius.circular(_m.radiusMd);
  BorderRadius get brInput => BorderRadius.circular(_m.radiusInput);
  BorderRadius get brCard => BorderRadius.circular(_m.radiusCard);
  BorderRadius get brIconTile => BorderRadius.circular(_m.radiusIconTile);
  BorderRadius get brDialog => BorderRadius.circular(_m.radiusDialog);
  BorderRadius get brFieldMobile => BorderRadius.circular(_m.radiusFieldMobile);
  BorderRadius get brChipLg => BorderRadius.circular(_m.radiusChipLg);
  BorderRadius get brChipXl => BorderRadius.circular(_m.radiusChipXl);
  BorderRadius get brBadge => BorderRadius.circular(_m.radiusBadge);
  BorderRadius get brPill => BorderRadius.circular(_m.radiusPill);
  BorderRadius get brSheet => BorderRadius.circular(_m.radiusSheet);

  /// 底部弹层顶部圆角（仅上边圆角）。
  BorderRadius get brSheetTop =>
      BorderRadius.vertical(top: Radius.circular(_m.radiusSheet));

  // ═══════════════════════════════════════════════════════════
  //  间距 / 描边 / 按钮高度（double）
  // ═══════════════════════════════════════════════════════════
  double get spacingXs => _m.spacingXs;
  double get spacingSm => _m.spacingSm;
  double get spacingMd => _m.spacingMd;
  double get spacingLg => _m.spacingLg;
  double get spacingXl => _m.spacingXl;
  double get strokeThin => _m.strokeThin;
  double get strokeThick => _m.strokeThick;
  double get buttonHeightSm => _m.buttonHeightSm;
  double get buttonHeightMd => _m.buttonHeightMd;
  double get buttonHeightLg => _m.buttonHeightLg;

  // ═══════════════════════════════════════════════════════════
  //  移动端几何（double）
  // ═══════════════════════════════════════════════════════════
  double get mobilePageMargin => _m.mobilePageMargin;
  double get mobileCardRadius => _m.mobileCardRadius;
  double get mobileCardGap => _m.mobileCardGap;
  double get mobileAppBarHeight => _m.mobileAppBarHeight;
  double get mobileTabsHeight => _m.mobileTabsHeight;
  double get mobileDockBottomGap => _m.mobileDockBottomGap;
  double get mobileFabSize => _m.mobileFabSize;
  double get mobileScrollBottomPadding => _m.mobileScrollBottomPadding;

  /// 移动卡片圆角便捷 getter。
  BorderRadius get brMobileCard => BorderRadius.circular(_m.mobileCardRadius);

  // ═══════════════════════════════════════════════════════════
  //  透明度派生（基色 + alpha 角色 → 半透明色）
  // ═══════════════════════════════════════════════════════════
  Color subtle(Color base) => base.withValues(alpha: _m.alphaSubtle);
  Color soft(Color base) => base.withValues(alpha: _m.alphaSoft);
  Color muted(Color base) => base.withValues(alpha: _m.alphaMuted);
  Color mutedStrong(Color base) => base.withValues(alpha: _m.alphaMutedStrong);
  Color active(Color base) => base.withValues(alpha: _m.alphaActive);
  Color selectedBorder(Color base) =>
      base.withValues(alpha: _m.alphaSelectedBorder);
  Color scrim(Color base) => base.withValues(alpha: _m.alphaScrim);
  Color borderFade(Color base) => base.withValues(alpha: _m.alphaBorder);
  Color borderStrong(Color base) =>
      base.withValues(alpha: _m.alphaBorderStrong);
  Color disabled(Color base) => base.withValues(alpha: _m.alphaDisabled);
  Color glass(Color base) => base.withValues(alpha: _m.alphaGlass);
  Color focusRing(Color base) => base.withValues(alpha: _m.alphaFocusRing);
  Color shadowStrong(Color base) =>
      base.withValues(alpha: _m.alphaShadowStrong);
  Color shadowSoft(Color base) => base.withValues(alpha: _m.alphaShadowSoft);
  Color shadowFaint(Color base) => base.withValues(alpha: _m.alphaShadowFaint);
  Color faint(Color base) => base.withValues(alpha: _m.alphaFaint);
  Color textSelection(Color base) =>
      base.withValues(alpha: _m.alphaTextSelection);
  Color borderSubtle(Color base) =>
      base.withValues(alpha: _m.alphaBorderSubtle);
  Color borderFaint(Color base) => base.withValues(alpha: _m.alphaBorderFaint);
  Color borderMedium(Color base) =>
      base.withValues(alpha: _m.alphaBorderMedium);
  Color emphasis(Color base) => base.withValues(alpha: _m.alphaEmphasis);
  Color glassSubtle(Color base) => base.withValues(alpha: _m.alphaGlassSubtle);

  // ── 裸 alpha 值（供渐变 stops 等非「基色+alpha」场景）──
  double get alphaSubtle => _m.alphaSubtle;
  double get alphaSoft => _m.alphaSoft;
  double get alphaMuted => _m.alphaMuted;
  double get alphaMutedStrong => _m.alphaMutedStrong;
  double get alphaActive => _m.alphaActive;
  double get alphaSelectedBorder => _m.alphaSelectedBorder;
  double get alphaScrim => _m.alphaScrim;
  double get alphaBorder => _m.alphaBorder;
  double get alphaBorderStrong => _m.alphaBorderStrong;
  double get alphaDisabled => _m.alphaDisabled;
  double get alphaGlass => _m.alphaGlass;
  double get alphaFocusRing => _m.alphaFocusRing;
  double get alphaFaint => _m.alphaFaint;
  double get alphaTextSelection => _m.alphaTextSelection;
  double get alphaBorderSubtle => _m.alphaBorderSubtle;
  double get alphaBorderFaint => _m.alphaBorderFaint;
  double get alphaBorderMedium => _m.alphaBorderMedium;
  double get alphaEmphasis => _m.alphaEmphasis;
  double get alphaGlassSubtle => _m.alphaGlassSubtle;
}
