import type { Lang } from "@/i18n/config";
import type { AccountMessages } from "@/i18n/messages/account";
import type { CloudPlan } from "@/components/pricing/types";
import type { AccountDevice, AccountMe, AccountNotifyOverview, AccountOrder } from "@/lib/account/types";
import type { Failure, Resource } from "./resource";

export const SECTIONS = ["overview", "membership", "devices", "notifications", "security"] as const;
export type Section = (typeof SECTIONS)[number];

/** 各分区共用的上下文:身份数据、分区数据缓存与导航。 */
export interface DashboardCtx {
  lang: Lang;
  t: AccountMessages;
  me: AccountMe;
  /** 用接口返回的最新 `AccountMe` 直接替换(改昵称 / 改密码)。 */
  setMe: (me: AccountMe) => void;
  /** 重新拉取 `me`(付款成功后);失败保持旧数据。 */
  refreshMe: () => Promise<void>;
  /** 401 跳登录 / 403 停用态;返回 true = 已处理。 */
  guard: (failure: Failure) => boolean;
  go: (section: Section) => void;
  devices: Resource<AccountDevice[]>;
  notify: Resource<AccountNotifyOverview>;
  orders: Resource<AccountOrder[]>;
  plans: Resource<CloudPlan[]>;
}
