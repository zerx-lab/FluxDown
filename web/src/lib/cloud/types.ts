// FluxCloud 云账户 —— Wire 契约 v1（camelCase，见 FluxCloud/server），与本地下载器
// api.ts 的 types.ts 完全独立，互不引用。

/** 用户状态：0=active 1=disabled 2=pending（待邮箱验证）。 */
export type CloudUserStatus = 'active' | 'disabled' | 'pending'

export interface CloudUser {
  id: string
  email: string
  nickname: string
  plan: string
  status: CloudUserStatus
  /** Origin ID(v1.2 新增):类 QQ 号唯一数字身份,从 10001 起严格递增;pending 用户为 null。 */
  originId: number | null
  createdAt: string
  lastLoginAt?: string
}

/** 套餐能力集：服务端自由演进字段，本文件只按需声明已知字段，未知字段仍可原样读取。 */
export interface Entitlements {
  maxSyncDevices?: number
  [key: string]: unknown
}

/** 受信任设备（DeviceDto，v1.1 增补 lastIp/appVersion，均可空；v1.3 增补 isOnline/isCurrent，
 *  多设备协同用，见 mdc §1.2）。 */
export interface CloudDevice {
  id: string
  deviceId: string
  name: string
  platform?: string
  /** 最近登录 IP，服务端按 X-Forwarded-For/X-Real-IP 记录，可能为空。 */
  lastIp?: string
  /** 客户端版本号，登录/信任设备时上报，可能为空（如旧版客户端未上报）。 */
  appVersion?: string
  createdAt: string
  lastSeenAt: string
  /** 该设备当前是否有活跃 SSE 连接（服务端 PresenceRegistry 判定）。 */
  isOnline?: boolean
  /** 是否为发起本次请求的设备（服务端按请求头 deviceId 比对）。 */
  isCurrent?: boolean
}

/** 登录/注册验证/验证码登录 成功后的统一响应。 */
export interface AuthResponse {
  accessToken: string
  refreshToken: string
  expiresIn: number
  user: CloudUser
  entitlements: Entitlements
  device: CloudDevice
}

/** POST /auth/login 的 tagged 响应：设备已受信任直接下发令牌，新设备则要求邮箱验证码。 */
export type LoginResult =
  | { status: 'ok'; auth: AuthResponse }
  | { status: 'deviceVerificationRequired'; ttlSeconds: number }

/** GET /me 响应：UserDto 字段打平 + entitlements。 */
export interface CloudProfile extends CloudUser {
  entitlements: Entitlements
}

/** POST /auth/register、/auth/code/send 等发码接口的响应。 */
export interface TtlResponse {
  ttlSeconds: number
}

/** GET /devices 响应。 */
export interface DevicesResponse {
  devices: CloudDevice[]
}

/** 跨设备任务状态机（cross_device_tasks.status，见 mdc §1.1）。 */
export type RemoteTaskStatus = 'pending' | 'accepted' | 'downloading' | 'paused' | 'completed' | 'failed' | 'canceled'

/** 跨设备任务（RemoteTaskDto）：downloadedBytes/speed/progress 来自服务端内存快照（无则 0），
 *  绝不落库、绝不轮询，靠 `GET /tasks/events` SSE 增量回流（见 mdc §1.3/§1.5）。 */
export interface RemoteTask {
  id: string
  fromDevice: string
  toDevice: string
  url: string
  saveDir?: string
  fileName: string
  status: RemoteTaskStatus
  totalBytes?: number
  downloadedBytes: number
  speed: number
  progress: number
  error?: string
  createdAt: string
  updatedAt: string
}

/** GET /tasks/remote 响应。 */
export interface RemoteTasksResponse {
  tasks: RemoteTask[]
}
/** GET /cdn/config 响应 resolvers[]（snake_case wire，直接对应引擎 config 表键约定，
 *  与本文件其余 camelCase 模型不同——对齐桌面端 cloud_models.dart CdnConfig）。 */
export interface CdnResolverEntry {
  url: string
  ecs: boolean
}

/** GET /cdn/config 响应 ecs_subnets[]：resolver ECS 查询的地域先验，客户端只消费 subnet。 */
export interface CdnEcsSubnetEntry {
  region: string
  isp: string
  subnet: string
}

/** GET /cdn/config 响应：CDN 多节点聚合下载云端配置快照（P1 §四 + P2 §五契约）。 */
export interface CdnConfig {
  revision: number
  enabled: boolean
  max_nodes: number
  resolvers: CdnResolverEntry[]
  ecs_subnets: CdnEcsSubnetEntry[]
}

/** fetchCdnConfig 结果：304 命中时 notModified=true，etag/config 均为 null。 */
export interface CdnConfigResult {
  notModified: boolean
  etag: string | null
  config: CdnConfig | null
}

/** 服务端错误统一形态 `{code, message}`，附带 HTTP 状态码方便按 code/status 分支处理。 */
export class CloudApiError extends Error {
  code: string
  status: number
  constructor(code: string, message: string, status: number) {
    super(message)
    this.code = code
    this.status = status
  }
}
