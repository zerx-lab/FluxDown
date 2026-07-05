import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/painting.dart';
import 'package:flutter/services.dart' show rootBundle;
import 'package:path/path.dart' as p;
import 'package:shared_preferences/shared_preferences.dart';
import 'package:window_manager/window_manager.dart';

import 'ico_codec.dart';
import 'log_service.dart';
import 'platform_utils.dart';
import 'tray_service.dart';

const _tag = 'AppIconService';
const _kPrefsInitTimeout = Duration(seconds: 3);

/// 应用图标选择。
enum AppIconChoice {
  /// 默认图标（exe 资源 / app_icon.ico）。
  defaultIcon,

  /// 内置备选图标「闪电」（assets/logo/fluxdown_bolt.png）。
  bolt,

  /// 用户导入的自定义图标。
  custom,
}

/// 动态应用图标服务（仅 Windows 生效）。
///
/// 管理窗口/任务栏/托盘图标在「默认」「内置闪电」「自定义」之间的切换：
/// - 默认图标来自 exe 资源与 CMake install 的 `app_icon.ico`；
/// - 内置「闪电」图标由打包资源 [builtinBoltAsset] 在应用时渲染为多尺寸
///   PNG 压缩 ICO，缓存在数据目录 `icons/bolt_icon.ico`；
/// - 自定义图标由用户选择的图片（png/jpg/webp/bmp/ico）转换为多尺寸
///   PNG 压缩 ICO，持久化在数据目录 `icons/custom_icon.ico`；
/// - 运行时通过 `window_manager.setIcon`（WM_SETICON）替换窗口与任务栏
///   图标，托盘图标经 [TrayService.setCustomIcon] 覆盖；
/// - 选择持久化在 SharedPreferences，每次启动 [init] 时重新应用
///   （WM_SETICON 仅对当前进程生效）。
///
/// 注意：固定到任务栏的快捷方式图标来自 .lnk/exe 资源，运行时无法修改；
/// 本服务只影响运行中窗口的任务栏按钮、Alt-Tab 与托盘。
class AppIconService extends ChangeNotifier {
  AppIconService._();
  static final AppIconService instance = AppIconService._();

  static const _kCustomEnabled = 'app_icon_custom'; // 旧版 bool 键（迁移用）
  static const _kChoiceKey = 'app_icon_choice';

  /// 内置备选图标「闪电」的打包资源路径（UI 预览也直接引用）。
  static const builtinBoltAsset = 'assets/logo/fluxdown_bolt.png';

  /// 生成 ICO 时渲染的正方形尺寸集合。
  static const _iconSizes = [16, 24, 32, 48, 64, 128, 256];

  AppIconChoice _choice = AppIconChoice.defaultIcon;

  /// 预览文件内容版本号 — 每次导入自增，供 UI 作为 Image key 破除缓存。
  int _previewRevision = 0;

  /// 当前的图标选择。
  AppIconChoice get choice => _choice;

  /// 当前是否启用自定义图标。
  bool get isCustom => _choice == AppIconChoice.custom;

  /// 当前是否启用内置「闪电」图标。
  bool get isBolt => _choice == AppIconChoice.bolt;

  int get previewRevision => _previewRevision;

  String get _iconsDir => p.join(resolveDataDir(), 'icons');
  String get _customIcoPath => p.join(_iconsDir, 'custom_icon.ico');
  String get _boltIcoPath => p.join(_iconsDir, 'bolt_icon.ico');
  String get _previewPath => p.join(_iconsDir, 'custom_icon_preview.png');

  /// 自定义图标文件是否已存在（曾成功导入过）。
  bool get hasCustomIcon => File(_customIcoPath).existsSync();

  /// 自定义图标的预览 PNG 路径；不存在时返回 `null`。
  String? get previewPngPath {
    final f = File(_previewPath);
    return f.existsSync() ? f.path : null;
  }

  static String get _defaultIcoPath =>
      p.join(File(Platform.resolvedExecutable).parent.path, 'app_icon.ico');

  /// 启动时恢复持久化的图标选择。需在 `windowManager.ensureInitialized`
  /// 与 `TrayService.init` 之后调用。
  Future<void> init() async {
    if (!Platform.isWindows) return;
    try {
      final prefs = await SharedPreferences.getInstance().timeout(
        _kPrefsInitTimeout,
      );
      var choice = _readChoice(prefs);
      if (choice == AppIconChoice.custom && !hasCustomIcon) {
        choice = AppIconChoice.defaultIcon;
        await prefs.setString(_kChoiceKey, _choiceTag(choice));
        logInfo(_tag, 'custom icon file missing, falling back to default');
      }
      _choice = choice;
      switch (choice) {
        case AppIconChoice.defaultIcon:
          break;
        case AppIconChoice.bolt:
          await _buildBoltIco();
          await _applyIco(_boltIcoPath);
          logInfo(_tag, 'restored bolt app icon: $_boltIcoPath');
        case AppIconChoice.custom:
          await _applyIco(_customIcoPath);
          logInfo(_tag, 'restored custom app icon: $_customIcoPath');
      }
    } catch (e, stack) {
      logError(_tag, 'init failed', e, stack);
    }
  }

  /// 读取持久化选择；无新键时从旧版 bool 键迁移。
  AppIconChoice _readChoice(SharedPreferences prefs) {
    final tag = prefs.getString(_kChoiceKey);
    if (tag != null) {
      return AppIconChoice.values.firstWhere(
        (c) => _choiceTag(c) == tag,
        orElse: () => AppIconChoice.defaultIcon,
      );
    }
    // 旧版本仅有 bool 键：true=自定义
    final legacyCustom = prefs.getBool(_kCustomEnabled) ?? false;
    return legacyCustom ? AppIconChoice.custom : AppIconChoice.defaultIcon;
  }

  static String _choiceTag(AppIconChoice c) => switch (c) {
    AppIconChoice.defaultIcon => 'default',
    AppIconChoice.bolt => 'bolt',
    AppIconChoice.custom => 'custom',
  };

  /// 切回默认应用图标。
  Future<void> useDefault() async {
    if (!Platform.isWindows) return;
    try {
      await windowManager.setIcon(_defaultIcoPath);
      await TrayService.instance.setCustomIcon(null);
    } catch (e, stack) {
      // 应用失败不阻塞持久化：图标文件有效时下次启动仍可生效
      logError(_tag, 'useDefault: failed to apply icon', e, stack);
    }
    _choice = AppIconChoice.defaultIcon;
    await _persist();
    notifyListeners();
  }

  /// 切换到内置「闪电」图标。ICO 每次应用时从打包资源重建，
  /// 保证应用升级后资源更新不会残留旧缓存。
  Future<void> useBolt() async {
    if (!Platform.isWindows) return;
    try {
      await _buildBoltIco();
      await _applyIco(_boltIcoPath);
    } catch (e, stack) {
      logError(_tag, 'useBolt: failed to apply icon', e, stack);
    }
    _choice = AppIconChoice.bolt;
    await _persist();
    notifyListeners();
  }

  /// 切换到已导入的自定义图标。[hasCustomIcon] 为 false 时无操作。
  Future<void> useCustom() async {
    if (!Platform.isWindows || !hasCustomIcon) return;
    try {
      await _applyIco(_customIcoPath);
    } catch (e, stack) {
      logError(_tag, 'useCustom: failed to apply icon', e, stack);
    }
    _choice = AppIconChoice.custom;
    await _persist();
    notifyListeners();
  }

  /// 导入用户选择的图片并立即启用为自定义图标。
  ///
  /// - `.ico` 文件直接拷贝（Flutter 无法解码 ICO，预览取其中最大的
  ///   PNG 条目，纯 BMP 条目的旧式 ICO 无预览）；
  /// - 其余格式（png/jpg/webp/bmp/gif 首帧）解码后按 [_iconSizes]
  ///   居中等比渲染为透明底正方形 PNG，编码为多尺寸 ICO。
  ///
  /// 解码失败或 IO 错误时抛出，由调用方提示用户。
  Future<void> importAndApply(String sourcePath) async {
    if (!Platform.isWindows) return;
    final bytes = await File(sourcePath).readAsBytes();
    final Uint8List ico;
    final Uint8List? preview;
    if (looksLikeIco(bytes)) {
      ico = bytes;
      preview = extractLargestPngEntry(bytes);
    } else {
      final rendered = await _renderIcoPngs(bytes);
      ico = buildIcoFromPngs(rendered);
      // 预览取 256px 条目，供设置页放大查看与侧边栏 Logo 使用
      preview = rendered.firstWhere((e) => e.size == 256).png;
    }

    await Directory(_iconsDir).create(recursive: true);
    // 临时文件 + rename 原子替换，防止半写文件被下次启动加载。
    // LoadImage/托盘不持有文件锁，删除旧文件安全。
    final tmp = File('$_customIcoPath.tmp');
    await tmp.writeAsBytes(ico, flush: true);
    final dest = File(_customIcoPath);
    if (await dest.exists()) {
      await dest.delete();
    }
    await tmp.rename(_customIcoPath);

    final previewFile = File(_previewPath);
    if (preview != null) {
      await previewFile.writeAsBytes(preview, flush: true);
    } else if (await previewFile.exists()) {
      await previewFile.delete();
    }
    await FileImage(previewFile).evict();
    _previewRevision++;
    logInfo(_tag, 'imported custom icon from $sourcePath');
    await useCustom();
  }

  Future<void> _applyIco(String path) async {
    await windowManager.setIcon(path);
    await TrayService.instance.setCustomIcon(path);
  }

  /// 从打包资源渲染「闪电」ICO 并原子写入 [_boltIcoPath]。
  Future<void> _buildBoltIco() async {
    final data = await rootBundle.load(builtinBoltAsset);
    final bytes = data.buffer.asUint8List(
      data.offsetInBytes,
      data.lengthInBytes,
    );
    final rendered = await _renderIcoPngs(bytes);
    final ico = buildIcoFromPngs(rendered);
    await Directory(_iconsDir).create(recursive: true);
    final tmp = File('$_boltIcoPath.tmp');
    await tmp.writeAsBytes(ico, flush: true);
    final dest = File(_boltIcoPath);
    if (await dest.exists()) {
      await dest.delete();
    }
    await tmp.rename(_boltIcoPath);
  }

  Future<void> _persist() async {
    try {
      final prefs = await SharedPreferences.getInstance();
      await prefs.setString(_kChoiceKey, _choiceTag(_choice));
    } catch (e, stack) {
      logError(_tag, 'failed to persist app icon setting', e, stack);
    }
  }

  /// 解码源图片并渲染 [_iconSizes] 全套正方形 PNG。
  Future<List<IcoPngEntry>> _renderIcoPngs(Uint8List source) async {
    final codec = await ui.instantiateImageCodec(source);
    final frame = await codec.getNextFrame();
    final src = frame.image;
    try {
      final out = <IcoPngEntry>[];
      for (final size in _iconSizes) {
        out.add(
          IcoPngEntry(size: size, png: await _renderSquarePng(src, size)),
        );
      }
      return out;
    } finally {
      src.dispose();
      codec.dispose();
    }
  }

  /// 将 [src] 居中等比缩放绘制到 size×size 透明画布，输出 PNG 字节。
  Future<Uint8List> _renderSquarePng(ui.Image src, int size) async {
    final recorder = ui.PictureRecorder();
    final canvas = ui.Canvas(recorder);
    final side = size.toDouble();
    final scale = src.width > src.height ? side / src.width : side / src.height;
    final w = src.width * scale;
    final h = src.height * scale;
    canvas.drawImageRect(
      src,
      ui.Rect.fromLTWH(0, 0, src.width.toDouble(), src.height.toDouble()),
      ui.Rect.fromLTWH((side - w) / 2, (side - h) / 2, w, h),
      ui.Paint()..filterQuality = ui.FilterQuality.high,
    );
    final picture = recorder.endRecording();
    final image = await picture.toImage(size, size);
    picture.dispose();
    try {
      final data = await image.toByteData(format: ui.ImageByteFormat.png);
      if (data == null) {
        throw StateError('PNG encode returned null for size $size');
      }
      return data.buffer.asUint8List(data.offsetInBytes, data.lengthInBytes);
    } finally {
      image.dispose();
    }
  }
}
