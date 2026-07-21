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

import '../bindings/bindings.dart' show ResolvePreviewResult;
import '../i18n/locale_provider.dart';
import '../models/download_queue.dart' show kMainQueueId;
import '../services/file_picker_service.dart';
import '../services/resolve_preview_client.dart'
    show isManifestPreviewableUrl;
import '../theme/app_colors.dart';
import '../theme/app_theme.dart';
import '../theme/flux_theme_tokens.dart';
import '../widgets/flux_sonner.dart';
import '../widgets/manifest_select_view.dart';
import '../widgets/quick_download_form.dart';
import 'popup_payload.dart';

/// onRelay 消息总线：QuickPopupApp 收原生 `onRelay` 后转交当前挂载的
/// [_PopupShellState]（生命周期挂接方式同 [QuickDownloadFormController]）。
class _PopupRelayBus {
  void Function(PopupRelayMessage msg)? handler;
}

/// 弹窗引擎与原生宿主的通道（原生侧注册在弹窗引擎 messenger 上）
const _popupChannel = MethodChannel('fluxdown/popup_child');

/// 弹窗引擎入口 — 由 main() 在检测到 `--quick-popup` 参数时调用。
/// rootBundle 资产读取不经插件通道，不违反零插件契约。
Future<void> runQuickPopupApp() async {
  WidgetsFlutterBinding.ensureInitialized();
  await I18nStore.load();
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

  /// relay 消息总线 — 清单预解析流程的主引擎回递通道。
  final _relayBus = _PopupRelayBus();

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
        // 表单（不重置 epoch、不动用户已填字段；清单视图态下表单在
        // Offstage 中同样接收——用户返回表单后可见，请求不丢失）。返回
        // 追加条数供原生侧日志/诊断用。
        return _formController.appendUrls(call.arguments as String);
      case 'onRelay':
        _relayBus.handler?.call(
          PopupRelayMessage.fromJsonString(call.arguments as String),
        );
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
                // FluxSonner（非 ShadSonner）：表单/清单视图的错误提示统一走
                // FluxSonner.of，与主窗口根（main.dart）同款挂载。
                child: FluxSonner(
                  child: ExcludeSemantics(
                    child: WidgetsApp(
                      color: theme.colorScheme.primary,
                      debugShowCheckedModeBanner: false,
                      home: _PopupShell(
                        key: ValueKey(_epoch),
                        payload: payload,
                        formController: _formController,
                        relayBus: _relayBus,
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
  final _PopupRelayBus relayBus;

  const _PopupShell({
    super.key,
    required this.payload,
    required this.formController,
    required this.relayBus,
  });

  @override
  State<_PopupShell> createState() => _PopupShellState();
}

class _PopupShellState extends State<_PopupShell> {
  final _contentKey = GlobalKey();
  final _titleKey = GlobalKey();
  double _lastSentHeight = 0;
  double _lastSentWidth = _kFormWindowWidth;

  /// 窗口高度上限（逻辑像素）。内容超过后窗口不再增高，
  /// 由内部 SingleChildScrollView 滚动兜底——避免展开高级选项时
  /// 窗口大幅跳变（原生 SetWindowPos 单帧巨量重排导致掉帧）。
  static const double _kMaxWindowHeight = 640;

  /// 表单态窗口逻辑宽度（原生宿主 kLogicalWidth 契约值）。
  static const double _kFormWindowWidth = 520;

  /// 清单选择视图态的窗口逻辑尺寸：主体对齐主窗口清单对话框的 780×620，
  /// 高度另加本窗自绘标题栏实测高。
  static const double _kManifestWindowWidth = 780;
  static const double _kManifestBodyHeight = 620;

  /// 是否已发送 reveal（每个载荷代次恰好一次；本 State 随 epoch 重建）。
  bool _revealed = false;

  // ── 清单预解析流程状态（操作逻辑对齐 new_download_dialog）────────────────

  /// 等待预解析结果中的表单提交快照（非 null = resolving 态：表单动作区
  /// 换「取消 + spinner」，主引擎在做 ResolvePreview）。
  QuickDownloadFormResult? _pendingPreview;

  /// 当前预解析尝试的 relay seq（每次发起递增；迟到 previewResult 按
  /// seq 判弃——取消后重新提交不会被旧结果污染）。
  int _relaySeq = 0;

  /// 命中清单后的视图数据（非 null = 清单选择视图态；表单保留在
  /// Offstage 中，取消返回时用户编辑不丢失）。
  ResolvePreviewResult? _manifest;

  /// 清单对应的源链接与表单快照（清单视图初值：目录/队列/线程/高级选项）。
  String _manifestSourceUrl = '';
  QuickDownloadFormResult? _manifestForm;

  @override
  void initState() {
    super.initState();
    widget.relayBus.handler = _onRelayMessage;
    WidgetsBinding.instance.addPostFrameCallback((_) => _requestResize());
  }

  @override
  void dispose() {
    // 实例方法 tear-off 相等性：同对象同方法 == 成立；epoch 切换时新
    // Shell 可能已接管 handler，勿误清。
    if (widget.relayBus.handler == _onRelayMessage) {
      widget.relayBus.handler = null;
    }
    super.dispose();
  }

  /// 测量标题栏 + 表单内容的自然总高并上报原生宿主。
  ///
  /// 首次（新载荷首帧）走 `reveal`：原生一次到位设高并显示窗口；
  /// 此后走 `resize`：内容在 AnimatedSize 中渐变，本方法随动画每帧触发，
  /// 原生窗口以小步长平滑跟随。高度均经 [_kMaxWindowHeight] 截断。
  /// 清单视图态由 [_requestManifestResize] 负责固定尺寸，本方法直接返回
  /// （Offstage 中的表单仍在布局，不得用其尺寸驱动窗口）。
  void _requestResize() {
    if (!mounted || _manifest != null) return;
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
    // 从清单视图返回表单后宽度需归位（needWidthRestore 强制重发一次）。
    final needWidthRestore = _lastSentWidth != _kFormWindowWidth;
    if (!needWidthRestore && (target - _lastSentHeight).abs() < 1.0) return;
    _lastSentHeight = target;
    final args = <String, double>{'height': target};
    if (needWidthRestore) {
      _lastSentWidth = _kFormWindowWidth;
      args['width'] = _kFormWindowWidth;
    }
    _popupChannel.invokeMethod<void>('resize', args).catchError((_) {});
  }

  /// 切入清单视图：窗口一次到位放大到 780 ×（标题栏 + 620）。
  void _requestManifestResize() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted || _manifest == null) return;
      final titleHeight = _titleKey.currentContext?.size?.height ?? 44;
      _lastSentWidth = _kManifestWindowWidth;
      _lastSentHeight = titleHeight + _kManifestBodyHeight;
      _popupChannel
          .invokeMethod<void>('resize', {
            'width': _kManifestWindowWidth,
            'height': _lastSentHeight,
          })
          .catchError((_) {});
    });
  }

  void _cancel() {
    _popupChannel.invokeMethod<void>('cancel').catchError((_) {});
  }

  /// 普通提交路径（多行/非 http(s)/预解析回退）：中继结果回主引擎，
  /// 原生宿主先藏窗再转发（既有 submit 语义，零差异）。
  void _submitPlain(QuickDownloadFormResult result) {
    final res = QuickPopupResult(
      requestId: widget.payload.requestId,
      form: result,
    );
    _popupChannel
        .invokeMethod<void>('submit', res.toJsonString())
        .catchError((_) {});
  }

  /// 表单提交入口：单条 http(s) 链接先经主引擎探测多文件清单
  /// （门控与 new_download_dialog 一致），其余路径零差异走普通提交。
  void _submit(QuickDownloadFormResult result) {
    final entries = parseQuickDownloadEntries(result.urlText);
    if (entries.length == 1 && isManifestPreviewableUrl(entries.first.url)) {
      _beginPreview(result);
      return;
    }
    _submitPlain(result);
  }

  void _beginPreview(QuickDownloadFormResult result) {
    final seq = ++_relaySeq;
    setState(() => _pendingPreview = result);
    _popupChannel
        .invokeMethod<void>(
          'relay',
          encodePreviewRequest(
            requestId: widget.payload.requestId,
            seq: seq,
            form: result,
          ).toJsonString(),
        )
        .catchError((_) {
          // 原生宿主无 relay（旧版）：零差异回退普通提交。
          if (!mounted || !identical(_pendingPreview, result)) return;
          setState(() => _pendingPreview = null);
          _submitPlain(result);
        });
    // 无清单/超时由主引擎代提交并关窗；命中清单经 onRelay 回递切视图。
    // 主引擎的 ResolvePreviewClient 自带 90s 超时，弹窗侧无需再计时。
  }

  /// resolving 态点了取消：立即恢复表单可编辑，并中继取消让主引擎
  /// 中止预解析（迟到结果两侧都会按 seq/pending 判弃）。
  void _cancelResolve() {
    if (_pendingPreview == null) return;
    final seq = _relaySeq;
    setState(() => _pendingPreview = null);
    _popupChannel
        .invokeMethod<void>(
          'relay',
          encodePreviewCancel(
            requestId: widget.payload.requestId,
            seq: seq,
          ).toJsonString(),
        )
        .catchError((_) {});
  }

  void _onRelayMessage(PopupRelayMessage msg) {
    if (!mounted) return;
    if (msg.requestId != widget.payload.requestId) return;
    if (msg.kind != kPopupRelayPreviewResult) return;
    if (msg.seq != _relaySeq) return; // 迟到（已取消/新一轮尝试）
    final pending = _pendingPreview;
    if (pending == null) return;
    final manifest = decodePreviewResultManifest(msg);
    if (manifest == null) {
      // 防御分支：无清单时主引擎通常直接代提交并关窗、不回递本消息；
      // 若协议演进为回递 null，则由弹窗走普通提交（两者互斥，不会双发）。
      setState(() => _pendingPreview = null);
      _submitPlain(pending);
      return;
    }
    setState(() {
      _pendingPreview = null;
      _manifest = manifest;
      _manifestSourceUrl = manifest.sourceUrl;
      _manifestForm = pending;
    });
    _requestManifestResize();
  }

  /// 清单视图取消/Esc/关闭：回到表单视图（Offstage 中的表单原样保留，
  /// 与主窗口「清单取消 → 回表单可重新编辑提交」同语义）。幂等。
  void _exitManifest() {
    if (_manifest == null) return;
    setState(() {
      _manifest = null;
      _manifestSourceUrl = '';
      _manifestForm = null;
    });
    // 通知主引擎清单视图已退出：恢复 append 合入表单语义并冲刷托管缓冲
    // 的外部请求（fire-and-forget；消息丢失时缓冲滞留到会话结束再冲刷为
    // 新会话——请求不丢，只是少了实时合并，见 popup_window_service）。
    _popupChannel
        .invokeMethod<void>(
          'relay',
          encodeManifestClosed(
            requestId: widget.payload.requestId,
            seq: _relaySeq,
          ).toJsonString(),
        )
        .catchError((_) {});
    // 表单尺寸无变化不会触发 SizeChangedLayoutNotifier，主动重测一次
    // （_lastSentWidth 仍是 780 → 强制带 width 归位）。
    WidgetsBinding.instance.addPostFrameCallback((_) => _requestResize());
  }

  /// 清单视图确认：建组投影中继回主引擎（referrer 由主引擎回填），
  /// 主引擎发 CreateTaskGroup 后会 close 本窗。此处不自行 cancel——那会
  /// 触发 onClosed 清掉 pending，groupSubmit 到达时被误判 stale。
  void _onManifestConfirm(ManifestGroupSubmission sub) {
    _popupChannel
        .invokeMethod<void>(
          'relay',
          encodeGroupSubmit(
            requestId: widget.payload.requestId,
            seq: _relaySeq,
            sub: sub,
          ).toJsonString(),
        )
        .catchError((_) {});
  }

  void _startDrag() {
    _popupChannel.invokeMethod<void>('startDrag').catchError((_) {});
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);
    final manifest = _manifest;

    // Esc 逐层退出：清单视图 → 回表单；resolving → 取消等待；表单 → 关窗。
    // 绑定表按当前 build 快照选择处理器，处理器自身幂等（清单视图内的
    // KeyboardListener 同键触发时第二次调用是 no-op）。
    final escapeAction = manifest != null
        ? _exitManifest
        : _pendingPreview != null
        ? _cancelResolve
        : _cancel;

    return CallbackShortcuts(
      bindings: {const SingleActivator(LogicalKeyboardKey.escape): escapeAction},
      child: Focus(
        autofocus: true,
        child: Container(
          color: c.bg,
          // 标题栏固定在窗口顶部（关闭按钮不随内容滚动），
          // 仅表单主体滚动；窗口高度跟随内容，超上限后内部滚动兜底。
          // 清单视图态：表单子树转入 Offstage 保活（用户取消返回时编辑
          // 不丢失），清单视图独占余下空间，窗口固定 780×(标题+620)。
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
                child: Stack(
                  fit: StackFit.expand,
                  children: [
                    Offstage(
                      offstage: manifest != null,
                      child: _buildFormBody(c, s),
                    ),
                    if (manifest != null) _buildManifestView(manifest),
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  /// 表单主体（滚动 + 高度跟随），即原窗口唯一内容；清单视图态下转入
  /// Offstage 保活。
  Widget _buildFormBody(AppColors c, S s) {
    return SingleChildScrollView(
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
                    resolving: _pendingPreview != null,
                    onCancelResolve: _cancelResolve,
                    onSubmit: _submit,
                    onCancel: _cancel,
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }

  /// 清单选择视图（与主窗口清单对话框共用 ManifestSelectView）。
  /// 初值取自触发预解析的表单快照；确认经 relay groupSubmit 中继回主引擎，
  /// 取消回表单视图。
  Widget _buildManifestView(ResolvePreviewResult manifest) {
    final form = _manifestForm;
    final host = _PopupFormHost(widget.payload);
    return ManifestSelectView(
      // seq 作 key：每轮预解析命中都从全新选择状态开始（与主窗口每次
      // 弹新对话框同语义）。
      key: ValueKey('manifest-$_relaySeq'),
      queues: widget.payload.queues,
      manifest: manifest,
      sourceUrl: _manifestSourceUrl,
      initialSaveDir: form?.saveDir ?? widget.payload.saveDir,
      initialQueueId: (form == null || form.queueId.isEmpty)
          ? kMainQueueId
          : form.queueId,
      segments: form?.segments ?? 0,
      cookies: form?.cookies ?? widget.payload.cookies,
      referrer: '',
      userAgent: form?.userAgent ?? '',
      proxyUrl: form?.proxyUrl ?? '',
      extraHeaders: form?.extraHeaders ?? const {},
      ignoreTlsErrors: form?.ignoreTlsErrors ?? false,
      pickDirectory: host.pickDirectory,
      onConfirm: _onManifestConfirm,
      onCancel: _exitManifest,
      showCloseButton: false,
    );
  }

  /// 标题栏：图标徽章 + 标题 + 大小/MIME 标签 + 关闭按钮，整行可拖动窗口。
  /// 清单视图态下关闭按钮语义变为「返回表单」（对齐主窗口清单对话框的
  /// 右上角 X = 取消清单、底层表单保留）。
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
            onTap: _manifest != null ? _exitManifest : _cancel,
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
  List<QuickDeviceOption> get devices => payload.devices;

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
