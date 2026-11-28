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

// ───────────── AI 助手（`/api/account/ai/*`，FluxCloud `/api/v1/ai/*`，裁剪见 `ai-routes.ts`） ─────────────

/** 不可用原因：`off` 未开放 / `internal_only` 只对内部账号开放 / `plan_disabled` 当前套餐不含 AI 助手。 */
export type AiUnavailableReason = "off" | "internal_only" | "plan_disabled";

/** `GET /api/account/ai/status`：各端 AI 入口是否显示（匿名取公开口径，有会话按本人身份，由 FluxCloud 判定）。 */
export interface AiStatus {
  visible: boolean;
}

/** `GET /api/account/ai/config`。 */
export interface AiConfig {
  available: boolean;
  reason: AiUnavailableReason | null;
  /** 入口是否显示：与 `available` 无关（套餐不含助手时入口照常显示，只是不可用）。 */
  visible: boolean;
  /** 单次提问的字数上限（不可用时为 0）。 */
  maxMessageChars: number;
  /** 每日提问次数上限；0 = 不限。 */
  dailyLimit: number;
  /** 今日（UTC+08:00 自然日）已用的提问次数，固定回复不计。 */
  usedToday: number;
  /** 当前能否带图提问（FluxCloud 配有支持图片的服务商）。 */
  imagesSupported: boolean;
  /** 每次提问最多附带的图片数（不可用时为 0）。 */
  maxImages: number;
  /** 单张图片的字节上限（解码后）。 */
  maxImageBytes: number;
}

/** 对一条回答的评价。 */
export type AiRating = "up" | "down";

export type AiSessionKind = "ask" | "feedback";

export interface AiSession {
  id: string;
  /** 创建入口：`website` 官网 / `desktop` 桌面 / `web` Web 管理界面。 */
  channel: string;
  kind: AiSessionKind;
  /** 首条提问的首行（空 = 尚未提问）。 */
  title: string;
  messageCount: number;
  /** 会话汇总：最近一次回答评价（界面按回答评价，不用它表示某条回答的状态）。 */
  rating: AiRating | null;
  /** 正在回答中。 */
  active: boolean;
  /** RFC 3339 */
  createdAt: string;
  /** RFC 3339 */
  updatedAt: string;
}

/** `GET /api/account/ai/sessions`：最近更新在前。 */
export interface AiSessionPage {
  total: number;
  items: AiSession[];
}

export interface AiMessage {
  id: string;
  runId: string | null;
  seq: number;
  /** `membership_offer` 由官网 BFF 合成（上游 `offer_membership` 成功的工具结果），无文本，仅表示该轮（同 `runId`）要显示开通会员卡片。 */
  role: "user" | "assistant" | "membership_offer";
  content: string;
  /** 提问附带的图片 id（只有用户消息可能非空），经 `/api/account/ai/attachments/<id>` 取。 */
  images: string[];
  /** 对这条回答的评价（只有助手消息有值；旧版 FluxCloud 不带）。 */
  rating?: AiRating | null;
  ratingComment?: string | null;
  /** RFC 3339 */
  createdAt: string;
}

export type AiIssueStatus = "draft" | "pending_review" | "submitted" | "rejected" | "discarded" | "failed";

/** 模型起草的问题单；`draft` 待用户确认后提交 GitHub 或进入后台审核。 */
export interface AiIssue {
  id: string;
  sessionId: string | null;
  /** `bug` / `feature`。 */
  kind: string;
  title: string;
  body: string;
  labels: string[];
  status: AiIssueStatus;
  githubNumber: number | null;
  githubUrl: string | null;
  /** 后台驳回时的备注。 */
  reviewNote: string | null;
  /** RFC 3339 */
  createdAt: string;
  /** RFC 3339 */
  updatedAt: string;
}

/** `GET /api/account/ai/sessions/{id}`。 */
export interface AiSessionDetail {
  session: AiSession;
  messages: AiMessage[];
  issues: AiIssue[];
}

export type AiToolKind = "client" | "server" | "mcp";

/** 异常结束的原因：轮数用尽 / 无回答 / 超时 / 取消 / 其它失败。旧版 FluxCloud 不带。 */
export type AiFailReason = "maxTurns" | "noAnswer" | "timeout" | "canceled" | "error";

/** `POST /api/account/ai/sessions/{id}/messages` 的 SSE 事件（每帧 `data` 一个）。 */
export type AiStreamEvent =
  | { type: "run"; runId: string; sessionId: string }
  | { type: "text"; delta: string }
  /** 本轮回答被重试：丢弃本轮已显示的文本。 */
  | { type: "turnRetried"; turn: number }
  | { type: "toolStarted"; callId: string; name: string; kind: AiToolKind }
  | { type: "toolFinished"; callId: string; name: string; kind: AiToolKind; isError: boolean; durationMs: number | null }
  | { type: "issueDraft"; issue: AiIssue }
  | { type: "done"; status: "completed" | "replied"; messageId: string | null; text: string }
  | { type: "error"; status: "failed" | "canceled"; reason?: AiFailReason; message: string };
