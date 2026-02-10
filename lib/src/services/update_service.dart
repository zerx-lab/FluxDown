import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:rinf/rinf.dart';

import '../bindings/bindings.dart';
import 'log_service.dart';

/// Application version injected at build time.
const _appVersion = String.fromEnvironment('APP_VERSION', defaultValue: 'dev');

/// Update state enum for UI consumption.
enum UpdateStatus {
  /// No check performed yet / idle.
  idle,

  /// Currently checking for updates.
  checking,

  /// An update is available (see [UpdateService.checkResult]).
  available,

  /// Downloading the update installer.
  downloading,

  /// Download completed, ready to install.
  readyToInstall,

  /// An error occurred.
  error,

  /// Already on the latest version.
  upToDate,
}

/// Singleton service that manages the auto-update lifecycle.
///
/// Uses [ChangeNotifier] so UI can `ListenableBuilder` on it.
class UpdateService extends ChangeNotifier {
  UpdateService._() {
    _init();
  }

  static final instance = UpdateService._();

  // ── State ──────────────────────────────────────────────────────────────

  UpdateStatus _status = UpdateStatus.idle;
  UpdateStatus get status => _status;

  /// Result from the last successful check.
  UpdateCheckResult? _checkResult;
  UpdateCheckResult? get checkResult => _checkResult;

  /// Latest download progress (only valid during [UpdateStatus.downloading]).
  UpdateDownloadProgress? _progress;
  UpdateDownloadProgress? get progress => _progress;

  /// Local path of the downloaded installer (valid in [readyToInstall]).
  String _installerPath = '';
  String get installerPath => _installerPath;

  /// Human-readable error message.
  String _errorMessage = '';
  String get errorMessage => _errorMessage;

  /// Current app version.
  String get currentVersion => _appVersion;

  // ── Subscriptions ──────────────────────────────────────────────────────

  StreamSubscription<RustSignalPack<UpdateCheckResult>>? _checkSub;
  StreamSubscription<RustSignalPack<UpdateDownloadProgress>>? _progressSub;

  void _init() {
    _checkSub = UpdateCheckResult.rustSignalStream.listen(_onCheckResult);
    _progressSub = UpdateDownloadProgress.rustSignalStream.listen(
      _onDownloadProgress,
    );
  }

  @override
  void dispose() {
    _checkSub?.cancel();
    _progressSub?.cancel();
    super.dispose();
  }

  // ── Actions ────────────────────────────────────────────────────────────

  /// Trigger a version check via Rust → website API.
  void checkForUpdate() {
    if (_status == UpdateStatus.checking ||
        _status == UpdateStatus.downloading) {
      return; // already in progress
    }
    logInfo('UpdateService', 'checkForUpdate, current=$_appVersion');
    _status = UpdateStatus.checking;
    _errorMessage = '';
    notifyListeners();

    CheckForUpdate(currentVersion: _appVersion).sendSignalToRust();
  }

  /// Start downloading the update installer.
  void downloadUpdate() {
    final result = _checkResult;
    if (result == null || !result.hasUpdate) return;
    if (_status == UpdateStatus.downloading) return;

    logInfo('UpdateService', 'downloadUpdate v${result.latestVersion}');
    _status = UpdateStatus.downloading;
    _progress = null;
    _errorMessage = '';
    notifyListeners();

    DownloadUpdate(
      url: result.downloadUrl,
      version: result.latestVersion,
    ).sendSignalToRust();
  }

  /// Launch the installer and exit the app.
  void installUpdate() {
    if (_installerPath.isEmpty) return;
    logInfo('UpdateService', 'installUpdate path=$_installerPath');
    InstallUpdate(installerPath: _installerPath).sendSignalToRust();
  }

  // ── Signal handlers ────────────────────────────────────────────────────

  void _onCheckResult(RustSignalPack<UpdateCheckResult> pack) {
    final msg = pack.message;
    logInfo(
      'UpdateService',
      'checkResult: hasUpdate=${msg.hasUpdate}, '
          'latest=${msg.latestVersion}, error=${msg.errorMessage}',
    );
    _checkResult = msg;

    if (msg.errorMessage.isNotEmpty) {
      _status = UpdateStatus.error;
      _errorMessage = msg.errorMessage;
    } else if (msg.hasUpdate) {
      _status = UpdateStatus.available;
    } else {
      _status = UpdateStatus.upToDate;
    }
    notifyListeners();
  }

  void _onDownloadProgress(RustSignalPack<UpdateDownloadProgress> pack) {
    final msg = pack.message;
    _progress = msg;

    switch (msg.status) {
      case 0: // downloading
        _status = UpdateStatus.downloading;
      case 1: // completed
        _status = UpdateStatus.readyToInstall;
        _installerPath = msg.installerPath;
      case 2: // error
        _status = UpdateStatus.error;
        _errorMessage = msg.errorMessage;
      default:
        break;
    }
    notifyListeners();
  }

  // ── Helpers ────────────────────────────────────────────────────────────

  /// Format bytes to human-readable string.
  static String formatBytes(int bytes) {
    if (bytes <= 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB'];
    int i = 0;
    double size = bytes.toDouble();
    while (size >= 1024 && i < units.length - 1) {
      size /= 1024;
      i++;
    }
    return '${size.toStringAsFixed(i == 0 ? 0 : 1)} ${units[i]}';
  }

  /// Format speed to human-readable string.
  static String formatSpeed(int bytesPerSec) {
    return '${formatBytes(bytesPerSec)}/s';
  }
}
