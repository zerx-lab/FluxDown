// FluxCloud 客户端 —— 轻量 JSON HTTP 封装（同 feedback_service.dart 用法：
// dart:io HttpClient，不引入 http 包），严格实现契约 v1 全部客户端接口。
//
// 401 自动刷新：devices/me 等需要 Bearer 的接口若返回 401，自动用 refreshToken
// 刷新一次并重放原请求；刷新也失败（refreshToken 过期/被吊销）则触发
// [onSessionExpired]，由 CloudAuthService 清空本地会话。本文件只负责传输层
// 机制，不持久化任何令牌 —— 令牌的读取/持久化由 CloudAuthService 通过
// [accessToken]/[refreshToken] 字段与 [onTokenRefreshed] 回调完成同步。

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';

import '../kv_store.dart';
import 'cloud_models.dart';
import 'device_identity.dart';

/// 默认服务地址：Actions 打包时用 --dart-define=FLUXCLOUD_BASE_URL=https://... 注入
/// 官方地址，未注入时回退本地联调端口（开发期）。
const _kDefaultApiBase = String.fromEnvironment(
  'FLUXCLOUD_BASE_URL',
  defaultValue: 'http://127.0.0.1:8720',
);
const _kApiBaseKvKey = 'cloud_api_base';
const _kApiPrefix = '/api/v1';

/// FluxCloud 服务地址配置：默认常量 + kv_store 覆盖项，供设置页读写。
class CloudApiConfig {
  CloudApiConfig._();

  /// 官方默认地址（开发期指向本地联调端口）。
  static const String defaultBaseUrl = _kDefaultApiBase;

  /// 当前生效的服务地址：仅调试构建允许 kv 自定义覆盖（对应设置项也只在
  /// 调试构建显示），正式包锁定默认常量，避免残留覆盖值指向失效地址。
  static String get baseUrl {
    if (!kDebugMode) return _kDefaultApiBase;
    final custom = KvStore.instance.getString(_kApiBaseKvKey);
    return (custom != null && custom.trim().isNotEmpty)
        ? custom.trim()
        : _kDefaultApiBase;
  }

  /// 是否为用户自定义地址（非默认值），供设置页展示"恢复默认"按钮状态。
  static bool get isCustom {
    final custom = KvStore.instance.getString(_kApiBaseKvKey);
    return custom != null && custom.trim().isNotEmpty && custom.trim() != _kDefaultApiBase;
  }

  static Future<void> setBaseUrl(String url) =>
      KvStore.instance.setString(_kApiBaseKvKey, url.trim());

  static Future<void> resetToDefault() => KvStore.instance.remove(_kApiBaseKvKey);
}

class CloudClient {
  CloudClient._();
  static final CloudClient instance = CloudClient._();

  static const _timeout = Duration(seconds: 15);

  /// 当前会话令牌，由 CloudAuthService 在登录/刷新/登出时同步写入。
  /// 客户端只用它们发起带 Authorization 头的请求 + 401 时的刷新重放，
  /// 不持久化、不感知具体业务状态。
  String? accessToken;
  String? refreshToken;

  /// 401 触发的刷新流程成功后回调，供上层持久化新令牌 + 更新用户快照。
  void Function(AuthResponse auth)? onTokenRefreshed;

  /// 刷新也失败（refreshToken 过期/被吊销）时回调，供上层清空本地会话。
  void Function()? onSessionExpired;

  HttpClient? _http;

  void _ensureHttp() {
    _http ??= HttpClient()
      ..connectionTimeout = const Duration(seconds: 10)
      ..idleTimeout = const Duration(seconds: 15);
  }

  /// 释放底层连接池；本服务为应用级单例，正常不需要主动调用。
  void dispose() {
    _http?.close(force: true);
    _http = null;
  }

  // ── 注册 / 登录 ──────────────────────────────────────────────────────

  /// POST /auth/register：发码建 pending 用户，返回验证码 TTL（秒）。
  Future<int> register({
    required String email,
    required String password,
    String? nickname,
  }) async {
    final json = await _request(
      'POST',
      '/auth/register',
      body: {
        'email': email,
        'password': password,
        if (nickname != null && nickname.trim().isNotEmpty)
          'nickname': nickname.trim(),
      },
    );
    return _ttlSeconds(json);
  }

  /// POST /auth/register/verify：验证码激活 pending 用户 + 信任当前设备 + 签发令牌。
  Future<AuthResponse> registerVerify({
    required String email,
    required String code,
    required String deviceId,
    String? deviceName,
    String? devicePlatform,
    String? appVersion,
  }) async {
    final json = await _request(
      'POST',
      '/auth/register/verify',
      body: _withDeviceInfo(
        {'email': email, 'code': code},
        deviceId,
        deviceName,
        devicePlatform,
        appVersion,
      ),
    );
    return AuthResponse.fromJson(json);
  }

  /// POST /auth/login：tagged 响应，设备已受信任直接下发令牌，
  /// 新设备则返回 deviceVerificationRequired（服务端已自动发码）。
  /// [account] 接受邮箱或纯数字 Origin ID（契约 v1.2），服务端字段名 account。
  Future<LoginResult> login({
    required String account,
    required String password,
    required String deviceId,
    String? deviceName,
    String? devicePlatform,
    String? appVersion,
  }) async {
    final json = await _request(
      'POST',
      '/auth/login',
      body: _withDeviceInfo(
        {'account': account, 'password': password},
        deviceId,
        deviceName,
        devicePlatform,
        appVersion,
      ),
    );
    final status = json['status'] as String?;
    if (status == 'deviceVerificationRequired') {
      return LoginDeviceVerificationRequired(_ttlSeconds(json));
    }
    final authJson = json['auth'];
    if (status == 'ok' && authJson is Map<String, dynamic>) {
      return LoginOk(AuthResponse.fromJson(authJson));
    }
    throw const CloudApiException(
      code: 'malformed_response',
      message: '登录响应格式异常',
      status: 200,
    );
  }

  /// POST /auth/login/verify：新设备验证码登录，重新校验密码 + 消费验证码。
  /// [account] 语义同 [login]。
  Future<AuthResponse> loginVerify({
    required String account,
    required String password,
    required String code,
    required String deviceId,
    String? deviceName,
    String? devicePlatform,
    String? appVersion,
  }) async {
    final json = await _request(
      'POST',
      '/auth/login/verify',
      body: _withDeviceInfo(
        {'account': account, 'password': password, 'code': code},
        deviceId,
        deviceName,
        devicePlatform,
        appVersion,
      ),
    );
    return AuthResponse.fromJson(json);
  }

  /// POST /auth/code/send：发送验证码登录用的验证码，返回 TTL（秒）。
  Future<int> sendCode(String email) async {
    final json = await _request('POST', '/auth/code/send', body: {'email': email});
    return _ttlSeconds(json);
  }

  /// POST /auth/code/verify：验证码登录（邮箱不存在则自动注册），信任当前设备。
  /// [nickname] 仅在服务端"邮箱不存在→自动注册新用户"分支生效，已存在用户忽略，
  /// 可放心恒传（默认昵称跟随当前界面语言，见 nickname_pool.dart）。
  Future<AuthResponse> verifyCode({
    required String email,
    required String code,
    required String deviceId,
    String? deviceName,
    String? devicePlatform,
    String? appVersion,
    String? nickname,
  }) async {
    final json = await _request(
      'POST',
      '/auth/code/verify',
      body: _withDeviceInfo(
        {
          'email': email,
          'code': code,
          if (nickname != null && nickname.trim().isNotEmpty)
            'nickname': nickname.trim(),
        },
        deviceId,
        deviceName,
        devicePlatform,
        appVersion,
      ),
    );
    return AuthResponse.fromJson(json);
  }

  /// POST /auth/refresh：刷新令牌轮换。
  Future<AuthResponse> refresh(String refreshToken) async {
    final json = await _request(
      'POST',
      '/auth/refresh',
      body: {'refreshToken': refreshToken},
    );
    return AuthResponse.fromJson(json);
  }

  /// POST /auth/logout。
  Future<void> logout(String refreshToken) async {
    await _request('POST', '/auth/logout', body: {'refreshToken': refreshToken});
  }

  // ── 已登录接口（Bearer UserAuth，401 自动刷新重放一次）──────────────────

  /// GET /me：当前用户信息 + 套餐能力快照。
  Future<CloudProfile> me() => _authed(() async {
    final json = await _request('GET', '/me', authed: true);
    return CloudProfile.fromJson(json);
  });

  /// GET /devices：当前用户名下已信任设备，按 lastSeenAt 降序。
  Future<List<CloudDevice>> devices() => _authed(() async {
    final json = await _request(
      'GET',
      '/devices?deviceId=${Uri.encodeQueryComponent(DeviceIdentity.deviceId())}',
      authed: true,
    );
    final list = json['devices'] as List<dynamic>? ?? const [];
    return list
        .map((e) => CloudDevice.fromJson(e as Map<String, dynamic>))
        .toList();
  });

  /// PATCH /devices/{id} {name}：设备改名，1-64 字符校验由服务端兜底。
  Future<CloudDevice> renameDevice(String id, String name) => _authed(() async {
    final json = await _request(
      'PATCH',
      '/devices/$id',
      body: {'name': name},
      authed: true,
    );
    return CloudDevice.fromJson(json);
  });

  /// DELETE /devices/{id}：删除设备 + 吊销其名下全部未撤销 refresh token。
  Future<void> deleteDevice(String id) => _authed(() async {
    await _request('DELETE', '/devices/$id', authed: true);
  });

  // ── 跨设备任务协同（Bearer UserAuth；SSE 事件流由 RemoteTaskService 独立直连）──

  /// POST /tasks/dispatch：把下载任务下发给目标设备执行。返回创建的跨设备任务。
  Future<RemoteTask> dispatchTask({
    required String toDevice,
    required String url,
    String? saveDir,
    String? fileName,
    Map<String, dynamic>? options,
  }) => _authed(() async {
    final json = await _request(
      'POST',
      '/tasks/dispatch',
      body: {
        'deviceId': DeviceIdentity.deviceId(),
        'toDevice': toDevice,
        'url': url,
        if (saveDir != null && saveDir.isNotEmpty) 'saveDir': saveDir,
        if (fileName != null && fileName.isNotEmpty) 'fileName': fileName,
        ...?options,
      },
      authed: true,
    );
    return RemoteTask.fromJson(json);
  });

  /// GET /tasks/remote：拉取本账号全部跨设备任务（持久态 + 内存进度快照），断线重连用。
  Future<List<RemoteTask>> remoteTasks() => _authed(() async {
    final json = await _request('GET', '/tasks/remote', authed: true);
    final list = json['tasks'] as List<dynamic>? ?? const [];
    return list
        .map((e) => RemoteTask.fromJson(e as Map<String, dynamic>))
        .toList();
  });

  /// POST /tasks/{id}/status：执行端上报任务状态转换（服务端落库 + 广播）。
  Future<void> reportTaskStatus(
    String id, {
    required String status,
    int? totalBytes,
    String? fileName,
    String? error,
  }) => _authed(() async {
    await _request(
      'POST',
      '/tasks/$id/status',
      body: {
        'status': status,
        if (totalBytes != null) 'totalBytes': totalBytes,
        if (fileName != null && fileName.isNotEmpty) 'fileName': fileName,
        if (error != null && error.isNotEmpty) 'error': error,
      },
      authed: true,
    );
  });

  /// POST /tasks/progress：执行端批量上报进度（服务端仅更内存 + 广播，不落库）。
  Future<void> reportProgress(List<ProgressReport> items) => _authed(() async {
    if (items.isEmpty) return;
    await _request(
      'POST',
      '/tasks/progress',
      body: {'items': items.map((e) => e.toJson()).toList()},
      authed: true,
    );
  });

  /// POST /tasks/{id}/command：向执行端下发控制命令（pause/resume/cancel）。
  Future<void> commandTask(String id, String action) => _authed(() async {
    await _request(
      'POST',
      '/tasks/$id/command',
      body: {'action': action},
      authed: true,
    );
  });

  /// POST /me/email/code：向当前绑定邮箱发送验证码（邮箱变更第一步），返回 TTL（秒）。
  Future<int> sendEmailChangeCode() => _authed(() async {
    final json = await _request('POST', '/me/email/code', authed: true);
    return _ttlSeconds(json);
  });

  /// POST /me/email/code/new：携原邮箱验证码向新邮箱发送验证码（第二步），返回 TTL（秒）。
  Future<int> sendEmailChangeNewCode({
    required String newEmail,
    required String oldCode,
  }) => _authed(() async {
    final json = await _request(
      'POST',
      '/me/email/code/new',
      body: {'email': newEmail, 'code': oldCode},
      authed: true,
    );
    return _ttlSeconds(json);
  });

  /// POST /me/email：同时校验原/新邮箱验证码并更新绑定邮箱（第三步），返回最新用户资料。
  Future<CloudProfile> changeEmail({
    required String newEmail,
    required String oldCode,
    required String newCode,
  }) => _authed(() async {
    final json = await _request(
      'POST',
      '/me/email',
      body: {'email': newEmail, 'oldCode': oldCode, 'newCode': newCode},
      authed: true,
    );
    return CloudProfile.fromJson(json);
  });

  // ── 配置同步（Bearer UserAuth，401 自动刷新重放一次；SSE 事件流由
  //    ConfigSyncService 用独立 HttpClient 直连，不走本类）──────────────────

  /// GET /sync/items：拉取 version > since 的条目（含墓碑），resync=true 时
  /// 客户端应重置水位线并将本地目录中云端缺失的键标脏重传。
  Future<SyncPullResult> syncPull({required int since, required String deviceId}) =>
      _authed(() async {
        final json = await _request(
          'GET',
          '/sync/items?since=$since&deviceId=${Uri.encodeQueryComponent(deviceId)}',
          authed: true,
        );
        return SyncPullResult.fromJson(json);
      });

  /// PUT /sync/items：批量推送本地变更，返回服务端最新 revision。回包 revision
  /// 恰为本地水位线+1 时，ConfigSyncService 会快进水位线以消除自回显 pull；
  /// 其余情况（有并发外部写入）仍靠 SSE 事件→pull 路径推进。
  Future<int> syncPush({
    required String deviceId,
    required List<Map<String, dynamic>> items,
  }) => _authed(() async {
    final json = await _request(
      'PUT',
      '/sync/items',
      body: {'deviceId': deviceId, 'items': items},
      authed: true,
    );
    return (json['revision'] as num?)?.toInt() ?? 0;
  });

  // ── 内部实现 ─────────────────────────────────────────────────────────

  Map<String, dynamic> _withDeviceInfo(
    Map<String, dynamic> body,
    String deviceId,
    String? deviceName,
    String? devicePlatform,
    String? appVersion,
  ) => {
    ...body,
    'deviceId': deviceId,
    if (deviceName != null && deviceName.isNotEmpty) 'deviceName': deviceName,
    if (devicePlatform != null && devicePlatform.isNotEmpty)
      'devicePlatform': devicePlatform,
    if (appVersion != null && appVersion.isNotEmpty) 'appVersion': appVersion,
  };

  int _ttlSeconds(Map<String, dynamic> json) =>
      (json['ttlSeconds'] as num?)?.toInt() ?? 0;

  /// 需要 Bearer 认证的调用统一包装：命中 401 时尝试用 refreshToken 刷新一次
  /// 并重放原请求；无 refreshToken 或刷新本身失败，则清会话并把原 401 抛出去。
  Future<T> _authed<T>(Future<T> Function() call) async {
    try {
      return await call();
    } on CloudApiException catch (e) {
      if (e.status != 401) rethrow;
      final rt = refreshToken;
      if (rt == null || rt.isEmpty) {
        onSessionExpired?.call();
        rethrow;
      }
      try {
        final auth = await refresh(rt);
        accessToken = auth.accessToken;
        refreshToken = auth.refreshToken;
        onTokenRefreshed?.call(auth);
      } catch (_) {
        onSessionExpired?.call();
        // rethrow 只会重抛这个 catch 自己捕获的刷新失败异常；这里要的是原始 401，
        // 显式 throw e（闭包捕获外层 catch 绑定的异常）。
        throw e;
      }
      return await call();
    }
  }

  Future<Map<String, dynamic>> _request(
    String method,
    String path, {
    Map<String, dynamic>? body,
    bool authed = false,
  }) async {
    _ensureHttp();
    final uri = Uri.parse('${CloudApiConfig.baseUrl}$_kApiPrefix$path');
    try {
      final HttpClientRequest req = await switch (method) {
        'GET' => _http!.getUrl(uri).timeout(_timeout),
        'POST' => _http!.postUrl(uri).timeout(_timeout),
        'PUT' => _http!.putUrl(uri).timeout(_timeout),
        'PATCH' => _http!.patchUrl(uri).timeout(_timeout),
        'DELETE' => _http!.deleteUrl(uri).timeout(_timeout),
        _ => throw ArgumentError('unsupported method $method'),
      };
      req.headers.set('Accept', 'application/json');
      if (authed && accessToken != null && accessToken!.isNotEmpty) {
        req.headers.set('Authorization', 'Bearer $accessToken');
      }
      if (body != null) {
        final payload = utf8.encode(jsonEncode(body));
        req.headers.set('Content-Type', 'application/json; charset=utf-8');
        req.contentLength = payload.length;
        req.add(payload);
      }
      final res = await req.close().timeout(_timeout);
      final text = await res.transform(utf8.decoder).join();

      if (res.statusCode >= 200 && res.statusCode < 300) {
        if (text.trim().isEmpty) return const {};
        final decoded = jsonDecode(text);
        return decoded is Map<String, dynamic> ? decoded : {'value': decoded};
      }

      var code = 'unknown_error';
      var message = 'HTTP ${res.statusCode}';
      try {
        final decoded = jsonDecode(text);
        if (decoded is Map<String, dynamic>) {
          code = (decoded['code'] as String?) ?? code;
          message = (decoded['message'] as String?) ?? message;
        }
      } catch (_) {
        // 错误体不是合法 JSON：保留默认 code/message，不阻断错误抛出。
      }
      throw CloudApiException(code: code, message: message, status: res.statusCode);
    } on CloudApiException {
      rethrow;
    } on TimeoutException {
      throw const CloudApiException(
        code: 'network_error',
        message: '请求超时，请检查网络或服务器地址',
        status: 0,
      );
    } catch (e) {
      throw CloudApiException(
        code: 'network_error',
        message: '网络请求失败：$e',
        status: 0,
      );
    }
  }
}
