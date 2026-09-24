// Doctor 诊断视图 —— 设置页 `Doctor` 分类的内容页。
//
// 一次 rinf 往返（`RunDiagnostics` → `DiagnosticsReport`）拿到全部检查项；
// 「复制报告」把环境信息 + 检查结论 + NMH 中继日志 + 当日 App 日志尾部拼成
// 纯文本，便于用户直接贴进 issue。
//
// 检查项标题 / 修复建议一律按 wire id 查 i18n（见 `S.doctorCheckLabel` /
// `S.doctorHintLabel`），未知 id 原样回显 wire 串 —— Rust 侧新增检查项时
// 这里既不崩也不留白，UI 无需同步改动即可先把新结论展示出来。

import 'dart:async';

import 'package:flutter/services.dart' show Clipboard, ClipboardData;
import 'package:flutter/widgets.dart';
import 'package:rinf/rinf.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../bindings/bindings.dart';
import '../i18n/locale_provider.dart';
import '../models/settings_provider.dart';
import '../services/log_service.dart';
import '../services/open_folder.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'flux_sonner.dart';
import 'webhook_delivery_panel.dart' show WebhookSpinner;

/// 复制报告时附带的当日 App 日志行数上限（够定位问题，又不至于塞爆剪贴板）。
const _kAppLogTailLines = 200;

/// 日志文本框高度上限，超出部分内部滚动。
const _kLogBoxMaxHeight = 220.0;

/// NMH 注册相关的 check id —— 这几项的修复动作都是「重写 NMH 注册」。
const _kNmhCheckIds = {'nmh_binary', 'nmh_manifest', 'nmh_browser'};

/// 改注册表 / 改文件关联的动作只在 Rust 侧写完后回一条 status 信号，没有
/// 「配置已生效」的 ack。这些动作触发后等这个时长再自动重跑诊断。
const _kReRunDelay = Duration(milliseconds: 900);

/// 诊断转圈的最小可见时长。本机探针通常几毫秒就回来，不撑住这段时间的话
/// loading 根本来不及被看见，点按钮等于毫无反馈。
const _kMinSpinDuration = Duration(milliseconds: 550);

/// 等宽小字：路径 / 注册表值 / 日志等技术细节统一用它，避免长路径视觉抖动。
TextStyle _monoStyle(Color color, {double fontSize = 11}) => TextStyle(
  fontFamily: 'monospace',
  fontSize: fontSize,
  height: 1.45,
  color: color,
);

class DoctorReportView extends StatefulWidget {
  final SettingsProvider settingsProvider;

  const DoctorReportView({super.key, required this.settingsProvider});

  @override
  State<DoctorReportView> createState() => _DoctorReportViewState();
}

class _DoctorReportViewState extends State<DoctorReportView> {
  StreamSubscription<RustSignalPack<DiagnosticsReport>>? _reportSub;
  StreamSubscription<RustSignalPack<NmhRepairResult>>? _repairSub;
  StreamSubscription<RustSignalPack<NativeListenerRestartResult>>? _listenerSub;

  DiagnosticsReport? _report;
  DateTime? _lastRun;
  bool _running = false;

  /// 本轮诊断的发起时刻，用来兑现 [_kMinSpinDuration]。
  DateTime? _runStartedAt;

  /// 下一份报告到手时是否播报结果（只有用户主动点按钮才置位）。
  bool _announceNextReport = false;

  /// 正在执行修复动作的行（`id:target`）；同一行的按钮期间禁用。
  String? _busyRow;

  @override
  void initState() {
    super.initState();
    _reportSub = DiagnosticsReport.rustSignalStream.listen(_onReport);
    _repairSub = NmhRepairResult.rustSignalStream.listen(_onRepairResult);
    _listenerSub = NativeListenerRestartResult.rustSignalStream.listen(
      _onListenerRestart,
    );
    // 打开这一页的人已经在排查故障了，先跑一遍再问——探针都是本机的
    // 注册表读取 + 回环 ping，秒级完成。
    _run();
  }

  @override
  void dispose() {
    _reportSub?.cancel();
    _repairSub?.cancel();
    _listenerSub?.cancel();
    super.dispose();
  }

  /// 报告到手。探针全在本机，实测 3ms 就回来了——直接收尾会让转圈一闪而过，
  /// 用户根本分辨不出「跑完了」还是「按钮没响应」。所以补足最小可见时长。
  void _onReport(RustSignalPack<DiagnosticsReport> pack) {
    if (!mounted) return;
    final report = pack.message;
    final elapsed = _runStartedAt == null
        ? Duration.zero
        : DateTime.now().difference(_runStartedAt!);
    final remaining = _kMinSpinDuration - elapsed;
    if (remaining <= Duration.zero) {
      _applyReport(report);
      return;
    }
    Future<void>.delayed(remaining, () {
      if (!mounted) return;
      _applyReport(report);
    });
  }

  void _applyReport(DiagnosticsReport report) {
    final announce = _announceNextReport;
    setState(() {
      _report = report;
      _running = false;
      _lastRun = DateTime.now();
      _announceNextReport = false;
    });
    if (!announce) return;
    // 只有用户主动点「运行诊断」才播报：进页面的自动首跑弹 toast 是噪音，
    // 修复动作也已经各自弹过结果了。
    final s = LocaleScope.of(context);
    final errors = report.checks.where((e) => e.level == 'error').length;
    final issues =
        errors + report.checks.where((e) => e.level == 'warn').length;
    if (issues == 0) {
      FluxSonner.of(context).show(
        ShadToast(
          title: Text(s.doctorRunDoneHealthy),
          duration: const Duration(seconds: 2),
        ),
      );
      return;
    }
    final title = Text(s.doctorRunDoneIssues(issues));
    FluxSonner.of(context).show(
      errors > 0
          ? ShadToast.destructive(title: title)
          : ShadToast(title: title, duration: const Duration(seconds: 3)),
    );
  }

  void _onRepairResult(RustSignalPack<NmhRepairResult> pack) {
    final msg = pack.message;
    _finishAction(
      ok: msg.ok,
      error: msg.error,
      okTitle: (s) => s.doctorRepairOk,
      failTitle: (s) => s.doctorRepairFailed(msg.error),
    );
  }

  void _onListenerRestart(RustSignalPack<NativeListenerRestartResult> pack) {
    final msg = pack.message;
    _finishAction(
      ok: msg.ok,
      error: msg.error,
      okTitle: (s) => s.doctorListenerRestartOk,
      failTitle: (s) => s.doctorListenerRestartFailed(msg.error),
    );
  }

  /// 修复动作收尾：清 busy 态、弹结果、成功后重跑诊断给出确定性结论。
  void _finishAction({
    required bool ok,
    required String error,
    required String Function(S) okTitle,
    required String Function(S) failTitle,
  }) {
    if (!mounted) return;
    setState(() => _busyRow = null);
    final s = LocaleScope.of(context);
    if (ok) {
      FluxSonner.of(context).show(
        ShadToast(
          title: Text(okTitle(s)),
          duration: const Duration(seconds: 2),
        ),
      );
      _run();
      return;
    }
    FluxSonner.of(context).show(
      ShadToast.destructive(title: Text(failTitle(s))),
    );
  }

  void _run({bool announce = false}) {
    if (_running) return;
    setState(() {
      _running = true;
      _runStartedAt = DateTime.now();
      _announceNextReport = announce;
    });
    final sp = widget.settingsProvider;
    RunDiagnostics(
      localServerPort: sp.localServerPort,
      localServerEnabled: sp.localServerEnabled,
    ).sendSignalToRust();
  }

  /// 触发一个「发出去就没有回执」的修复动作：定时重跑诊断来确认结果。
  ///
  /// 重跑带播报——这类动作（注册协议 / 开服务）本身没有结果信号，重跑后的
  /// 那条 toast 就是用户唯一的「到底成了没有」。
  void _fireAndRecheck(String rowKey, VoidCallback action) {
    setState(() => _busyRow = rowKey);
    action();
    Future<void>.delayed(_kReRunDelay, () {
      if (!mounted) return;
      setState(() => _busyRow = null);
      _run(announce: true);
    });
  }

  void _openLogDir() {
    // 统一走 Rust open 动词链路（openFolder → reveal_file.rs），不再硬编码
    // explorer，默认文件管理器（含第三方）由系统 open 关联解析。
    openFolder(LogService.instance.logDir.path);
  }

  /// 这一条检查项能就地做什么。`null` = 没有可用动作（正常项 / 只能重装）。
  ///
  /// 覆盖用户视角的全部「没注册 / 没起来」场景：NMH 注册被清掉、本机监听端点
  /// 被拆、本地服务没开、协议与 `.torrent` 关联没拿到手。日志目录恒给「打开
  /// 目录」，方便手工翻日志。
  _RowAction? _actionFor(S s, DiagnosticCheck check) {
    final sp = widget.settingsProvider;
    final key = '${check.id}:${check.target}';
    if (check.id == 'log_dir') {
      return _RowAction(key: key, label: s.doctorActionOpenLogDir, run: _openLogDir);
    }
    if (check.level == 'ok') return null;
    if (_kNmhCheckIds.contains(check.id)) {
      return _RowAction(
        key: key,
        label: s.doctorActionReregister,
        run: () {
          setState(() => _busyRow = key);
          RepairNmhRegistration().sendSignalToRust();
        },
      );
    }
    switch (check.id) {
      case 'app_listener':
        return _RowAction(
          key: key,
          label: s.doctorActionRestartListener,
          run: () {
            setState(() => _busyRow = key);
            RestartNativeListener().sendSignalToRust();
          },
        );
      case 'local_server':
        return _RowAction(
          key: key,
          label: s.doctorActionEnableService,
          run: () =>
              _fireAndRecheck(key, () => sp.setLocalServerEnabled(true)),
        );
      case 'torrent_association':
        return _RowAction(
          key: key,
          label: s.doctorActionRegister,
          run: () => _fireAndRecheck(key, () => sp.setFileAssociation(true)),
        );
      case 'url_protocol':
        // `magnet`/`ed2k` 经 provider：那两个开关还带 `*_assoc_user_disabled`
        // 退出标记，绕过 provider 直发信号会让设置页和现实对不上。
        // `fluxdown` 没有用户开关（启动自动注册），直发信号即可。
        final VoidCallback? register = switch (check.target) {
          'magnet' => () => sp.setMagnetProtocolAssociation(true),
          'ed2k' => () => sp.setEd2kProtocolAssociation(true),
          'fluxdown' => () =>
              SetUrlProtocol(scheme: 'fluxdown', enable: true).sendSignalToRust(),
          _ => null,
        };
        if (register == null) return null;
        return _RowAction(
          key: key,
          label: s.doctorActionRegister,
          run: () => _fireAndRecheck(key, register),
        );
      default:
        return null;
    }
  }

  /// 当日 App 日志尾部（最多 [_kAppLogTailLines] 行）。
  Future<String> _appLogTail() async {
    final raw = await LogService.instance.readTodayLog();
    if (raw.isEmpty) return '';
    final lines = raw.split('\n');
    if (lines.length <= _kAppLogTailLines) return raw;
    return lines.sublist(lines.length - _kAppLogTailLines).join('\n');
  }

  /// 拼纯文本报告：等级前缀用大写 wire 值，标题走当前语言，细节原样。
  String _buildReportText(S s, DiagnosticsReport report, String appLog) {
    final b = StringBuffer('FluxDown Doctor Report\n---\n');
    for (final line in report.environment) {
      b.writeln(line);
    }
    b.writeln('---');
    for (final check in report.checks) {
      final target = check.target.isEmpty ? '' : ' · ${check.target}';
      b.writeln(
        '[${check.level.toUpperCase()}] ${s.doctorCheckLabel(check.id)}$target',
      );
      if (check.detail.isNotEmpty) {
        for (final line in check.detail.split('\n')) {
          b.writeln('    $line');
        }
      }
      if (check.hint.isNotEmpty) {
        b.writeln('    hint: ${s.doctorHintLabel(check.hint)}');
      }
    }
    b
      ..writeln('---')
      ..writeln('NMH relay log:')
      ..writeln(report.nmhLogTail)
      ..writeln('---')
      ..writeln('App log (today, tail):')
      ..writeln(appLog);
    return b.toString();
  }

  Future<void> _copyReport() async {
    final s = LocaleScope.of(context);
    final report = _report;
    if (report == null) return;
    final appLog = await _appLogTail();
    if (!mounted) return;
    await Clipboard.setData(
      ClipboardData(text: _buildReportText(s, report, appLog)),
    );
    if (!mounted) return;
    FluxSonner.of(context).show(
      ShadToast(
        title: Text(s.doctorCopied),
        duration: const Duration(seconds: 2),
      ),
    );
  }

  static String _formatTime(DateTime t) {
    final h = t.hour.toString().padLeft(2, '0');
    final m = t.minute.toString().padLeft(2, '0');
    final sec = t.second.toString().padLeft(2, '0');
    return '$h:$m:$sec';
  }

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    final report = _report;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _buildToolbar(s, c),
        if (report != null)
          // 重跑期间把旧结果压暗：明确表示「这份是上一轮的，正在刷新」，
          // 而不是让人盯着不变的列表猜按钮到底响应了没有。
          AnimatedOpacity(
            opacity: _running ? 0.4 : 1,
            duration: const Duration(milliseconds: 140),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                const SizedBox(height: 14),
                _buildSummary(s, c, report),
                const SizedBox(height: 10),
                // checks 为空只可能是 Rust 侧异常；此时别留一条孤零零的空描边。
                if (report.checks.isNotEmpty)
                  _Panel(
                    children: [
                      // 按下标判末行：DiagnosticCheck 值相等，同一结论可能重复出现。
                      for (var i = 0; i < report.checks.length; i++)
                        _CheckRow(
                          check: report.checks[i],
                          isLast: i == report.checks.length - 1,
                          action: _actionFor(s, report.checks[i]),
                          busyKey: _busyRow,
                        ),
                    ],
                  ),
                const SizedBox(height: 16),
                _buildSectionTitle(c, s.doctorEnvTitle),
                _Panel(
                  children: [
                    Padding(
                      padding: const EdgeInsets.symmetric(
                        horizontal: 14,
                        vertical: 10,
                      ),
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          for (final line in report.environment)
                            Text(line, style: _monoStyle(c.textSecondary)),
                        ],
                      ),
                    ),
                  ],
                ),
                const SizedBox(height: 16),
                _buildSectionTitle(c, s.doctorNmhLogTitle),
                _Panel(children: [_buildNmhLog(s, c, report.nmhLogTail)]),
              ],
            ),
          ),
      ],
    );
  }

  /// 顶部一行：运行诊断 / 上次运行时间 / 复制报告。
  Widget _buildToolbar(S s, AppColors c) {
    return Row(
      children: [
        ShadButton(
          size: ShadButtonSize.sm,
          enabled: !_running,
          onPressed: () => _run(announce: true),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              if (_running) ...[
                WebhookSpinner(color: c.accentForeground),
                const SizedBox(width: 6),
                Text(s.doctorRunning),
              ] else ...[
                Icon(
                  LucideIcons.stethoscope,
                  size: 13,
                  color: c.accentForeground,
                ),
                const SizedBox(width: 6),
                Text(s.doctorRun),
              ],
            ],
          ),
        ),
        const SizedBox(width: 10),
        Expanded(
          child: Text(
            _lastRun == null
                ? s.doctorNeverRun
                : s.doctorLastRun(_formatTime(_lastRun!)),
            style: TextStyle(fontSize: 11.5, color: c.textMuted),
            overflow: TextOverflow.ellipsis,
          ),
        ),
        ShadButton.outline(
          size: ShadButtonSize.sm,
          enabled: _report != null,
          onPressed: _copyReport,
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(
                LucideIcons.clipboardCopy,
                size: 13,
                color: c.textSecondary,
              ),
              const SizedBox(width: 6),
              Text(s.doctorCopyReport),
            ],
          ),
        ),
      ],
    );
  }

  /// 汇总行：全绿一句话，否则给出问题条数（修复入口在各行内，不在这里）。
  Widget _buildSummary(S s, AppColors c, DiagnosticsReport report) {
    final errors = report.checks.where((e) => e.level == 'error').length;
    final warnings = report.checks.where((e) => e.level == 'warn').length;
    final issues = errors + warnings;
    final healthy = issues == 0;
    final tint = healthy
        ? c.statusSuccess
        : (errors > 0 ? c.statusError : c.statusWarning);
    return Row(
      children: [
        Icon(
          healthy ? LucideIcons.circleCheck : LucideIcons.triangleAlert,
          size: 15,
          color: tint,
        ),
        const SizedBox(width: 7),
        Expanded(
          child: Text(
            healthy ? s.doctorAllHealthy : s.doctorIssuesFound(issues),
            style: TextStyle(
              fontSize: 12.5,
              fontWeight: FontWeight.w600,
              color: tint,
            ),
          ),
        ),
      ],
    );
  }

  Widget _buildSectionTitle(AppColors c, String title) => Padding(
    padding: const EdgeInsets.only(left: 4, bottom: 6),
    child: Text(
      title,
      style: TextStyle(
        fontSize: 12.5,
        fontWeight: FontWeight.w600,
        color: c.textSecondary,
      ),
    ),
  );

  Widget _buildNmhLog(S s, AppColors c, String tail) {
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
      child: tail.isEmpty
          ? Text(
              s.doctorNmhLogEmpty,
              style: TextStyle(fontSize: 11.5, color: c.textMuted),
            )
          : ConstrainedBox(
              constraints: const BoxConstraints(maxHeight: _kLogBoxMaxHeight),
              child: SingleChildScrollView(
                child: Text(tail, style: _monoStyle(c.textSecondary)),
              ),
            ),
    );
  }
}

/// 卡片容器：与设置页分组同一视觉语言（面板底 + 中等描边 + 发丝线分隔）。
class _Panel extends StatelessWidget {
  final List<Widget> children;

  const _Panel({required this.children});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return Container(
      clipBehavior: Clip.antiAlias,
      decoration: BoxDecoration(
        color: c.surface1,
        borderRadius: m.brDialog,
        border: Border.all(color: m.borderMedium(c.border), width: 1),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: children,
      ),
    );
  }
}

/// 单条检查项：等级图标 + 标题（`target` 非空时追加）+ 技术细节 + 修复建议。
class _CheckRow extends StatelessWidget {
  final DiagnosticCheck check;
  final bool isLast;

  /// 这一行能就地执行的修复动作；`null` = 无动作。
  final _RowAction? action;

  /// 当前正在执行动作的行 key（`null` = 空闲）。非空时禁用所有行内按钮避免
  /// 连点叠加，只有 key 相符的那一行转圈。
  final String? busyKey;

  const _CheckRow({
    required this.check,
    required this.isLast,
    required this.action,
    required this.busyKey,
  });

  /// 等级 wire → （色, 图标）；未知等级按 `info` 处理（灰点，不误报故障）。
  static (Color, IconData) _visual(String level, AppColors c) =>
      switch (level) {
        'ok' => (c.statusSuccess, LucideIcons.circleCheck),
        'warn' => (c.statusWarning, LucideIcons.triangleAlert),
        'error' => (c.statusError, LucideIcons.circleX),
        _ => (c.textMuted, LucideIcons.info),
      };

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final (tint, icon) = _visual(check.level, c);
    final title = check.target.isEmpty
        ? s.doctorCheckLabel(check.id)
        : '${s.doctorCheckLabel(check.id)} · ${check.target}';
    return Container(
      decoration: BoxDecoration(
        border: isLast
            ? null
            : Border(bottom: BorderSide(color: m.borderFade(c.border))),
      ),
      padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 9),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          // 整行只有这一个等级标记：图标形状 + 颜色表达等级，尺寸与标题同高。
          // 等级文字不再在行尾并排一小块灰字，只留给读屏与「复制报告」纯文本。
          Padding(
            padding: const EdgeInsets.only(top: 1),
            child: Icon(
              icon,
              size: 15,
              color: tint,
              semanticLabel: s.doctorLevelLabel(check.level),
            ),
          ),
          const SizedBox(width: 9),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  title,
                  style: TextStyle(
                    fontSize: 12.5,
                    fontWeight: FontWeight.w500,
                    color: c.textPrimary,
                  ),
                ),
                if (check.detail.isNotEmpty) ...[
                  const SizedBox(height: 3),
                  // 长路径 / 注册表值：等宽换行，不裁剪、不横向溢出。
                  Text(
                    check.detail,
                    style: _monoStyle(c.textMuted, fontSize: 10.5),
                  ),
                ],
                if (check.hint.isNotEmpty) ...[
                  const SizedBox(height: 4),
                  Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Icon(LucideIcons.info, size: 11, color: tint),
                      const SizedBox(width: 5),
                      Expanded(
                        child: Text(
                          s.doctorHintLabel(check.hint),
                          style: TextStyle(
                            fontSize: 11,
                            height: 1.4,
                            color: tint,
                          ),
                        ),
                      ),
                    ],
                  ),
                ],
              ],
            ),
          ),
          const SizedBox(width: 10),
          if (action != null) _ActionButton(action: action!, busyKey: busyKey),
        ],
      ),
    );
  }
}

// 行内转圈复用 `webhook_delivery_panel.dart` 的 `WebhookSpinner`：同一套
// 「按钮点下去必须立刻有动静」的诉求，不再平行造第二个。

/// 一条检查项的行内修复动作。
class _RowAction {
  /// `id:target`，用于标识发起动作的行。
  final String key;
  final String label;
  final VoidCallback run;

  const _RowAction({
    required this.key,
    required this.label,
    required this.run,
  });
}

/// 行内修复按钮：本行在执行时转圈，其它行在任何动作执行期间一律禁用
/// ——修注册表 / 重绑端点这类动作并发跑只会互相打架。
class _ActionButton extends StatelessWidget {
  final _RowAction action;
  final String? busyKey;

  const _ActionButton({required this.action, required this.busyKey});

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final running = busyKey == action.key;
    return ShadButton.ghost(
      size: ShadButtonSize.sm,
      enabled: busyKey == null,
      onPressed: action.run,
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (running)
            WebhookSpinner(color: c.textSecondary)
          else
            Icon(LucideIcons.wrench, size: 12, color: c.textSecondary),
          const SizedBox(width: 5),
          Text(action.label),
        ],
      ),
    );
  }
}
