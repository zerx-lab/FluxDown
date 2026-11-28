/** FluxCloud 拒绝提问的错误码：本对话已达单会话提问上限。 */
export const SESSION_LIMIT_CODE = "ai_session_limit";

/** 剩余提问次数不超过这个数时，输入区附近显示「还可提问 N 次」。 */
export const REMAINING_HINT_MAX = 3;

export interface SessionLimitView {
  /** 已达上限且没有进行中的执行：输入区换成提示卡，隐藏重新生成 / 编辑重发。 */
  reached: boolean;
  /** 剩余 1..=3 次时的剩余数；其余情况 null。 */
  remaining: number | null;
}

const UNLIMITED: SessionLimitView = { reached: false, remaining: null };

/**
 * 单会话提问上限的界面状态。`limit` = `ai/config.sessionLimit`，`used` = 当前会话详情的 `questionsUsed`。
 * 上限为 0 / 缺失（含旧版 FluxCloud）或还没有会话详情（新对话、加载中）= 不限；`running` = 当前会话有进行中的执行。
 * 计数以服务端为准，客户端不自己累加。
 */
export function sessionLimitView(limit: number | undefined, used: number | undefined, running: boolean): SessionLimitView {
  if (limit === undefined || used === undefined || !Number.isFinite(limit) || !Number.isFinite(used) || limit <= 0) return UNLIMITED;
  const left = limit - used;
  if (left <= 0) return running ? UNLIMITED : { reached: true, remaining: null };
  return { reached: false, remaining: left <= REMAINING_HINT_MAX ? left : null };
}

/** 提问在开始回答前被拒的分类：会话忙（409）/ 会话上限 / 其它（额度用完、网络等按错误码取文案）。 */
export type SendErrorKind = "busy" | "sessionLimit" | "other";

export function sendErrorKind(status: number, code: string): SendErrorKind {
  if (code === SESSION_LIMIT_CODE) return "sessionLimit";
  return status === 409 ? "busy" : "other";
}
