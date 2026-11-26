/**
 * 账户中心浏览器侧 wire 类型（官网 BFF `/api/account/*` 的响应形状，camelCase）。
 *
 * 字段口径以 FluxCloud `server/crates/server/src/dto.rs` 为准（`UserDto` / `MeResponse` / `DeviceDto` /
 * `OrderDto` / `notify::dto::OverviewDto`）；BFF 只裁掉浏览器用不到的字段，不改名、不改语义。
 * 访问令牌与刷新令牌只在服务端 HttpOnly Cookie 里，任何响应都不含令牌。
 */
import type { CloudPlan } from "@/components/pricing/types";

export interface AccountEntitlements {
  /** 受信任 App 设备数上限；0 = 不限。 */
  maxDevices: number;
  /** 同时在线设备数上限；0 = 不限。 */
  maxOnlineDevices: number;
  originIdEdit: boolean;
  notifyEnabled: boolean;
  /** 0 = 不限。 */
  notifyDailyQuota: number;
  /** 0 = 不限。 */
  notifyMonthlyQuota: number;
  /** 0 = 不限。 */
  notifyMaxChannels: number;
}

export interface AccountSubscription {
  planCode: string;
  planName: string;
  /** RFC 3339 */
  startsAt: string;
  /** RFC 3339 */
  endsAt: string;
}

/** `GET /api/account/me`（FluxCloud `GET /api/v1/me`）。 */
export interface AccountMe {
  id: string;
  email: string;
  nickname: string;
  /** 当前套餐 code（`free` / `pro` / 自定义）。 */
  plan: string;
  status: "active" | "pending" | "disabled";
  originId: number | null;
  originIdChanged: boolean;
  membershipOrdinal: number | null;
  hasPassword: boolean;
  /** RFC 3339 */
  createdAt: string;
  /** RFC 3339 */
  lastLoginAt: string | null;
  entitlements: AccountEntitlements;
  /** 当前套餐展示快照（不受上架状态影响）；null = 套餐行缺失，按 `plan` code 展示。 */
  currentPlan: CloudPlan | null;
  /** 差价升级抵扣基数（分）。 */
  purchaseCreditMinor: number;
  /** 生效中的周期会员卡；null = 无。 */
  activeSubscription: AccountSubscription | null;
}

/** `GET /api/account/devices` 的条目：只含 App 设备（Web 会话由 FluxCloud 排除）。 */
export interface AccountDevice {
  /** 设备行 id：改名 / 移除用它。 */
  id: string;
  name: string;
  /** `windows` / `macos` / `linux` / `android` / `ios` / 其他或 null。 */
  platform: string | null;
  lastIp: string | null;
  appVersion: string | null;
  /** RFC 3339 */
  createdAt: string;
  /** RFC 3339 */
  lastSeenAt: string;
  isOnline: boolean;
}

export interface AccountPayNetwork {
  key: string;
  name: string;
  chainId: number;
}

/** `GET /api/account/orders` 的条目（本人最近 20 单，按创建时间倒序）。 */
export interface AccountOrder {
  orderNo: string;
  planCode: string;
  planName: string;
  /** `pending` / `paid` / `expired` / `failed` / `refunded` 等。 */
  status: string;
  amountMinor: number;
  listPriceMinor: number;
  creditMinor: number;
  referralCode: string | null;
  referralDiscountMinor: number;
  upgradeFromPlan: string | null;
  currency: string;
  campaignName: string | null;
  stageLabel: string | null;
  /** RFC 3339 */
  createdAt: string;
  /** RFC 3339 */
  paidAt: string | null;
  /** RFC 3339 */
  expiresAt: string;
  /** `wxpay` / `usdt` / `waffo` */
  payMethod: string;
  payAmountMicros: number | null;
}

export interface AccountNotifyUsage {
  dailyUsed: number;
  /** 0 = 不限。 */
  dailyLimit: number;
  monthlyUsed: number;
  /** 0 = 不限。 */
  monthlyLimit: number;
  /** RFC 3339 */
  dailyResetAt: string;
  /** RFC 3339 */
  monthlyResetAt: string;
}

export interface AccountNotifyChannel {
  id: string;
  /** `email` / `telegram` */
  kind: string;
  name: string;
  enabled: boolean;
  events: string[];
  /** 打码后的投递目标展示文本。 */
  target: string;
  /** `ok` / `failing` */
  status: string;
  lastError?: string;
  /** RFC 3339 */
  lastDeliveredAt?: string;
  /** RFC 3339 */
  createdAt: string;
}

/** `GET /api/account/notifications`（FluxCloud `GET /api/v1/notifications/overview`）。 */
export interface AccountNotifyOverview {
  enabled: boolean;
  usage: AccountNotifyUsage;
  /** 0 = 不限。 */
  maxChannels: number;
  channels: AccountNotifyChannel[];
}

/** 登录类接口的结果：`ok` = 已写入会话 Cookie；`verify` = 新浏览器需邮箱验证码（已发到账号邮箱）。 */
export type AccountLoginResult = { status: "ok" } | { status: "verify"; ttlSeconds: number };

/** 发码类接口的结果。 */
export interface AccountCodeSent {
  ttlSeconds: number;
}
