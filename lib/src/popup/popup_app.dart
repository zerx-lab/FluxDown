/// 外部唤起独立快速下载小窗 — 弹窗引擎 Dart 入口。
///
/// 原生宿主以 dart entrypoint 参数 `--quick-popup` 在**第二个 Flutter 引擎**
/// 中运行 `main()`，由 main() 分发到 [runQuickPopupApp]。
///
/// 本引擎的硬约束（契约见 popup-contract）：
/// - **零插件注册**：不得触碰 SharedPreferences / file_selector /
///   window_manager 等任何插件通道；
/// - **不初始化 Rust**：提交结果经 `fluxdown/popup_child` 原生通道
///   中继回主引擎，由主引擎发送下载信号；
/// - 主题令牌 / 语言 / 队列等环境数据全部由载荷 JSON 注入，
///   与主窗口共享同一套 FluxThemeTokens → ShadTheme 渲染管线，
///   UI 观感与应用内完全一致。
library;

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../i18n/locale_provider.dart';
import '../services/file_picker_service.dart';
import '../theme/app_colors.dart';
import '../theme/app_theme.dart';
import '../theme/flux_theme_tokens.dart';
import '../widgets/quick_download_form.dart';
import 'popup_payload.dart';

/// 弹窗引擎与原生宿主的通道（原生侧注册在弹窗引擎 messenger 上）
const _popupChannel = MethodChannel('fluxdown/popup_child');

/// 弹窗引擎入口 — 由 main() 在检测到 `--quick-popup` 参数时调用。
void runQuickPopupApp() {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(const QuickPopupApp());
}

/// 弹窗根组件：监听载荷投递，按载荷重建主题/语言/表单。
class QuickPopupApp extends StatefulWidget {
  const QuickPopupApp({super.key});

  @override
  State<QuickPopupApp> createState() => _QuickPopupAppState();
}

class _QuickPopupAppState extends State<QuickPopupApp> {
  QuickPopupPayload? _payload;

  /// 载荷代次 — 每次 setPayload 递增，作为表单子树的 Key 强制重置表单状态
  int _epoch = 0;

  /// 表单外部控制器 — appendPayload（小窗可见期间新请求合入表单）用。
  final _formController = QuickDownloadFormController();

  @override
  void initState() {
    super.initState();
    _popupChannel.setMethodCallHandler(_onCall);
    // 首帧就绪后通知原生宿主投递（可能暂存中的）载荷
    WidgetsBinding.instance.addPostFrameCallback((_) {
      _popupChannel.invokeMethod<void>('ready').catchError((_) {});
    });
  }

  Future<dynamic> _onCall(MethodCall call) async {
    switch (call.method) {
      case 'setPayload':
        final payload = QuickPopupPayload.fromJsonString(
          call.arguments as String,
        );
        // 同步全局 locale（currentS 供表单内无 context 场景使用）
        currentLocale = payload.locale;
        currentS = S.of(payload.locale);
        setState(() {
          _payload = payload;
          _epoch++;
        });
      case 'appendPayload':
        // append 模式：小窗可见期间新到的外部请求，把新 URL 合入当前
        // 表单（不重置 epoch、不动用户已填字段）。返回追加条数供原生侧
        // 日志/诊断用。
        return _formController.appendUrls(call.arguments as String);
    }
    return null;
  }

  @override
  Widget build(BuildContext context) {
    final payload = _payload;
    if (payload == null) {
      // 载荷到达前的空帧 — 中性深灰，避免引擎首帧白闪
      return const ColoredBox(color: Color(0xFF202020));
    }

    // 与 main.dart 主窗口根组件同构：手动组合 ShadTheme + WidgetsApp
    final tokens = FluxThemeTokens.fromJson(payload.tokensJson);
    final theme = buildThemeFromTokens(tokens);
    return LocaleScope(
      s: S.of(payload.locale),
      child: FluxThemeScope(
        tokens: tokens,
        child: ShadTheme(
          data: theme,
          child: Directionality(
            textDirection: TextDirection.ltr,
            child: DefaultTextStyle(
              style: theme.textTheme.p.copyWith(
                color: theme.colorScheme.foreground,
              ),
              child: ShadToaster(
                child: ShadSonner(
                  child: ExcludeSemantics(
                    child: WidgetsApp(
                      color: theme.colorScheme.primary,
                      debugShowCheckedModeBanner: false,
                      home: _PopupShell(
                        key: ValueKey(_epoch),
                        payload: payload,
                        formController: _formController,
                      ),
                      pageRouteBuilder:
                          <T>(RouteSettings settings, WidgetBuilder builder) {
                            return PageRouteBuilder<T>(
                              settings: settings,
                              pageBuilder: (context, _, _) => builder(context),
                            );
                          },
                    ),
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// 小窗外壳：自绘标题栏（拖动区 + 关闭按钮）+ 描述 + 快速下载表单。
///
/// 显示时序（reveal 握手）：原生宿主投递载荷后保持窗口隐藏，本组件在
/// 新载荷首帧布局完成后经 `reveal` 通道携带目标高度一次性请求
/// 「设高 + 显示」——消除复用小窗时旧表单闪现与默认高度→内容高度的
/// 二段跳。此后的内容高度变化（展开高级选项等）经 `resize` 通道跟随。
class _PopupShell extends StatefulWidget {
  final QuickPopupPayload payload;
  final QuickDownloadFormController formController;

  const _PopupShell({
    super.key,
    required this.payload,
    required this.formController,
  });

  @override
  State<_PopupShell> createState() => _PopupShellState();
}

class _PopupShellState extends State<_PopupShell> {
  final _contentKey = GlobalKey();
  final _titleKey = GlobalKey();
  double _lastSentHeight = 0;

  /// 窗口高度上限（逻辑像素）。内容超过后窗口不再增高，
  /// 由内部 SingleChildScrollView 滚动兜底——避免展开高级选项时
  /// 窗口大幅跳变（原生 SetWindowPos 单帧巨量重排导致掉帧）。
  static const double _kMaxWindowHeight = 640;

  /// 是否已发送 reveal（每个载荷代次恰好一次；本 State 随 epoch 重建）。
  bool _revealed = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) => _requestResize());
  }

  /// 测量标题栏 + 表单内容的自然总高并上报原生宿主。
  ///
  /// 首次（新载荷首帧）走 `reveal`：原生一次到位设高并显示窗口；
  /// 此后走 `resize`：内容在 AnimatedSize 中渐变，本方法随动画每帧触发，
  /// 原生窗口以小步长平滑跟随。高度均经 [_kMaxWindowHeight] 截断。
  void _requestResize() {
    if (!mounted) return;
    final bodySize = _contentKey.currentContext?.size;
    final titleSize = _titleKey.currentContext?.size;
    if (bodySize == null || titleSize == null) return;
    final natural = titleSize.height + bodySize.height;
    final target = natural > _kMaxWindowHeight ? _kMaxWindowHeight : natural;
    if (!_revealed) {
      _revealed = true;
      _lastSentHeight = target;
      _popupChannel.invokeMethod<void>('reveal', {'height': target}).catchError(
        (_) {
          // 旧版原生宿主无 reveal（NotImplemented）：其 show 流程会自行
          // 显示窗口，退化为既有行为——补发 resize 修正高度即可。
          _popupChannel
              .invokeMethod<void>('resize', {'height': target})
              .catchError((_) {});
        },
      );
      return;
    }
    if ((target - _lastSentHeight).abs() < 1.0) return;
    _lastSentHeight = target;
    _popupChannel
        .invokeMethod<void>('resize', {'height': target})
        .catchError((_) {});
  }

  void _cancel() {
    _popupChannel.invokeMethod<void>('cancel').catchError((_) {});
  }

  void _submit(QuickDownloadFormResult result) {
    final res = QuickPopupResult(
      requestId: widget.payload.requestId,
      form: result,
    );
    _popupChannel
        .invokeMethod<void>('submit', res.toJsonString())
        .catchError((_) {});
  }

  void _startDrag() {
    _popupChannel.invokeMethod<void>('startDrag').catchError((_) {});
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);

    return CallbackShortcuts(
      bindings: {const SingleActivator(LogicalKeyboardKey.escape): _cancel},
      child: Focus(
        autofocus: true,
        child: Container(
          color: c.bg,
          // 标题栏固定在窗口顶部（关闭按钮不随内容滚动），
          // 仅表单主体滚动；窗口高度跟随内容，超上限后内部滚动兜底。
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              // 标题栏右侧留白收窄（20→10）：让关闭按钮贴近窗口右上角
              Padding(
                key: _titleKey,
                padding: const EdgeInsets.fromLTRB(20, 12, 10, 0),
                child: _buildTitleBar(c, s),
              ),
              Expanded(
                child: SingleChildScrollView(
                  child: NotificationListener<SizeChangedLayoutNotification>(
                    onNotification: (_) {
                      WidgetsBinding.instance.addPostFrameCallback(
                        (_) => _requestResize(),
                      );
                      return true;
                    },
                    child: SizeChangedLayoutNotifier(
                      // 内容高度变化（展开高级选项 / 增删请求头行）经 AnimatedSize
                      // 渐变，SizeChangedLayoutNotifier 随动画逐帧上报，窗口高度
                      // 平滑跟随而非一次性跳变。
                      child: AnimatedSize(
                        duration: const Duration(milliseconds: 180),
                        curve: Curves.easeOutCubic,
                        alignment: Alignment.topCenter,
                        child: Padding(
                          key: _contentKey,
                          padding: const EdgeInsets.fromLTRB(20, 8, 20, 20),
                          child: Column(
                            mainAxisSize: MainAxisSize.min,
                            crossAxisAlignment: CrossAxisAlignment.stretch,
                            children: [
                              Text(
                                s.fromBrowserExtension,
                                style: TextStyle(
                                  fontSize: 13,
                                  color: c.textMuted,
                                ),
                              ),
                              const SizedBox(height: 16),
                              QuickDownloadForm(
                                initialUrl: widget.payload.url,
                                initialFileName: widget.payload.filename,
                                initialSaveDir: widget.payload.saveDir,
                                defaultQueueId: widget.payload.defaultQueueId,
                                initialCookies: widget.payload.cookies,
                                host: _PopupFormHost(widget.payload),
                                controller: widget.formController,
                                onSubmit: _submit,
                                onCancel: _cancel,
                              ),
                            ],
                          ),
                        ),
                      ),
                    ),
                  ),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  /// 标题栏：图标徽章 + 标题 + 大小/MIME 标签 + 关闭按钮，整行可拖动窗口。
  Widget _buildTitleBar(AppColors c, S s) {
    final payload = widget.payload;
    return GestureDetector(
      behavior: HitTestBehavior.translucent,
      onPanStart: (_) => _startDrag(),
      child: Row(
        children: [
          Container(
            width: 28,
            height: 28,
            decoration: BoxDecoration(
              color: c.accent.withValues(alpha: 0.1),
              borderRadius: BorderRadius.circular(6),
            ),
            child: Icon(LucideIcons.download, size: 14, color: c.accent),
          ),
          const SizedBox(width: 10),
          Text(
            s.newDownload,
            style: TextStyle(
              fontSize: 15,
              fontWeight: FontWeight.w600,
              color: c.textPrimary,
            ),
          ),
          const SizedBox(width: 8),
          if (payload.fileSize > 0)
            QuickInfoTag(
              text: formatQuickFileSize(
                payload.fileSize,
                unknownLabel: s.unknownSize,
              ),
              c: c,
            ),
          if (payload.fileSize > 0 && payload.mimeType.isNotEmpty)
            const SizedBox(width: 6),
          if (payload.mimeType.isNotEmpty)
            Flexible(
              child: QuickInfoTag(text: payload.mimeType, c: c),
            ),
          const Spacer(),
          ShadGestureDetector(
            cursor: SystemMouseCursors.click,
            onTap: _cancel,
            child: Container(
              width: 28,
              height: 28,
              alignment: Alignment.center,
              child: Icon(LucideIcons.x, size: 16, color: c.textMuted),
            ),
          ),
        ],
      ),
    );
  }
}

/// 表单宿主 — 环境数据来自载荷，目录选择经原生通道。
class _PopupFormHost implements QuickDownloadFormHost {
  final QuickPopupPayload payload;

  const _PopupFormHost(this.payload);

  @override
  List<QuickQueueOption> get queues => payload.queues;

  @override
  int get defaultSegments => payload.defaultSegments;

  @override
  String get lastDialogThreads => payload.lastDialogThreads;

  @override
  Future<String?> pickDirectory({
    required String dialogTitle,
    String? initialDirectory,
  }) async {
    try {
      return await _popupChannel.invokeMethod<String>('pickFolder', {
        'title': dialogTitle,
        'initialDir': initialDirectory ?? '',
      });
    } on PlatformException catch (e) {
      throw FilePickerException(
        FilePickerFailReason.nativeDialogFailed,
        cause: e,
      );
    } on MissingPluginException catch (e) {
      throw FilePickerException(FilePickerFailReason.noDialogTool, cause: e);
    }
  }
}
