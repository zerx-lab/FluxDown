// agent.* 方法：每个包装器接收与 wire 完全一致的 params 对象，返回 wire 结果。
// 仅限 Web 可用的方法（不含读写宿主机路径的 platform.* / installFile / exportLogs 等）。

import { call } from '../client';
import { METHOD } from '../protocol';
import type {
  CloudNotifyChannelCreateParams,
  CloudNotifyChannelDto,
  CloudNotifyChannelIdParams,
  CloudNotifyChannelUpdateParams,
  CloudNotifyDeliveriesPage,
  CloudNotifyEmailCodeParams,
  CloudNotifyEmailCodeResult,
  CloudNotifyEmailVerifyParams,
  CloudNotifyDeliveriesParams,
  CloudNotifyPrivacyParams,
  CloudNotifyReportingParams,
  CloudNotifyStateDto,
  CloudNotifyTelegramBindDto,
  CloudNotifyTelegramBindStatusDto,
  CloudNotifyTelegramBindStatusParams,
  CloudNotifyTestResult,
  AgentLoginResult,
  AgentSessionDto,
  CaptureCreateGroupParams,
  CapturePreviewParams,
  CaptureResolveParams,
  CaptureResolveResult,
  CaptureSubmitParams,
  CaptureSubmitResult,
  CreateGroupResponse,
  ChangeEmailParams,
  ChangePasswordParams,
  ChangeNicknameParams,
  ChangeOriginIdParams,
  CheckOriginIdParams,
  CloudDevice,
  CloudOrder,
  CloudPlan,
  CloudProfile,
  CloudReferralCode,
  CloudReferralCodesResult,
  CloudReferralRecordsResult,
  CloudReferralSummary,
  CloudReferralValidateResult,
  InstalledPlugin,
  PrivatePluginInstallRequest,
  PrivatePluginListDto,
  PluginFavoriteResultDto,
  PluginFavoriteSetRequest,
  PluginFavoritesDto,
  DeviceIdParams,
  DeviceRenameParams,
  DiagnosticRepairParams,
  DiagnosticsReportDto,
  GatewayPatchParams,
  GatewayRevealTokenResult,
  GatewayStatusDto,
  JsonValue,
  LoginParams,
  LoginVerifyParams,
  LogPathsDto,
  OkResult,
  OrderCreateParams,
  OrderGetParams,
  OriginIdCheckResult,
  PageParams,
  PendingCaptureDto,
  PowerArmParams,
  PowerArmResult,
  PreferencesPatchParams,
  PreferencesPatchResult,
  RandomOriginIdResult,
  ReferralCreateCodeParams,
  ReferralDeleteCodeParams,
  ReferralListRecordsParams,
  ReferralValidateParams,
  RegisterParams,
  RegisterVerifyParams,
  LinkAddressParams,
  LinkApproveParams,
  LinkControlCredentialDto,
  LinkDeviceInfo,
  LinkDeviceParams,
  LinkDiscoveredPeer,
  LinkDiscoveryParams,
  LinkStopPairingParams,
  LinkDispatchParams,
  LinkDispatchResult,
  LinkPairBeginParams,
  LinkPairBeginResponse,
  LinkPairFinishParams,
  LinkPairFinishResponse,
  LinkRenameParams,
  LinkPairingCodeDto,
  RemoteCommandParams,
  RemoteDispatchParams,
  RemoteDispatchResult,
  RemoteTaskDto,
  ResolvePreviewResponse,
  ResetPasswordParams,
  SendCodeParams,
  SendNewEmailCodeParams,
  SyncLocalOnlyParams,
  SyncStatusDto,
  TtlResult,
  UpdateCheckParams,
  UpdateStatusDto,
  VerifyCodeParams,
} from '../protocol';

/** 对端用户有 60s 决策窗口，服务端最长等待 70s。 */
const PAIR_FINISH_TIMEOUT_MS = 75_000;
/**
 * Doctor 会真实写入保存目录并运行组件：agent 等 daemon 动态探测最长 30s、修复组件后再探测
 * 同样 30s，再叠加其余检查。留出余量，确保报告（含 `permission_probe` 超时那一行）先于客户端
 * 超时到达。
 */
const DIAGNOSTICS_TIMEOUT_MS = 60_000;

const session = {
  /** 当前会话；未登录为 null。 */
  get: () => call<AgentSessionDto | null>(METHOD.AGENT_SESSION_GET),
};

const auth = {
  /** 发码并建 pending 用户；结果一般为 `deviceVerificationRequired`（`ttlSeconds` 为验证码有效期）。 */
  register: (params: RegisterParams) => call<AgentLoginResult>(METHOD.AGENT_AUTH_REGISTER, params),
  registerVerify: (params: RegisterVerifyParams) =>
    call<AgentLoginResult>(METHOD.AGENT_AUTH_REGISTER_VERIFY, params),
  login: (params: LoginParams) => call<AgentLoginResult>(METHOD.AGENT_AUTH_LOGIN, params),
  /** 新设备验证码登录，重新校验密码并消费验证码。 */
  loginVerify: (params: LoginVerifyParams) =>
    call<AgentLoginResult>(METHOD.AGENT_AUTH_LOGIN_VERIFY, params),
  sendCode: (params: SendCodeParams) => call<TtlResult>(METHOD.AGENT_AUTH_SEND_CODE, params),
  verifyCode: (params: VerifyCodeParams) =>
    call<AgentLoginResult>(METHOD.AGENT_AUTH_VERIFY_CODE, params),
  logout: () => call<OkResult>(METHOD.AGENT_AUTH_LOGOUT),
  refreshProfile: () => call<CloudProfile>(METHOD.AGENT_AUTH_REFRESH_PROFILE),
  /** 向该邮箱发送重置密码验证码；未注册邮箱同样成功（防枚举）。 */
  sendPasswordResetCode: (params: SendCodeParams) =>
    call<TtlResult>(METHOD.AGENT_AUTH_SEND_PASSWORD_RESET_CODE, params),
  /** 重置成功后该账号全部设备会话失效，需用新密码重新登录。 */
  resetPassword: (params: ResetPasswordParams) => call<OkResult>(METHOD.AGENT_AUTH_RESET_PASSWORD, params),
};

const profile = {
  sendEmailCode: () => call<TtlResult>(METHOD.AGENT_PROFILE_SEND_EMAIL_CODE),
  sendNewEmailCode: (params: SendNewEmailCodeParams) =>
    call<TtlResult>(METHOD.AGENT_PROFILE_SEND_NEW_EMAIL_CODE, params),
  changeEmail: (params: ChangeEmailParams) =>
    call<CloudProfile>(METHOD.AGENT_PROFILE_CHANGE_EMAIL, params),
  randomOriginId: () => call<RandomOriginIdResult>(METHOD.AGENT_PROFILE_RANDOM_ORIGIN_ID),
  checkOriginId: (params: CheckOriginIdParams) =>
    call<OriginIdCheckResult>(METHOD.AGENT_PROFILE_CHECK_ORIGIN_ID, params),
  changeOriginId: (params: ChangeOriginIdParams) =>
    call<CloudProfile>(METHOD.AGENT_PROFILE_CHANGE_ORIGIN_ID, params),
  changeNickname: (params: ChangeNicknameParams) =>
    call<CloudProfile>(METHOD.AGENT_PROFILE_CHANGE_NICKNAME, params),
  /** 向当前绑定邮箱发送修改密码验证码。 */
  sendPasswordCode: () => call<TtlResult>(METHOD.AGENT_PROFILE_SEND_PASSWORD_CODE),
  /** 成功后其他设备会话失效，本设备保持登录并返回最新资料。 */
  changePassword: (params: ChangePasswordParams) =>
    call<CloudProfile>(METHOD.AGENT_PROFILE_CHANGE_PASSWORD, params),
};

const gateway = {
  get: () => call<GatewayStatusDto>(METHOD.AGENT_GATEWAY_GET),
  patch: (params: GatewayPatchParams) =>
    call<GatewayStatusDto>(METHOD.AGENT_GATEWAY_PATCH, params),
  /** 展示/复制用户 token；未配置为空串。 */
  revealToken: () => call<GatewayRevealTokenResult>(METHOD.AGENT_GATEWAY_REVEAL_TOKEN),
};

const device = {
  /** FluxCloud 原样响应 `{ devices }`，同时会刷新快照中的 cloudDevices。 */
  list: () => call<{ devices: CloudDevice[] }>(METHOD.AGENT_DEVICE_LIST),
  rename: (params: DeviceRenameParams) => call<CloudDevice>(METHOD.AGENT_DEVICE_RENAME, params),
  /** FluxCloud 原样响应（结构未固定）；删除当前设备会同时清除本机会话。 */
  delete: (params: DeviceIdParams) => call<JsonValue>(METHOD.AGENT_DEVICE_DELETE, params),
  /** 让同账号另一台在线设备下线释放在线名额（不删除设备）；不能对本机调用。 */
  disconnect: (params: DeviceIdParams) => call<OkResult>(METHOD.AGENT_DEVICE_DISCONNECT, params),
};

const preferences = {
  patch: (params: PreferencesPatchParams) =>
    call<PreferencesPatchResult>(METHOD.AGENT_PREFERENCES_PATCH, params),
};

const sync = {
  get: () => call<SyncStatusDto>(METHOD.AGENT_SYNC_GET),
  enable: () => call<OkResult>(METHOD.AGENT_SYNC_ENABLE),
  disable: () => call<OkResult>(METHOD.AGENT_SYNC_DISABLE),
  now: () => call<OkResult>(METHOD.AGENT_SYNC_NOW),
  /** 把同步目录键设为本设备专属 / 恢复同步。 */
  setLocalOnly: (params: SyncLocalOnlyParams) =>
    call<SyncStatusDto>(METHOD.AGENT_SYNC_SET_LOCAL_ONLY, params),
};

const link = {
  /** 展示本机配对码并开始广播。 */
  pairingCode: () => call<LinkPairingCodeDto>(METHOD.AGENT_LINK_PAIRING_CODE),
  stopPairing: (params?: LinkStopPairingParams) => call<OkResult>(METHOD.AGENT_LINK_STOP_PAIRING, params),
  discoverySet: (params: LinkDiscoveryParams) =>
    call<OkResult>(METHOD.AGENT_LINK_DISCOVERY_SET, params),
  probe: (params: LinkAddressParams) => call<LinkDiscoveredPeer>(METHOD.AGENT_LINK_PROBE, params),
  pairBegin: (params: LinkPairBeginParams) =>
    call<LinkPairBeginResponse>(METHOD.AGENT_LINK_PAIR_BEGIN, params),
  pairFinish: (params: LinkPairFinishParams) =>
    call<LinkPairFinishResponse>(METHOD.AGENT_LINK_PAIR_FINISH, params, { timeoutMs: PAIR_FINISH_TIMEOUT_MS }),
  approve: (params: LinkApproveParams) => call<OkResult>(METHOD.AGENT_LINK_APPROVE, params),
  remove: (params: LinkDeviceParams) => call<OkResult>(METHOD.AGENT_LINK_REMOVE, params),
  rename: (params: LinkRenameParams) => call<LinkDeviceInfo>(METHOD.AGENT_LINK_RENAME, params),
  refresh: () => call<LinkDeviceInfo[]>(METHOD.AGENT_LINK_REFRESH),
  controlCredential: (params: LinkDeviceParams) =>
    call<LinkControlCredentialDto>(METHOD.AGENT_LINK_CONTROL_CREDENTIAL, params),
  dispatch: (params: LinkDispatchParams) =>
    call<LinkDispatchResult>(METHOD.AGENT_LINK_DISPATCH, params),
};

const remote = {
  list: () => call<RemoteTaskDto[]>(METHOD.AGENT_REMOTE_LIST),
  reconnect: () => call<{ accepted: boolean }>(METHOD.AGENT_REMOTE_RECONNECT),
  dispatch: (params: RemoteDispatchParams) =>
    call<RemoteDispatchResult>(METHOD.AGENT_REMOTE_DISPATCH, params),
  command: (params: RemoteCommandParams) => call<OkResult>(METHOD.AGENT_REMOTE_COMMAND, params),
};

const plan = {
  /** 公开套餐目录（含下架但仍展示的套餐）。 */
  list: () => call<CloudPlan[]>(METHOD.AGENT_PLAN_LIST),
};

const order = {
  create: (params: OrderCreateParams) => call<CloudOrder>(METHOD.AGENT_ORDER_CREATE, params),
  get: (params: OrderGetParams) => call<CloudOrder>(METHOD.AGENT_ORDER_GET, params),
  /** 本人订单，创建时间倒序，最多 20 条。 */
  list: () => call<CloudOrder[]>(METHOD.AGENT_ORDER_LIST),
};

const referral = {
  summary: () => call<CloudReferralSummary>(METHOD.AGENT_REFERRAL_SUMMARY),
  listCodes: (params?: PageParams) =>
    call<CloudReferralCodesResult>(METHOD.AGENT_REFERRAL_LIST_CODES, params),
  createCode: (params?: ReferralCreateCodeParams) =>
    call<CloudReferralCode>(METHOD.AGENT_REFERRAL_CREATE_CODE, params),
  /** FluxCloud 原样响应（结构未固定）。 */
  deleteCode: (params: ReferralDeleteCodeParams) =>
    call<JsonValue>(METHOD.AGENT_REFERRAL_DELETE_CODE, params),
  listRecords: (params?: ReferralListRecordsParams) =>
    call<CloudReferralRecordsResult>(METHOD.AGENT_REFERRAL_LIST_RECORDS, params),
  validate: (params: ReferralValidateParams) =>
    call<CloudReferralValidateResult>(METHOD.AGENT_REFERRAL_VALIDATE, params),
};

const pluginFavorite = {
  list: () => call<PluginFavoritesDto>(METHOD.AGENT_PLUGIN_FAVORITE_LIST),
  set: (params: PluginFavoriteSetRequest) =>
    call<PluginFavoriteResultDto>(METHOD.AGENT_PLUGIN_FAVORITE_SET, params),
};

/** 慢调用：agent 取票据 → 拉包 → 校验 → daemon 安装，与 `daemon.plugin.marketInstall` 同量级。 */
const PRIVATE_INSTALL_TIMEOUT_MS = 120_000;

/** FluxCloud 私有插件商店：目录（未登录 `signedIn=false`）与安装。 */
const privatePlugin = {
  list: () => call<PrivatePluginListDto>(METHOD.AGENT_PRIVATE_PLUGIN_LIST),
  install: (params: PrivatePluginInstallRequest) =>
    call<InstalledPlugin>(METHOD.AGENT_PRIVATE_PLUGIN_INSTALL, params, { timeoutMs: PRIVATE_INSTALL_TIMEOUT_MS }),
};

const capture = {
  submit: (params: CaptureSubmitParams) =>
    call<CaptureSubmitResult>(METHOD.AGENT_CAPTURE_SUBMIT, params),
  /** 等待确认的外部捕获队列。 */
  list: () => call<PendingCaptureDto[]>(METHOD.AGENT_CAPTURE_LIST),
  resolve: (params: CaptureResolveParams) =>
    call<CaptureResolveResult>(METHOD.AGENT_CAPTURE_RESOLVE, params),
  /** 保留浏览器上下文的只读预解析，不消费捕获事务。 */
  preview: (params: CapturePreviewParams) =>
    call<ResolvePreviewResponse>(METHOD.AGENT_CAPTURE_PREVIEW, params, { timeoutMs: 90_000 }),
  createGroup: (params: CaptureCreateGroupParams) =>
    call<CreateGroupResponse>(METHOD.AGENT_CAPTURE_CREATE_GROUP, params),
};

const diagnostics = {
  run: () =>
    call<DiagnosticsReportDto>(METHOD.AGENT_DIAGNOSTICS_RUN, undefined, {
      timeoutMs: DIAGNOSTICS_TIMEOUT_MS,
    }),
  /** 成功返回 `{ ok: true }` 或所转发 daemon RPC 的结果（`refreshTrackers` 等）。 */
  repair: (params: DiagnosticRepairParams) =>
    call<JsonValue>(METHOD.AGENT_DIAGNOSTICS_REPAIR, params, {
      timeoutMs: DIAGNOSTICS_TIMEOUT_MS,
    }),
  logPaths: () => call<LogPathsDto>(METHOD.AGENT_DIAGNOSTICS_LOG_PATHS),
};

const update = {
  check: (params?: UpdateCheckParams) => call<UpdateStatusDto>(METHOD.AGENT_UPDATE_CHECK, params),
  download: () => call<UpdateStatusDto>(METHOD.AGENT_UPDATE_DOWNLOAD),
  install: () => call<UpdateStatusDto>(METHOD.AGENT_UPDATE_INSTALL),
  cancel: () => call<UpdateStatusDto>(METHOD.AGENT_UPDATE_CANCEL),
};

const power = {
  /** 完成后关机；无活跃任务时被拒绝（invalidArgument）。 */
  arm: (params: PowerArmParams) => call<PowerArmResult>(METHOD.AGENT_POWER_ARM, params),
  disarm: () => call<OkResult>(METHOD.AGENT_POWER_DISARM),
};

const cloudNotify = {
  /** 读本机缓存；过期时 agent 后台刷新并经 `cloudNotifyChanged` 推送。 */
  get: () => call<CloudNotifyStateDto>(METHOD.AGENT_CLOUD_NOTIFY_GET),
  refresh: () => call<CloudNotifyStateDto>(METHOD.AGENT_CLOUD_NOTIFY_REFRESH),
  setReporting: (params: CloudNotifyReportingParams) =>
    call<CloudNotifyStateDto>(METHOD.AGENT_CLOUD_NOTIFY_SET_REPORTING, params),
  setPrivacy: (params: CloudNotifyPrivacyParams) =>
    call<CloudNotifyStateDto>(METHOD.AGENT_CLOUD_NOTIFY_SET_PRIVACY, params),
  createChannel: (params: CloudNotifyChannelCreateParams) =>
    call<CloudNotifyChannelDto>(METHOD.AGENT_CLOUD_NOTIFY_CREATE_CHANNEL, params),
  updateChannel: (params: CloudNotifyChannelUpdateParams) =>
    call<CloudNotifyChannelDto>(METHOD.AGENT_CLOUD_NOTIFY_UPDATE_CHANNEL, params),
  deleteChannel: (params: CloudNotifyChannelIdParams) =>
    call<OkResult>(METHOD.AGENT_CLOUD_NOTIFY_DELETE_CHANNEL, params),
  /** 同步发送一条测试消息（不计额度）。 */
  testChannel: (params: CloudNotifyChannelIdParams) =>
    call<CloudNotifyTestResult>(METHOD.AGENT_CLOUD_NOTIFY_TEST_CHANNEL, params),
  telegramBindStart: () => call<CloudNotifyTelegramBindDto>(METHOD.AGENT_CLOUD_NOTIFY_TELEGRAM_BIND_START),
  telegramBindStatus: (params: CloudNotifyTelegramBindStatusParams) =>
    call<CloudNotifyTelegramBindStatusDto>(METHOD.AGENT_CLOUD_NOTIFY_TELEGRAM_BIND_STATUS, params),
  deliveries: (params?: CloudNotifyDeliveriesParams) =>
    call<CloudNotifyDeliveriesPage>(METHOD.AGENT_CLOUD_NOTIFY_DELIVERIES, params),
  sendEmailCode: (params: CloudNotifyEmailCodeParams) =>
    call<CloudNotifyEmailCodeResult>(METHOD.AGENT_CLOUD_NOTIFY_SEND_EMAIL_CODE, params),
  verifyEmail: (params: CloudNotifyEmailVerifyParams) =>
    call<OkResult>(METHOD.AGENT_CLOUD_NOTIFY_VERIFY_EMAIL, params),
};


// 桌面集成：在 agent 所在主机上打开 / 定位任务产物。仅当本连接的 hello 带
// `agent.openTaskFiles` 能力时可用（见 downloads/model/actions.ts）；否则 agent 返回 Unsupported。
// `copyTaskFiles` 另需 `agent.copyTaskFiles` 能力。
const platform = {
  openTask: (params: { taskId: string }) => call<OkResult>(METHOD.AGENT_PLATFORM_OPEN_TASK, params),
  revealTask: (params: { taskId: string }) =>
    call<OkResult>(METHOD.AGENT_PLATFORM_REVEAL_TASK, params),
  /** 产物整体写入主机剪贴板；任一产物不在磁盘上 → `notFound`，不写半份。 */
  copyTaskFiles: (params: { taskIds: string[] }) =>
    call<OkResult>(METHOD.AGENT_PLATFORM_COPY_TASK_FILES, params),
};

export const agent = {
  session,
  auth,
  profile,
  gateway,
  device,
  preferences,
  sync,
  link,
  remote,
  plan,
  order,
  referral,
  pluginFavorite,
  privatePlugin,
  capture,
  diagnostics,
  update,
  power,
  cloudNotify,
  platform,
};
