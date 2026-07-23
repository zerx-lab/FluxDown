// FluxCloud 配置同步 — 同步键目录 v1（见 local://sync-contract.md「同步键目录」节，
// 契约唯一依据，与三方共同维护，不得偏离）。
//
// 本文件把契约里列出的每个同步键，绑定到本地 Provider 的读/写入口：
// [SyncEntry.read] 返回可直接 jsonEncode 的当前值（供 ConfigSyncService 打包 PUT）；
// [SyncEntry.apply] 把远端拉回的 JSON 值宽容解析后写回 Provider，类型不符时静默
// 跳过并 logInfo（远端可能是未来客户端写入的新格式，本端不应崩溃或写脏数据）。
//
// **明确排除**（设备本地属性，不进目录，理由逐项列出）：
// - 保存目录/路径类：default_save_dir、last_save_dir、reveal_file_cmd、
//   custom_categories 中的 saveDir 字段 —— 各设备目录结构不同，同步了也没意义。
// - 端口类：bt/ed2k 监听端口、local_server_*（含 token）—— 端口/凭据是本机网络
//   身份，跨设备同步会互相冲突或泄露本机管理令牌。
// - 每机行为：close_to_tray、start_minimized_to_tray、auto_startup、
//   analytics_enabled、log_max_size_mb、ui_scale、悬浮球坐标、torrent 关联 ——
//   这些描述"这台机器"的行为/尺寸/系统集成状态，不是用户偏好。
// - 代理全部字段 —— 隐私：value 可被管理员在后台查看，代理地址/账号密码不得上云。
// - 导入主题正文（FluxThemeTokens JSON）—— 体积超出单条 8192 字节限额且高频
//   变更价值低；目录只同步"当前选中哪个自定义主题 ID"，主题内容不同步。

import 'package:flutter/material.dart';

import '../../i18n/locale_provider.dart';
import '../../models/settings_provider.dart';
import '../../theme/theme_provider.dart';
import '../log_service.dart';

const _tag = 'SyncCatalog';

/// 单个同步键的读/写绑定。[read] 必须返回 JSON 可编码的值（bool/num/String/null）。
class SyncEntry {
  final String key;
  final dynamic Function() read;
  final void Function(dynamic value) apply;

  const SyncEntry({required this.key, required this.read, required this.apply});
}

// ─────────────────────────────────────────────
// 纯函数：值编码 / 解码（不依赖 Provider 实例，供单测直接调用）
// ─────────────────────────────────────────────

/// bool 宽容解析：类型不符返回 null（不做 "truthy" 猜测，避免误写）。
bool? decodeBool(dynamic value) => value is bool ? value : null;

/// int 宽容解析：JSON 数字在 pull 场景可能被其他语言客户端编码为浮点（如 5.0），
/// 统一按 num 接受再转 int；非数字类型返回 null。
int? decodeInt(dynamic value) {
  if (value is int) return value;
  if (value is num) return value.toInt();
  return null;
}

/// appearance.theme_mode 编码：ThemeMode.name（"system"|"light"|"dark"）。
String encodeThemeMode(ThemeMode mode) => mode.name;

/// appearance.theme_mode 解码：未知字符串返回 null（调用方据此跳过）。
ThemeMode? decodeThemeMode(String value) =>
    ThemeMode.values.where((m) => m.name == value).firstOrNull;

/// appearance.dark_theme / light_theme 的解析结果：要么是选中的自定义主题 ID，
/// 要么是内置主题 ID，二者互斥。
class ThemeSelection {
  final String? customId;
  final BuiltinThemeId? builtinId;

  const ThemeSelection.custom(String id) : customId = id, builtinId = null;
  const ThemeSelection.builtin(BuiltinThemeId id)
    : builtinId = id,
      customId = null;
}

/// appearance.dark_theme / light_theme 编码：`"custom:<id>"` 或 `"builtin:<name>"`。
String encodeThemeSelection({
  required String? customId,
  required BuiltinThemeId builtin,
}) => customId != null ? 'custom:$customId' : 'builtin:${builtin.name}';

/// appearance.dark_theme / light_theme 解码：格式非法或内置 ID 未知返回 null；
/// "custom:" 前缀本地是否存在该导入主题由调用方（拿到 [ThemeProvider]）校验。
ThemeSelection? decodeThemeSelection(String value) {
  if (value.startsWith('custom:')) {
    final id = value.substring('custom:'.length);
    return id.isEmpty ? null : ThemeSelection.custom(id);
  }
  if (value.startsWith('builtin:')) {
    final name = value.substring('builtin:'.length);
    final id = BuiltinThemeId.values.where((e) => e.name == name).firstOrNull;
    return id == null ? null : ThemeSelection.builtin(id);
  }
  return null;
}

// ─────────────────────────────────────────────
// 目录装配
// ─────────────────────────────────────────────

SyncEntry _bool(String key, bool Function() read, void Function(bool) write) =>
    SyncEntry(
      key: key,
      read: read,
      apply: (value) {
        final v = decodeBool(value);
        if (v == null) {
          logInfo(_tag, 'skip $key: expected bool, got ${value.runtimeType}');
          return;
        }
        write(v);
      },
    );

SyncEntry _int(String key, int Function() read, void Function(int) write) =>
    SyncEntry(
      key: key,
      read: read,
      apply: (value) {
        final v = decodeInt(value);
        if (v == null) {
          logInfo(_tag, 'skip $key: expected int, got ${value.runtimeType}');
          return;
        }
        write(v);
      },
    );

SyncEntry _string(
  String key,
  String Function() read,
  void Function(String) write,
) => SyncEntry(
  key: key,
  read: read,
  apply: (value) {
    if (value is! String) {
      logInfo(_tag, 'skip $key: expected string, got ${value.runtimeType}');
      return;
    }
    write(value);
  },
);

SyncEntry _double(
  String key,
  double Function() read,
  void Function(double) write,
) => SyncEntry(
  key: key,
  read: read,
  apply: (value) {
    final v = switch (value) {
      num n => n.toDouble(),
      String s => double.tryParse(s),
      _ => null,
    };
    if (v == null) {
      logInfo(_tag, 'skip $key: expected double, got ${value.runtimeType}');
      return;
    }
    write(v);
  },
);

void _applyThemeSelection(
  ThemeProvider theme,
  String key,
  dynamic value, {
  required bool dark,
}) {
  if (value is! String) {
    logInfo(_tag, 'skip $key: expected string, got ${value.runtimeType}');
    return;
  }
  final sel = decodeThemeSelection(value);
  if (sel == null) {
    logInfo(_tag, 'skip $key: malformed value "$value"');
    return;
  }
  final customId = sel.customId;
  if (customId != null) {
    final exists = theme.importedThemes.any((e) => e.id == customId);
    if (!exists) {
      logInfo(_tag, 'skip $key: imported theme "$customId" not found locally');
      return;
    }
    theme.selectImportedTheme(customId);
    return;
  }
  final builtinId = sel.builtinId!;
  if (dark) {
    theme.setDarkTheme(builtinId);
  } else {
    theme.setLightTheme(builtinId);
  }
}

/// 装配契约「同步键目录 v1」的全部 41 个键。顺序与契约文档一致，便于对照审阅。
List<SyncEntry> buildSyncCatalog({
  required SettingsProvider settings,
  required ThemeProvider theme,
  required LocaleNotifier locale,
}) => [
  // ── appearance（5）──
  SyncEntry(
    key: 'appearance.theme_mode',
    read: () => encodeThemeMode(theme.themeMode),
    apply: (value) {
      if (value is! String) {
        logInfo(
          _tag,
          'skip appearance.theme_mode: expected string, got ${value.runtimeType}',
        );
        return;
      }
      final mode = decodeThemeMode(value);
      if (mode == null) {
        logInfo(_tag, 'skip appearance.theme_mode: unknown value "$value"');
        return;
      }
      theme.setThemeMode(mode);
    },
  ),
  SyncEntry(
    key: 'appearance.dark_theme',
    read: () => encodeThemeSelection(
      customId: theme.isCustomDarkActive ? theme.selectedCustomDarkId : null,
      builtin: theme.selectedDarkTheme,
    ),
    apply: (value) =>
        _applyThemeSelection(theme, 'appearance.dark_theme', value, dark: true),
  ),
  SyncEntry(
    key: 'appearance.light_theme',
    read: () => encodeThemeSelection(
      customId: theme.isCustomLightActive ? theme.selectedCustomLightId : null,
      builtin: theme.selectedLightTheme,
    ),
    apply: (value) => _applyThemeSelection(
      theme,
      'appearance.light_theme',
      value,
      dark: false,
    ),
  ),
  SyncEntry(
    key: 'appearance.color_scheme',
    read: () => theme.colorScheme.name,
    apply: (value) {
      if (value is! String) {
        logInfo(
          _tag,
          'skip appearance.color_scheme: expected string, got ${value.runtimeType}',
        );
        return;
      }
      final scheme = AppColorScheme.values
          .where((e) => e.name == value)
          .firstOrNull;
      if (scheme == null) {
        logInfo(_tag, 'skip appearance.color_scheme: unknown value "$value"');
        return;
      }
      theme.setColorScheme(scheme);
    },
  ),
  SyncEntry(
    key: 'appearance.custom_color',
    read: () => theme.customColor.toARGB32(),
    apply: (value) {
      final v = decodeInt(value);
      if (v == null) {
        logInfo(
          _tag,
          'skip appearance.custom_color: expected int, got ${value.runtimeType}',
        );
        return;
      }
      theme.setCustomColor(Color(v));
    },
  ),

  // ── general（6）──
  SyncEntry(
    key: 'general.locale',
    read: () => locale.preference,
    apply: (value) {
      if (value is! String) {
        logInfo(
          _tag,
          'skip general.locale: expected string, got ${value.runtimeType}',
        );
        return;
      }
      if (value != kLocaleSystem && !I18nStore.available.contains(value)) {
        logInfo(_tag, 'skip general.locale: unknown locale "$value"');
        return;
      }
      locale.setLocale(value);
    },
  ),
  _string(
    'general.update_channel',
    () => settings.updateChannel,
    settings.setUpdateChannel,
  ),
  _bool(
    'general.auto_check_update',
    () => settings.autoCheckUpdate,
    settings.setAutoCheckUpdate,
  ),
  _bool(
    'general.clipboard_watch',
    () => settings.clipboardWatchEnabled,
    settings.setClipboardWatchEnabled,
  ),
  _bool(
    'general.floating_ball_enabled',
    () => settings.floatingBallEnabled,
    settings.setFloatingBallEnabled,
  ),
  _bool(
    'general.floating_ball_active_only',
    () => settings.floatingBallActiveOnly,
    settings.setFloatingBallActiveOnly,
  ),

  // ── ui（7，均 bool）──
  _bool(
    'ui.show_sidebar_status',
    () => settings.showSidebarStatus,
    settings.setShowSidebarStatus,
  ),
  _bool(
    'ui.show_sidebar_queues',
    () => settings.showSidebarQueues,
    settings.setShowSidebarQueues,
  ),
  _bool(
    'ui.show_sidebar_category',
    () => settings.showSidebarCategory,
    settings.setShowSidebarCategory,
  ),
  _bool(
    'ui.show_titlebar_pause_all',
    () => settings.showTitlebarPauseAll,
    settings.setShowTitlebarPauseAll,
  ),
  _bool(
    'ui.show_titlebar_resume_all',
    () => settings.showTitlebarResumeAll,
    settings.setShowTitlebarResumeAll,
  ),
  _bool(
    'ui.show_titlebar_settings',
    () => settings.showTitlebarSettings,
    settings.setShowTitlebarSettings,
  ),
  _bool(
    'ui.show_titlebar_theme',
    () => settings.showTitlebarTheme,
    settings.setShowTitlebarTheme,
  ),

  // ── download（13）──
  _int(
    'download.max_concurrent_tasks',
    () => settings.maxConcurrentTasks,
    settings.setMaxConcurrentTasks,
  ),
  _int(
    'download.default_segments',
    () => settings.defaultSegments,
    settings.setDefaultSegments,
  ),
  _int(
    'download.auto_max_connections',
    () => settings.autoMaxConnections,
    settings.setAutoMaxConnections,
  ),
  _int(
    'download.speed_limit_bytes',
    () => settings.speedLimitBytes,
    settings.setSpeedLimitBytes,
  ),
  _int(
    'download.max_auto_retries',
    () => settings.maxAutoRetries,
    settings.setMaxAutoRetries,
  ),
  _int(
    'download.auto_retry_delay_secs',
    () => settings.autoRetryDelaySecs,
    settings.setAutoRetryDelaySecs,
  ),
  _bool(
    'download.auto_resume_on_start',
    () => settings.autoResumeOnStart,
    settings.setAutoResumeOnStart,
  ),
  _bool(
    'download.remember_last_save_dir',
    () => settings.rememberLastSaveDir,
    settings.setRememberLastSaveDir,
  ),
  _bool(
    'download.use_server_time',
    () => settings.useServerTime,
    settings.setUseServerTime,
  ),
  _string(
    'download.global_user_agent',
    () => settings.globalUserAgent,
    settings.setGlobalUserAgent,
  ),
  _bool(
    'download.notify_on_complete',
    () => settings.notifyOnComplete,
    settings.setNotifyOnComplete,
  ),
  _bool(
    'download.silent_download',
    () => settings.silentDownloadEnabled,
    settings.setSilentDownloadEnabled,
  ),
  _bool(
    'download.keep_awake',
    () => settings.keepAwakeWhileDownloading,
    settings.setKeepAwakeWhileDownloading,
  ),

  // ── bt（5 + 6 做种）──
  _bool('bt.enable_dht', () => settings.btEnableDht, settings.setBtEnableDht),
  _bool(
    'bt.enable_upnp',
    () => settings.btEnableUpnp,
    settings.setBtEnableUpnp,
  ),
  _string(
    'bt.custom_trackers',
    () => settings.btCustomTrackers,
    settings.setBtCustomTrackers,
  ),
  _bool(
    'bt.tracker_sub_enabled',
    () => settings.btTrackerSubEnabled,
    settings.setBtTrackerSubEnabled,
  ),
  _string(
    'bt.tracker_sub_urls',
    () => settings.btTrackerSubUrls,
    settings.setBtTrackerSubUrls,
  ),

  // ── bt 做种（6）──
  // 启用态由 limit>0 编码（0=关闭），同步 limit 值即同步启用状态。
  _double(
    'bt.seed_ratio_limit',
    () => settings.btSeedRatioLimit,
    settings.setBtSeedRatioLimit,
  ),
  _double(
    'bt.seed_post_ratio_limit',
    () => settings.btSeedPostRatioLimit,
    settings.setBtSeedPostRatioLimit,
  ),
  _int(
    'bt.seed_time_limit_minutes',
    () => settings.btSeedTimeLimitMinutes,
    settings.setBtSeedTimeLimitMinutes,
  ),
  _int(
    'bt.seed_inactive_time_limit_minutes',
    () => settings.btSeedInactiveTimeLimitMinutes,
    settings.setBtSeedInactiveTimeLimitMinutes,
  ),
  _string(
    'bt.seed_limit_operator',
    () => settings.btSeedConditionsOperator,
    settings.setBtSeedConditionsOperator,
  ),
  _string(
    'bt.seed_then_action',
    () => settings.btSeedThenAction,
    settings.setBtSeedThenAction,
  ),

  // ── ed2k（5）──
  _bool(
    'ed2k.enable_kad',
    () => settings.ed2kEnableKad,
    settings.setEd2kEnableKad,
  ),
  _bool(
    'ed2k.enable_upnp',
    () => settings.ed2kEnableUpnp,
    settings.setEd2kEnableUpnp,
  ),
  _string(
    'ed2k.server_list',
    () => settings.ed2kServerList,
    settings.setEd2kServerList,
  ),
  _bool(
    'ed2k.server_sub_enabled',
    () => settings.ed2kServerSubEnabled,
    settings.setEd2kServerSubEnabled,
  ),
  _string(
    'ed2k.server_sub_urls',
    () => settings.ed2kServerSubUrls,
    settings.setEd2kServerSubUrls,
  ),
];
