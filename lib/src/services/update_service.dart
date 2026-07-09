import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:rinf/rinf.dart';

import '../bindings/bindings.dart';
import 'kv_store.dart';
import 'log_service.dart';

/// Application version injected at build time.
const _appVersion = String.fromEnvironment('APP_VERSION', defaultValue: 'dev');

/// Base URL of the website API.
const _updateApiBase = 'https://fluxdown.zerx.dev';

/// KvStore key for the last version whose changelog was shown.
const _prefKeyLastShownVersion = 'update_changelog_last_shown';

/// A single release entry from the changelog API.
class ChangelogRelease {
  final String tag;
  final String version;
  final String publishedAt;
  final String body;

  const ChangelogRelease({
    required this.tag,
    required this.version,
    required this.publishedAt,
    required this.body,
  });

  factory ChangelogRelease.fromJson(Map<String, dynamic> json) {
    return ChangelogRelease(
      tag: json['tag'] as String? ?? '',
      version: json['version'] as String? ?? '',
      publishedAt: json['published_at'] as String? ?? '',
      body: json['body'] as String? ?? '',
    );
  }
}

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

  /// Pending "previous update failed" message reported by the Rust updater on
  /// startup (empty when there is nothing to report). Set once after the app
  /// requests the marker; the UI should show it once and then call
  /// [acknowledgeFailureMarker].
  String _pendingFailureMessage = '';
  String get pendingFailureMessage => _pendingFailureMessage;

  /// Clear the pending failure message after it has been shown to the user.
  void acknowledgeFailureMarker() {
    if (_pendingFailureMessage.isEmpty) return;
    _pendingFailureMessage = '';
    notifyListeners();
  }

  /// Current app version.
  String get currentVersion => _appVersion;

  /// Changelog releases fetched from the website API (newer than current version).
  List<ChangelogRelease> _changelogReleases = const [];
  List<ChangelogRelease> get changelogReleases => _changelogReleases;

  /// Whether the changelog dialog should be shown (new update + not yet shown).
  bool _shouldShowChangelog = false;
  bool get shouldShowChangelog => _shouldShowChangelog;

  /// Mark the changelog as shown for the current latest version.
  /// Prevents repeated popups on subsequent launches.
  Future<void> markChangelogShown() async {
    _shouldShowChangelog = false;
    final latest = _checkResult?.latestVersion;
    if (latest != null && latest.isNotEmpty) {
      final prefs = KvStore.instance;
      await prefs.setString(_prefKeyLastShownVersion, latest);
      logInfo('UpdateService', 'marked changelog shown for v$latest');
    }
  }

  // ── Subscriptions ──────────────────────────────────────────────────────

  StreamSubscription<RustSignalPack<UpdateCheckResult>>? _checkSub;
  StreamSubscription<RustSignalPack<UpdateDownloadProgress>>? _progressSub;
  StreamSubscription<RustSignalPack<UpdateFailureMarker>>? _failureMarkerSub;

  void _init() {
    _checkSub = UpdateCheckResult.rustSignalStream.listen(_onCheckResult);
    _progressSub = UpdateDownloadProgress.rustSignalStream.listen(
      _onDownloadProgress,
    );
    // Subscribe BEFORE requesting so we never miss the response (no startup
    // race), then ask Rust whether a previous portable update failed.
    _failureMarkerSub = UpdateFailureMarker.rustSignalStream.listen(
      _onFailureMarker,
    );
    RequestUpdateFailureMarker().sendSignalToRust();
  }

  @override
  void dispose() {
    _checkTimeoutTimer?.cancel();
    _checkSub?.cancel();
    _progressSub?.cancel();
    _failureMarkerSub?.cancel();
    super.dispose();
  }

  // ── Actions ────────────────────────────────────────────────────────────

  /// Timer that resets [_status] to [idle] if no response arrives from Rust
  /// within [_checkTimeout]. This prevents the UI from spinning forever when
  /// the Rust task silently panics or the network is unreachable at boot.
  Timer? _checkTimeoutTimer;
  static const _checkTimeout = Duration(seconds: 20);

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

    // Start a timeout guard — if Rust never responds, fall back to error.
    _checkTimeoutTimer?.cancel();
    _checkTimeoutTimer = Timer(_checkTimeout, _onCheckTimeout);

    CheckForUpdate(currentVersion: _appVersion).sendSignalToRust();
  }

  void _onCheckTimeout() {
    if (_status != UpdateStatus.checking) return;
    logInfo(
      'UpdateService',
      'check timed out after ${_checkTimeout.inSeconds}s',
    );
    _status = UpdateStatus.error;
    _errorMessage = 'Check timed out';
    notifyListeners();
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
      fileSize: result.fileSize,
    ).sendSignalToRust();
  }

  /// Launch the installer.
  ///
  /// 桌面端：交给 Rust updater（helper 二进制接管，进程随后退出）。
  /// Android：Rust 无法唤起系统安装器 —— 经 MainActivity 的
  /// `com.fluxdown/storage` channel 走 FileProvider + ACTION_VIEW。
  Future<void> installUpdate() async {
    if (_installerPath.isEmpty) return;
    logInfo('UpdateService', 'installUpdate path=$_installerPath');
    if (Platform.isAndroid) {
      try {
        await const MethodChannel(
          'com.fluxdown/storage',
        ).invokeMethod<bool>('installApk', {'path': _installerPath});
      } on PlatformException catch (e) {
        logError('UpdateService', 'installApk failed', e);
        _status = UpdateStatus.error;
        _errorMessage = e.message ?? e.code;
        notifyListeners();
      }
      return;
    }
    InstallUpdate(installerPath: _installerPath).sendSignalToRust();
  }

  // ── Signal handlers ────────────────────────────────────────────────────

  void _onCheckResult(RustSignalPack<UpdateCheckResult> pack) {
    _checkTimeoutTimer?.cancel();
    _checkTimeoutTimer = null;

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
      // Fetch changelog in background — don't block the status update
      _fetchChangelogAndCheckShown(msg.latestVersion);
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

  /// Handle the pending failure marker reported by Rust on startup.
  void _onFailureMarker(RustSignalPack<UpdateFailureMarker> pack) {
    final message = pack.message.message;
    if (message.isEmpty) return;
    logInfo('UpdateService', 'pending update failure marker received');
    _pendingFailureMessage = message;
    notifyListeners();
  }

  // ── Changelog ──────────────────────────────────────────────────────────

  /// Fetch changelog from website API and decide whether to show the dialog.
  Future<void> _fetchChangelogAndCheckShown(String latestVersion) async {
    // Android：/api/changelog 过滤的是桌面 release 版本线，与 mobile-v*
    // 独立版本号不可比 —— 会误拉全量桌面 changelog，直接跳过。
    if (Platform.isAndroid) return;
    try {
      // Check if we already showed changelog for this version
      final prefs = KvStore.instance;
      final lastShown = prefs.getString(_prefKeyLastShownVersion) ?? '';
      if (lastShown == latestVersion) {
        logInfo(
          'UpdateService',
          'changelog already shown for v$latestVersion, skipping',
        );
        return;
      }

      await _fetchChangelog();

      if (_changelogReleases.isNotEmpty) {
        _shouldShowChangelog = true;
        notifyListeners();
      }
    } catch (e) {
      logInfo('UpdateService', 'failed to fetch changelog: $e');
      // Non-critical — don't change status, user can still update
    }
  }

  /// Fetch all releases newer than the current version from the website API.
  Future<void> _fetchChangelog() async {
    if (_appVersion == 'dev') {
      logInfo('UpdateService', 'skip changelog fetch in dev mode');
      return;
    }

    final uri = Uri.parse(
      '$_updateApiBase/api/changelog?per_page=50&since=v$_appVersion',
    );
    logInfo('UpdateService', 'fetching changelog: $uri');

    final client = HttpClient();
    try {
      client.connectionTimeout = const Duration(seconds: 10);
      final request = await client.getUrl(uri);
      final response = await request.close();

      if (response.statusCode != 200) {
        logInfo(
          'UpdateService',
          'changelog API returned ${response.statusCode}',
        );
        return;
      }

      final body = await response.transform(utf8.decoder).join();
      final json = jsonDecode(body) as Map<String, dynamic>;
      final rawReleases = json['releases'] as List<dynamic>? ?? [];

      // Parse and filter out the current version (API returns >=, we want >)
      final releases = rawReleases
          .map((r) => ChangelogRelease.fromJson(r as Map<String, dynamic>))
          .where((r) => r.version != _appVersion)
          .toList();

      _changelogReleases = releases;
      logInfo(
        'UpdateService',
        'fetched ${releases.length} changelog release(s)',
      );
    } finally {
      client.close();
    }
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
