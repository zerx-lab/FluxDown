import 'dart:async';

import 'package:flutter/widgets.dart';
import 'package:rinf/rinf.dart';

import '../bindings/bindings.dart';
import '../services/log_service.dart';
import '../widgets/resolve_variant_dialog.dart';

const _tag = 'ResolveVariantSvc';

class ResolveVariantService {
  static ResolveVariantService? _instance;

  final GlobalKey<NavigatorState> navigatorKey;
  StreamSubscription<RustSignalPack<ResolveVariantSelectionRequest>>? _sub;
  bool _dialogOpen = false;

  ResolveVariantService._({required this.navigatorKey});

  static void init({required GlobalKey<NavigatorState> navigatorKey}) {
    logInfo(_tag, 'init');
    _instance?._teardown();
    _instance = ResolveVariantService._(navigatorKey: navigatorKey);
    _instance!._startListening();
  }

  static void shutdown() {
    logInfo(_tag, 'shutdown');
    _instance?._teardown();
    _instance = null;
  }

  void _teardown() {
    _sub?.cancel();
  }

  void _startListening() {
    _sub = ResolveVariantSelectionRequest.rustSignalStream.listen(
      _onVariantRequest,
    );
  }

  void _onVariantRequest(
    RustSignalPack<ResolveVariantSelectionRequest> pack,
  ) {
    final msg = pack.message;
    logInfo(
      _tag,
      'received variant options: task=${msg.taskId}, count=${msg.options.length}',
    );

    if (_dialogOpen) {
      logInfo(_tag, 'dialog already open, ignoring');
      return;
    }

    final context = navigatorKey.currentContext;
    if (context == null) {
      logInfo(_tag, 'no context, auto-selecting default variant');
      _autoSelectDefault(msg);
      return;
    }

    if (!context.mounted) {
      logInfo(_tag, 'context not mounted, auto-selecting default variant');
      _autoSelectDefault(msg);
      return;
    }

    _dialogOpen = true;
    showResolveVariantDialog(
      context,
      taskId: msg.taskId,
      defaultIndex: msg.defaultIndex,
      options: msg.options,
    );
    Future.microtask(() {
      _dialogOpen = false;
    });
  }

  void _autoSelectDefault(ResolveVariantSelectionRequest msg) {
    if (msg.options.isEmpty) return;
    SelectResolveVariant(
      taskId: msg.taskId,
      selectedIndex: msg.defaultIndex,
    ).sendSignalToRust();
  }
}
