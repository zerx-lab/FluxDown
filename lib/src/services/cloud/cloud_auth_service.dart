// FluxCloud 账户会话服务 —— 单例 + ChangeNotifier（同 UpdateService 的单例风格），
// 持久化 accessToken/refreshToken/用户快照，暴露契约全部客户端动作。
//
// 职责边界：本服务只维护"会话状态"（未登录/已登录 + 当前用户 + 套餐能力），
// 流程性 UI 状态（如"等待注册验证码"“等待新设备验证码”及倒计时）由调用方
// （设置页对话框）自行持有 —— 本服务的注册/登录方法均为无状态的一次性请求，
// 可重复调用（如重发验证码即再次调用 register/login/sendCode）。

import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';

import '../kv_store.dart';
import '../log_service.dart';
import 'cloud_client.dart';
import 'cloud_models.dart';
import 'device_identity.dart';

const _tag = 'CloudAuth';

const _kAccessTokenKey = 'cloud_access_token';
const _kRefreshTokenKey = 'cloud_refresh_token';
const _kUserKey = 'cloud_user';
const _kEntitlementsKey = 'cloud_entitlements';

enum CloudAuthStatus { unauthenticated, authenticated }

class CloudAuthService extends ChangeNotifier {
  CloudAuthService._() {
    _restore();
    CloudClient.instance.onTokenRefreshed = (auth) {
      unawaited(_applySession(auth));
    };
    CloudClient.instance.onSessionExpired = () {
      unawaited(_clearSession());
    };
  }

  static final CloudAuthService instance = CloudAuthService._();

  CloudAuthStatus _status = CloudAuthStatus.unauthenticated;
  CloudAuthStatus get status => _status;
  bool get isLoggedIn => _status == CloudAuthStatus.authenticated;

  CloudUser? _user;
  CloudUser? get user => _user;

  Entitlements? _entitlements;
  Entitlements? get entitlements => _entitlements;

  /// 当前设备的持久标识，供设备列表 UI 判断"是否当前设备"。
  String get currentDeviceId => DeviceIdentity.deviceId();

  /// 设备名册缓存（登录后共享给侧栏设备区与设置页，避免各自重复拉取）。
  List<CloudDevice> _devices = const [];
  List<CloudDevice> get devices => _devices;

  /// 除当前设备外的远程设备（侧栏「设备」区渐进披露的判定源）。
  List<CloudDevice> get remoteDevices =>
      _devices.where((d) => d.deviceId != currentDeviceId).toList();

  /// 是否存在远程设备（决定侧栏设备区是否自动出现）。
  bool get hasRemoteDevices => remoteDevices.isNotEmpty;

  void _restore() {
    final at = KvStore.instance.getString(_kAccessTokenKey);
    final rt = KvStore.instance.getString(_kRefreshTokenKey);
    final userJson = KvStore.instance.getString(_kUserKey);
    if (at == null || at.isEmpty || rt == null || rt.isEmpty || userJson == null) {
      return;
    }
    try {
      _user = CloudUser.fromJson(jsonDecode(userJson) as Map<String, dynamic>);
      final entJson = KvStore.instance.getString(_kEntitlementsKey);
      _entitlements = Entitlements.fromJson(
        entJson != null ? jsonDecode(entJson) as Map<String, dynamic> : null,
      );
      CloudClient.instance.accessToken = at;
      CloudClient.instance.refreshToken = rt;
      _status = CloudAuthStatus.authenticated;
    } catch (e, stack) {
      logError(_tag, 'restore session failed, treating as signed out', e, stack);
    }
  }

  // ── 注册 ─────────────────────────────────────────────────────────────

  /// 发起注册（或为未完成注册的邮箱重发验证码）。返回验证码 TTL（秒）。
  Future<int> register({
    required String email,
    required String password,
    String? nickname,
  }) => CloudClient.instance.register(email: email, password: password, nickname: nickname);

  /// 提交注册验证码：激活账户 + 信任当前设备 + 建立会话。
  Future<void> registerVerify({required String email, required String code}) async {
    final auth = await CloudClient.instance.registerVerify(
      email: email,
      code: code,
      deviceId: DeviceIdentity.deviceId(),
      deviceName: await DeviceIdentity.resolvedName(),
      devicePlatform: DeviceIdentity.platform(),
      appVersion: DeviceIdentity.appVersion(),
    );
    await _applySession(auth);
  }

  // ── 登录（密码）───────────────────────────────────────────────────────

  /// 密码登录：设备已受信任直接建立会话；新设备返回 [LoginDeviceVerificationRequired]
  /// （服务端已自动发码），调用方转入验证码输入界面后再调 [loginVerify]。
  /// [account] 接受邮箱或纯数字 Origin ID（契约 v1.2）。
  Future<LoginResult> login({required String account, required String password}) async {
    final result = await CloudClient.instance.login(
      account: account,
      password: password,
      deviceId: DeviceIdentity.deviceId(),
      deviceName: await DeviceIdentity.resolvedName(),
      devicePlatform: DeviceIdentity.platform(),
      appVersion: DeviceIdentity.appVersion(),
    );
    if (result case LoginOk(:final auth)) {
      await _applySession(auth);
    }
    return result;
  }

  /// 新设备验证码登录：重新校验密码 + 消费验证码 + 信任设备 + 建立会话。
  Future<void> loginVerify({
    required String account,
    required String password,
    required String code,
  }) async {
    final auth = await CloudClient.instance.loginVerify(
      account: account,
      password: password,
      code: code,
      deviceId: DeviceIdentity.deviceId(),
      deviceName: await DeviceIdentity.resolvedName(),
      devicePlatform: DeviceIdentity.platform(),
      appVersion: DeviceIdentity.appVersion(),
    );
    await _applySession(auth);
  }

  // ── 登录（验证码，邮箱不存在则自动注册）──────────────────────────────────

  /// 发送验证码登录用的验证码，返回 TTL（秒）。
  Future<int> sendCode(String email) => CloudClient.instance.sendCode(email);

  /// 提交验证码：邮箱不存在则自动注册，pending 用户自动激活，信任当前设备并建立会话。
  /// [nickname] 转发给服务端，仅在自动注册新用户分支生效（见 CloudClient.verifyCode）。
  Future<void> verifyCode({
    required String email,
    required String code,
    String? nickname,
  }) async {
    final auth = await CloudClient.instance.verifyCode(
      email: email,
      code: code,
      deviceId: DeviceIdentity.deviceId(),
      deviceName: await DeviceIdentity.resolvedName(),
      devicePlatform: DeviceIdentity.platform(),
      appVersion: DeviceIdentity.appVersion(),
      nickname: nickname,
    );
    await _applySession(auth);
  }

  // ── 会话管理 ─────────────────────────────────────────────────────────

  /// 退出登录：尽力通知服务端吊销 refreshToken（失败也不阻塞本地登出）。
  Future<void> logout() async {
    final rt = KvStore.instance.getString(_kRefreshTokenKey);
    if (rt != null && rt.isNotEmpty) {
      try {
        await CloudClient.instance.logout(rt);
      } catch (e, stack) {
        logError(_tag, 'server logout failed, clearing local session anyway', e, stack);
      }
    }
    await _clearSession();
  }

  /// 拉取 /me 刷新用户信息与套餐能力快照（如昵称/套餐在其他设备被更改后同步）。
  Future<void> refreshProfile() async {
    if (!isLoggedIn) return;
    final profile = await CloudClient.instance.me();
    _user = profile.user;
    _entitlements = profile.entitlements;
    await _persistUser();
    notifyListeners();
  }

  // ── 邮箱变更 ─────────────────────────────────────────────────────────

  /// 第一步：向当前绑定邮箱发送验证码，返回 TTL（秒）。
  Future<int> sendEmailChangeCode() =>
      CloudClient.instance.sendEmailChangeCode();

  /// 第二步：携原邮箱验证码向新邮箱发送验证码，返回 TTL（秒）。
  Future<int> sendEmailChangeNewCode({
    required String newEmail,
    required String oldCode,
  }) => CloudClient.instance.sendEmailChangeNewCode(
    newEmail: newEmail,
    oldCode: oldCode,
  );

  /// 第三步：校验原/新邮箱验证码并更新绑定邮箱，成功后刷新本地用户快照。
  Future<void> changeEmail({
    required String newEmail,
    required String oldCode,
    required String newCode,
  }) async {
    final profile = await CloudClient.instance.changeEmail(
      newEmail: newEmail,
      oldCode: oldCode,
      newCode: newCode,
    );
    _user = profile.user;
    _entitlements = profile.entitlements;
    await _persistUser();
    notifyListeners();
  }

  // ── 设备管理 ─────────────────────────────────────────────────────────

  /// 拉取设备名册并更新缓存（设置页与侧栏共用；成功后 notifyListeners）。
  Future<List<CloudDevice>> fetchDevices() async {
    final list = await CloudClient.instance.devices();
    _devices = list;
    notifyListeners();
    return list;
  }

  /// 刷新设备名册缓存（忽略返回值的语义化别名，供侧栏/服务静默调用）。
  Future<void> refreshDevices() async {
    try {
      await fetchDevices();
    } catch (_) {
      // 网络失败不清空既有缓存，静默忽略（侧栏容错，避免闪烁）。
    }
  }

  Future<CloudDevice> renameDevice(String id, String name) async {
    final updated = await CloudClient.instance.renameDevice(id, name);
    _devices = [for (final d in _devices) d.id == id ? updated : d];
    notifyListeners();
    return updated;
  }

  /// 删除设备；删除的恰好是当前设备时，服务端已吊销其全部会话，本地同步登出。
  Future<void> deleteDevice(CloudDevice device) async {
    await CloudClient.instance.deleteDevice(device.id);
    _devices = _devices.where((d) => d.id != device.id).toList();
    if (device.deviceId == currentDeviceId) {
      await _clearSession();
    } else {
      notifyListeners();
    }
  }

  // ── 内部 ─────────────────────────────────────────────────────────────

  Future<void> _applySession(AuthResponse auth) async {
    _user = auth.user;
    _entitlements = auth.entitlements;
    _status = CloudAuthStatus.authenticated;
    CloudClient.instance.accessToken = auth.accessToken;
    CloudClient.instance.refreshToken = auth.refreshToken;
    await KvStore.instance.setString(_kAccessTokenKey, auth.accessToken);
    await KvStore.instance.setString(_kRefreshTokenKey, auth.refreshToken);
    await _persistUser();
    notifyListeners();
  }

  Future<void> _persistUser() async {
    final u = _user;
    if (u == null) return;
    await KvStore.instance.setString(_kUserKey, jsonEncode(u.toJson()));
    await KvStore.instance.setString(
      _kEntitlementsKey,
      jsonEncode(_entitlements?.toJson() ?? const {}),
    );
  }

  Future<void> _clearSession() async {
    _user = null;
    _entitlements = null;
    _devices = const [];
    _status = CloudAuthStatus.unauthenticated;
    CloudClient.instance.accessToken = null;
    CloudClient.instance.refreshToken = null;
    await KvStore.instance.remove(_kAccessTokenKey);
    await KvStore.instance.remove(_kRefreshTokenKey);
    await KvStore.instance.remove(_kUserKey);
    await KvStore.instance.remove(_kEntitlementsKey);
    notifyListeners();
  }
}
