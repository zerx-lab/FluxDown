// 「添加设备」弹窗 —— 双 Tab：账户自动（登录用户默认）/ 本地配对（未登录默认）。
//
// 设计依据：design/desktop-multi-device/DESIGN.md §6.5。
// - 账户自动：登录同一 FluxDown ID 的设备自动同步入册（复用已落地的云能力），
//   本弹窗仅作场景化入口 + 名册一览，不重建设置页的设备管理。
// - 本地配对：不登录账号，在同一局域网内直接配对（mDNS 发现 + 一次性配对码 +
//   SAS 核对），走 Rust 端 LinkManager；当前仅「网络可达直连」，未来可插拔
//   iroh/中继打洞（见 services/link/link_transport.dart）。

import 'package:flutter/material.dart';
import 'package:shadcn_ui/shadcn_ui.dart';

import '../i18n/locale_provider.dart';
import '../services/cloud/cloud_auth_service.dart';
import '../services/cloud/cloud_models.dart';
import '../services/link/link_models.dart';
import '../services/link/local_pairing_service.dart';
import '../theme/app_colors.dart';
import '../theme/app_metrics.dart';
import 'flux_sonner.dart';

/// 打开「添加设备」弹窗。
void showAddDeviceDialog(BuildContext context) {
  showShadDialog(context: context, builder: (_) => const AddDeviceDialog());
}

enum _AddDeviceTab { account, local }

/// 双 Tab 添加设备弹窗。默认页由登录态决定（登录→账户自动；未登录→本地配对）。
class AddDeviceDialog extends StatefulWidget {
  const AddDeviceDialog({super.key});

  @override
  State<AddDeviceDialog> createState() => _AddDeviceDialogState();
}

class _AddDeviceDialogState extends State<AddDeviceDialog> {
  late _AddDeviceTab _tab;
  final _codeCtrl = TextEditingController();
  final _hostCtrl = TextEditingController();
  final _portCtrl = TextEditingController(text: '17800');
  bool _manual = false;

  @override
  void initState() {
    super.initState();
    _tab = CloudAuthService.instance.isLoggedIn
        ? _AddDeviceTab.account
        : _AddDeviceTab.local;
    if (_tab == _AddDeviceTab.local) {
      // 进入本地配对页即开始局域网发现；退出（dispose）时停止。
      LocalPairingService.instance.startDiscovery();
    }
  }

  @override
  void dispose() {
    LocalPairingService.instance.stopDiscovery();
    _codeCtrl.dispose();
    _hostCtrl.dispose();
    _portCtrl.dispose();
    super.dispose();
  }

  void _switchTab(_AddDeviceTab tab) {
    if (_tab == tab) return;
    setState(() => _tab = tab);
    if (tab == _AddDeviceTab.local) {
      LocalPairingService.instance.startDiscovery();
    } else {
      LocalPairingService.instance.stopDiscovery();
    }
  }

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    return ShadDialog(
      title: Text(s.addDeviceEntry),
      actions: [
        ShadButton(
          onPressed: () => Navigator.of(context).pop(),
          child: Text(s.confirm),
        ),
      ],
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 460),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          mainAxisSize: MainAxisSize.min,
          children: [
            const SizedBox(height: 4),
            _segmented(s),
            const SizedBox(height: 14),
            if (_tab == _AddDeviceTab.account)
              _AccountTab()
            else
              _LocalTab(
                codeCtrl: _codeCtrl,
                hostCtrl: _hostCtrl,
                portCtrl: _portCtrl,
                manual: _manual,
                onToggleManual: () => setState(() => _manual = !_manual),
              ),
          ],
        ),
      ),
    );
  }

  Widget _segmented(S s) {
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    Widget seg(String label, _AddDeviceTab tab) {
      final active = _tab == tab;
      return Expanded(
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: () => _switchTab(tab),
          child: Container(
            height: 32,
            alignment: Alignment.center,
            decoration: BoxDecoration(
              color: active ? c.surface1 : Colors.transparent,
              borderRadius: m.brInput,
              border: active
                  ? Border.all(color: m.borderFade(c.border))
                  : null,
            ),
            child: Text(
              label,
              style: TextStyle(
                fontSize: 12.5,
                fontWeight: active ? FontWeight.w600 : FontWeight.w500,
                color: active ? c.textPrimary : c.textMuted,
              ),
            ),
          ),
        ),
      );
    }

    return Container(
      padding: const EdgeInsets.all(3),
      decoration: BoxDecoration(
        color: c.surface2,
        borderRadius: m.brInput,
      ),
      child: Row(
        children: [
          seg(s.addDeviceTabAccount, _AddDeviceTab.account),
          const SizedBox(width: 3),
          seg(s.addDeviceTabLocal, _AddDeviceTab.local),
        ],
      ),
    );
  }
}

// ─────────────────────────────────────────────────────────────────────────
// 账户自动 Tab
// ─────────────────────────────────────────────────────────────────────────

class _AccountTab extends StatelessWidget {
  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return ListenableBuilder(
      listenable: CloudAuthService.instance,
      builder: (context, _) {
        final auth = CloudAuthService.instance;
        final user = auth.user;
        if (user == null) {
          return Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                s.addDeviceHint,
                style: TextStyle(fontSize: 12.5, height: 1.5, color: c.textSecondary),
              ),
              const SizedBox(height: 12),
              Text(
                s.addDeviceLoginRequired,
                style: TextStyle(fontSize: 12, color: c.statusError),
              ),
            ],
          );
        }
        final account = user.originId != null
            ? '${user.email}  ·  #${user.originId}'
            : user.email;
        final devices = auth.devices;
        return Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          mainAxisSize: MainAxisSize.min,
          children: [
            // 已登录徽标。
            Container(
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
              decoration: BoxDecoration(
                color: c.statusSuccess.withValues(alpha: 0.10),
                borderRadius: m.brInput,
              ),
              child: Row(
                children: [
                  Icon(LucideIcons.check, size: 14, color: c.statusSuccess),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      s.addDeviceAccountSynced(account),
                      style: TextStyle(fontSize: 12, color: c.textSecondary),
                    ),
                  ),
                ],
              ),
            ),
            const SizedBox(height: 10),
            if (devices.isEmpty)
              Padding(
                padding: const EdgeInsets.symmetric(vertical: 12),
                child: Text(
                  s.accountDevicesEmpty,
                  style: TextStyle(fontSize: 12, color: c.textMuted),
                  textAlign: TextAlign.center,
                ),
              )
            else
              Container(
                decoration: BoxDecoration(
                  borderRadius: m.brInput,
                  border: Border.all(color: m.borderFade(c.border)),
                ),
                clipBehavior: Clip.antiAlias,
                child: Column(
                  children: [
                    for (var i = 0; i < devices.length; i++) ...[
                      if (i > 0)
                        Container(
                          height: 1,
                          margin: const EdgeInsets.only(left: 46),
                          color: m.borderFade(c.border),
                        ),
                      _CloudDeviceRow(device: devices[i]),
                    ],
                  ],
                ),
              ),
            const SizedBox(height: 12),
            Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Icon(LucideIcons.info, size: 13, color: c.textMuted),
                const SizedBox(width: 6),
                Expanded(
                  child: Text(
                    s.addDeviceAccountFooter,
                    style: TextStyle(fontSize: 11.5, height: 1.5, color: c.textMuted),
                  ),
                ),
              ],
            ),
          ],
        );
      },
    );
  }
}

class _CloudDeviceRow extends StatelessWidget {
  final CloudDevice device;
  const _CloudDeviceRow({required this.device});

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    final isCurrent = device.deviceId == CloudAuthService.instance.currentDeviceId;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
      child: Row(
        children: [
          Icon(_platformIcon(device.platform), size: 16, color: c.textSecondary),
          const SizedBox(width: 10),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  device.name,
                  style: TextStyle(
                    fontSize: 12.5,
                    fontWeight: FontWeight.w600,
                    color: c.textPrimary,
                  ),
                ),
                Text(
                  isCurrent ? s.thisDevice : _platformLabel(s, device.platform),
                  style: TextStyle(fontSize: 11, color: c.textMuted),
                ),
              ],
            ),
          ),
          _statusDot(device.isOnline ? c.statusSuccess : c.textMuted),
          const SizedBox(width: 6),
          Text(
            device.isOnline ? s.deviceOnline : s.deviceOffline,
            style: TextStyle(
              fontSize: 11,
              color: device.isOnline ? c.statusSuccess : c.textMuted,
            ),
          ),
        ],
      ),
    );
  }
}

// ─────────────────────────────────────────────────────────────────────────
// 本地配对 Tab
// ─────────────────────────────────────────────────────────────────────────

class _LocalTab extends StatelessWidget {
  final TextEditingController codeCtrl;
  final TextEditingController hostCtrl;
  final TextEditingController portCtrl;
  final bool manual;
  final VoidCallback onToggleManual;

  const _LocalTab({
    required this.codeCtrl,
    required this.hostCtrl,
    required this.portCtrl,
    required this.manual,
    required this.onToggleManual,
  });

  void _connect(BuildContext context, String host, int port) {
    final s = LocaleScope.of(context);
    final code = codeCtrl.text.trim();
    if (code.length < 6) {
      FluxSonner.of(context).show(ShadToast.destructive(
        title: Text(s.localPairingCodeHint),
      ));
      return;
    }
    LocalPairingService.instance.beginPairing(host: host, port: port, code: code);
  }

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    return ListenableBuilder(
      listenable: LocalPairingService.instance,
      builder: (context, _) {
        final svc = LocalPairingService.instance;
        final challenge = svc.pendingChallenge;
        if (challenge != null) {
          return _SasView(challenge: challenge);
        }
        final peers = svc.discoveredPeers;
        return Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          mainAxisSize: MainAxisSize.min,
          children: [
            Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Icon(LucideIcons.info, size: 13, color: c.textMuted),
                const SizedBox(width: 6),
                Expanded(
                  child: Text(
                    s.localPairingHint,
                    style: TextStyle(fontSize: 11.5, height: 1.5, color: c.textMuted),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 12),
            // 发现列表。
            Container(
              constraints: const BoxConstraints(maxHeight: 180),
              decoration: BoxDecoration(
                borderRadius: m.brInput,
                border: Border.all(color: m.borderFade(c.border)),
              ),
              clipBehavior: Clip.antiAlias,
              child: peers.isEmpty
                  ? Padding(
                      padding: const EdgeInsets.symmetric(vertical: 22),
                      child: Center(
                        child: Text(
                          svc.discovering
                              ? s.localPairingDiscovering
                              : s.localPairingNoDevices,
                          style: TextStyle(fontSize: 12, color: c.textMuted),
                        ),
                      ),
                    )
                  : SingleChildScrollView(
                      child: Column(
                        children: [
                          for (var i = 0; i < peers.length; i++) ...[
                            if (i > 0)
                              Container(
                                height: 1,
                                margin: const EdgeInsets.only(left: 46),
                                color: m.borderFade(c.border),
                              ),
                            _PeerRow(
                              peer: peers[i],
                              onConnect: () => _connect(
                                context,
                                peers[i].host,
                                peers[i].port,
                              ),
                            ),
                          ],
                        ],
                      ),
                    ),
            ),
            const SizedBox(height: 12),
            // 配对码输入。
            Text(
              s.localPairingCodeLabel,
              style: TextStyle(fontSize: 12, fontWeight: FontWeight.w600, color: c.textSecondary),
            ),
            const SizedBox(height: 6),
            ShadInput(
              controller: codeCtrl,
              placeholder: Text(s.localPairingCodePlaceholder),
              keyboardType: TextInputType.number,
              maxLength: 6,
            ),
            const SizedBox(height: 4),
            Text(
              s.localPairingCodeHint,
              style: TextStyle(fontSize: 11, color: c.textMuted),
            ),
            const SizedBox(height: 8),
            // 高级：手动输入地址。
            GestureDetector(
              behavior: HitTestBehavior.opaque,
              onTap: onToggleManual,
              child: Row(
                children: [
                  Icon(
                    manual ? LucideIcons.chevronDown : LucideIcons.chevronRight,
                    size: 14,
                    color: c.accent,
                  ),
                  const SizedBox(width: 4),
                  Text(
                    s.localPairingManualAddress,
                    style: TextStyle(fontSize: 12, color: c.accent),
                  ),
                ],
              ),
            ),
            if (manual) ...[
              const SizedBox(height: 8),
              Row(
                children: [
                  Expanded(
                    flex: 3,
                    child: ShadInput(
                      controller: hostCtrl,
                      placeholder: const Text('192.168.1.5'),
                    ),
                  ),
                  const SizedBox(width: 8),
                  Expanded(
                    child: ShadInput(
                      controller: portCtrl,
                      placeholder: const Text('17800'),
                      keyboardType: TextInputType.number,
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 8),
              Align(
                alignment: Alignment.centerRight,
                child: ShadButton(
                  size: ShadButtonSize.sm,
                  onPressed: () {
                    final host = hostCtrl.text.trim();
                    final port = int.tryParse(portCtrl.text.trim()) ?? 17800;
                    if (host.isEmpty) return;
                    _connect(context, host, port);
                  },
                  child: Text(s.localPairingConnect),
                ),
              ),
            ],
            if (svc.lastError != null) ...[
              const SizedBox(height: 10),
              Text(
                svc.lastError!,
                style: TextStyle(fontSize: 11.5, color: c.statusError),
              ),
            ],
          ],
        );
      },
    );
  }
}

class _PeerRow extends StatelessWidget {
  final LocalDiscoveredPeer peer;
  final VoidCallback onConnect;
  const _PeerRow({required this.peer, required this.onConnect});

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      child: Row(
        children: [
          Icon(_platformIcon(peer.platform), size: 16, color: c.textSecondary),
          const SizedBox(width: 10),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  peer.name,
                  style: TextStyle(
                    fontSize: 12.5,
                    fontWeight: FontWeight.w600,
                    color: c.textPrimary,
                  ),
                ),
                Text(
                  '${peer.host}:${peer.port}',
                  style: TextStyle(fontSize: 11, color: c.textMuted),
                ),
              ],
            ),
          ),
          ShadButton.outline(
            size: ShadButtonSize.sm,
            onPressed: onConnect,
            child: Text(s.localPairingConnect),
          ),
        ],
      ),
    );
  }
}

/// SAS 核对视图：双端应显示相同数字，用户核对一致后确认。
class _SasView extends StatelessWidget {
  final PairingChallenge challenge;
  const _SasView({required this.challenge});

  @override
  Widget build(BuildContext context) {
    final s = LocaleScope.of(context);
    final c = AppColors.of(context);
    final m = AppMetrics.of(context);
    final spaced = challenge.sas.split('').join('  ');
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(
          s.localPairingSasTitle,
          style: TextStyle(fontSize: 13, fontWeight: FontWeight.w600, color: c.textPrimary),
        ),
        const SizedBox(height: 4),
        Text(
          challenge.peerName,
          style: TextStyle(fontSize: 12, color: c.textMuted),
        ),
        const SizedBox(height: 14),
        Container(
          padding: const EdgeInsets.symmetric(vertical: 18),
          decoration: BoxDecoration(
            color: c.surface2,
            borderRadius: m.brInput,
          ),
          alignment: Alignment.center,
          child: Text(
            spaced,
            style: TextStyle(
              fontSize: 26,
              fontWeight: FontWeight.w700,
              letterSpacing: 2,
              color: c.textPrimary,
              fontFeatures: const [FontFeature.tabularFigures()],
            ),
          ),
        ),
        const SizedBox(height: 10),
        Text(
          s.localPairingSasHint,
          style: TextStyle(fontSize: 11.5, height: 1.5, color: c.textMuted),
        ),
        const SizedBox(height: 14),
        Row(
          children: [
            Expanded(
              child: ShadButton.outline(
                onPressed: () => LocalPairingService.instance.confirmPairing(false),
                child: Text(s.cancel),
              ),
            ),
            const SizedBox(width: 10),
            Expanded(
              child: ShadButton(
                onPressed: () {
                  LocalPairingService.instance.confirmPairing(true);
                  FluxSonner.of(context).show(ShadToast(
                    title: Text(s.localPairingPaired(challenge.peerName)),
                  ));
                },
                child: Text(s.localPairingConfirm),
              ),
            ),
          ],
        ),
      ],
    );
  }
}

// ── 共享小工具 ────────────────────────────────────────────────────────────

Widget _statusDot(Color color) => Container(
      width: 8,
      height: 8,
      decoration: BoxDecoration(
        shape: BoxShape.circle,
        color: color,
      ),
    );

IconData _platformIcon(String? platform) => switch (platform) {
      'windows' || 'macos' || 'linux' => LucideIcons.monitor,
      'android' || 'ios' => LucideIcons.smartphone,
      'server' => LucideIcons.server,
      _ => LucideIcons.server,
    };

String _platformLabel(S s, String? platform) => switch (platform) {
      'windows' => s.accountDevicePlatformWindows,
      'macos' => s.accountDevicePlatformMacos,
      'linux' => s.accountDevicePlatformLinux,
      'android' => s.accountDevicePlatformAndroid,
      'ios' => s.accountDevicePlatformIos,
      _ => '—',
    };
