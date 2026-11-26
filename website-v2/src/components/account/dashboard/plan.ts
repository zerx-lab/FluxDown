import type { AccountMessages } from "@/i18n/messages/account";
import type { AccountMe } from "@/lib/account/types";

/** 展示用套餐名:套餐快照的名称优先,其次免费版文案,最后原样显示 code。 */
export function planLabel(me: AccountMe, t: AccountMessages): string {
  return me.currentPlan?.name ?? (me.plan === "free" ? t.identity.planFree : me.plan);
}
