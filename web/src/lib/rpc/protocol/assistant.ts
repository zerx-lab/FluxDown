// AI 助手 DTO（native/protocol/src/assistant.rs）。
// 数据流：UI 经 `agent.assistant.*` 提问；agent 以账号身份调 FluxCloud，在本机执行云端下发的客户端工具，
// 非只读工具先挂起等待用户确认。进行中与最近结束的执行投影为 `AgentSnapshot.assistant`，
// 经 `assistantChanged` 整体替换推送；历史消息按需经 `agent.assistant.session` 从云端读取。

import type { JsonValue } from './common';
import type { ErrorReason } from './error';

/** 本机实现的客户端工具信封协议版本。 */
export const ASSISTANT_CLIENT_TOOL_PROTOCOL = 3;
/** `AssistantStateDto.runs` 保留的执行数上限（进行中的执行永不被裁掉）。 */
export const ASSISTANT_MAX_RUNS = 8;

/** 发起提问的界面渠道（Web 固定为 `web`）。 */
export type AssistantChannel = 'desktop' | 'web';

/** 会话类型：普通提问或反馈（反馈场景偏向起草问题单）。 */
export type AssistantSessionKind = 'ask' | 'feedback';

/** 客户端工具在本机的副作用：`network` 产生真实网络流量，`write` 改动设置或任务；非 `read` 需确认。 */
export type AssistantToolEffect = 'read' | 'network' | 'write';

/** 云端 `GET /api/v1/ai/config` 的本机投影。 */
export interface AssistantConfigDto {
  available: boolean;
  /** 不可用原因：`off` / `internal_only` / `plan_disabled`。 */
  reason?: string | null;
  maxMessageChars: number;
  imagesSupported: boolean;
  maxImages: number;
  maxImageBytes: number;
  /** `0` = 不限。 */
  dailyLimit: number;
  usedToday: number;
  /** 云端工具协议与本机一致、本机工具会被声明。 */
  toolsEnabled: boolean;
}

/** 一次执行的状态：`starting` 已发出提问、尚未收到云端首帧；`replied` 路由为固定回复。 */
export type AssistantRunStatus = 'starting' | 'running' | 'completed' | 'replied' | 'failed' | 'canceled';

/** `starting` / `running` 为进行中（`AssistantRunStatus::is_active`）。 */
export function isAssistantRunActive(status: AssistantRunStatus): boolean {
  return status === 'starting' || status === 'running';
}

export type AssistantToolStatus = 'running' | 'succeeded' | 'failed';

/** 非只读客户端工具的确认状态；`expired` = 超时未确认（云端按「未执行」交还模型）。 */
export type AssistantConfirmationState = 'pending' | 'approved' | 'declined' | 'expired';

/** 一次工具调用。服务端 / MCP 工具只有名称、状态与耗时；客户端工具另带本机执行信息。 */
export interface AssistantToolCallDto {
  callId: string;
  name: string;
  /** `client` / `server` / `mcp`。 */
  kind: string;
  status: AssistantToolStatus;
  durationMs?: number | null;
  /** 客户端工具的请求 ID（`clientToolCall.requestId`）。 */
  requestId?: string | null;
  effect?: AssistantToolEffect;
  /** 客户端工具的调用参数（确认卡片据此说明将要做什么）。 */
  arguments?: JsonValue | null;
  confirmation?: AssistantConfirmationState | null;
  confirmDeadlineUnixMs?: number | null;
  /** 本机执行结果摘要（客户端工具，截断后的文本）。 */
  outputPreview?: string | null;
}

/** 云端起草、待用户确认提交的问题单。 */
export interface AssistantIssueDto {
  id: string;
  sessionId?: string | null;
  kind: string;
  title: string;
  body: string;
  labels?: string[];
  /** `draft` / `pending_review` / `submitted` / `failed` / `rejected` / `discarded`。 */
  status: string;
  githubNumber?: number | null;
  githubUrl?: string | null;
  error?: string | null;
  reviewNote?: string | null;
  createdAt: string;
  updatedAt: string;
}

/** 执行中按到达顺序排列的条目（`type` 内部标记）。 */
export type AssistantRunItemDto =
  /** 一段回答文本（工具调用把回答切成多段）。 */
  | { type: 'text'; text: string }
  | { type: 'tool'; call: AssistantToolCallDto }
  | { type: 'issueDraft'; issue: AssistantIssueDto };

/** 一次执行（一问一答，含其间的工具调用）。 */
export interface AssistantRunDto {
  sessionId: string;
  /** 云端执行 ID（收到首帧前为 null）。 */
  runId?: string | null;
  status: AssistantRunStatus;
  question: string;
  startedAtUnixMs: number;
  items?: AssistantRunItemDto[];
  messageId?: string | null;
  error?: string | null;
  inputTokens?: number;
  outputTokens?: number;
}

/** AI 助手在本机的完整状态投影（`AgentSnapshot.assistant`，未登录时为空）。 */
export interface AssistantStateDto {
  /** 最近一次成功拉取的云端配置；未登录或从未拉取成功为 null。 */
  config?: AssistantConfigDto | null;
  loading?: boolean;
  lastErrorReason?: ErrorReason | null;
  lastErrorMessage?: string | null;
  /**
   * 是否显示 AI 助手入口（云端 `ai.mode` 决定，agent 已登录取 `ai/config.visible`、未登录取 `ai/status.visible`）：
   * `enabled` 所有人可见；`internal` 仅内部账号登录后可见；`off` 都不可见。与套餐无关。
   * UI 一律以它判定是否显示入口；旧 agent 快照缺该字段按 false 处理，初始 false，拉取失败保留上次值。
   */
  visible: boolean;
  /** 进行中与最近结束的执行（新的在后，至多 `ASSISTANT_MAX_RUNS` 条）。 */
  runs: AssistantRunDto[];
}

/** 空状态（对应 Rust `AssistantStateDto::default()`；登出时快照回到它）。 */
export const EMPTY_ASSISTANT_STATE: AssistantStateDto = {
  config: null,
  loading: false,
  lastErrorReason: null,
  lastErrorMessage: null,
  visible: false,
  runs: [],
};

/** 云端会话摘要。 */
export interface AssistantSessionDto {
  id: string;
  channel: string;
  kind: string;
  title: string;
  messageCount: number;
  /** 会话汇总：该会话最近一次的回答评价（`up` / `down`；按条评价见 `AssistantMessageDto.rating`，界面不用它）。 */
  rating?: string | null;
  /** 云端正在回答中。 */
  active?: boolean;
  createdAt: string;
  updatedAt: string;
}

/** 云端会话中的一条消息（`role`：`user` / `assistant` / `tool_call` / `tool_result`）。 */
export interface AssistantMessageDto {
  id: string;
  runId?: string | null;
  seq: number;
  role: string;
  content: string;
  toolName?: string | null;
  toolCallId?: string | null;
  toolKind?: string | null;
  isError?: boolean;
  durationMs?: number | null;
  /** `data:` URL 图片。 */
  images?: string[];
  /** 用户对该条回答的评价（`up` / `down`）；只有 `assistant` 消息可能非空。 */
  rating?: string | null;
  ratingComment?: string | null;
  createdAt: string;
}

/** `agent.assistant.sessions` 的结果。 */
export interface AssistantSessionPage {
  total: number;
  items: AssistantSessionDto[];
}

/** `agent.assistant.session` 的结果。 */
export interface AssistantSessionDetail {
  session: AssistantSessionDto;
  messages: AssistantMessageDto[];
  issues?: AssistantIssueDto[];
}

/** `agent.assistant.sessions` 参数。 */
export interface AssistantSessionsParams {
  limit?: number;
  offset?: number;
}

/** `agent.assistant.clearSessions` 的结果。 */
export interface AssistantClearedSessions {
  /** 本次从用户侧隐藏的会话数。 */
  cleared: number;
}

/** 只带会话 ID 的参数（`session` / `deleteSession` / `cancel`）。 */
export interface AssistantSessionIdParams {
  sessionId: string;
}

/** `agent.assistant.ask` 参数。`sessionId` 缺省 / 为空时先新建会话。 */
export interface AssistantAskParams {
  sessionId?: string | null;
  /** 仅新建会话时使用。 */
  kind?: AssistantSessionKind;
  channel?: AssistantChannel;
  text: string;
  /** `data:image/(png|jpeg|gif|webp);base64,...`。 */
  images?: string[];
  /** 界面语言（BCP 47），新建会话时随客户端信息上报。 */
  locale?: string | null;
}

/** `agent.assistant.ask` 的结果：执行随后经 `assistantChanged` 推送。 */
export interface AssistantAskResult {
  sessionId: string;
}

/** `agent.assistant.confirm` 参数。 */
export interface AssistantConfirmParams {
  requestId: string;
  approved: boolean;
}

/** `agent.assistant.rate` 参数：评价一条回答（`messageId` 为 `assistant` 消息 ID，即执行的 `AssistantRunDto.messageId`；`rating`：`up` / `down`，缺省 / null 清除）。 */
export interface AssistantRateParams {
  sessionId: string;
  messageId: string;
  rating?: string | null;
  comment?: string | null;
}

/** 提问文本里第 `number` 张附图（从 1 起）的占位符 `[Image #N]`（`assistant_image_placeholder`；云端把图片紧跟在占位符后发给模型）。 */
export function assistantImagePlaceholder(number: number): string {
  return `[Image #${number}]`;
}

/** `agent.assistant.confirmIssue` / `discardIssue` 参数；确认时可改标题与正文。 */
export interface AssistantIssueParams {
  issueId: string;
  title?: string | null;
  body?: string | null;
}
