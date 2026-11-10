/**
 * 站点公告(单一数据源):顶部公告条、强提示弹窗与 /announcements 列表共用。
 *
 * - `active: false` 的条目只出现在列表里(标记为已结束)。
 * - 顶部公告条显示最新一条 active 且非弹窗的公告;弹窗显示 active 且带 `popup` 的公告。
 * - 关闭记录存 localStorage[`DISMISSED_KEY`](id 数组),与旧站同键,旧的关闭记录继续有效。
 */
import type { Lang } from "@/i18n/config";
import { CLOUD_PROMO, cloudPromoCopy } from "@/lib/cloud-promo";

export type Localized = Record<Lang, string>;

export interface AnnouncementPopup {
  title: Localized;
  body: Localized;
  /** 仿冒域名,弹窗中以删除线列出。 */
  blocked: string[];
}

export interface Announcement {
  id: string;
  /** ISO 日期 YYYY-MM-DD。 */
  date: string;
  /**
   * ISO 截止时刻；过期后公告条与「进行中」列表不再展示（仍可出现在历史列表时需保持 active 语义由调用方处理）。
   * 缺省 = 无自动过期。
   */
  endsAt?: string;
  active: boolean;
  text: Localized;
  /** 无语言前缀的站内路径,渲染时经 `href()` 本地化。 */
  link?: string;
  /** 以模态弹窗强提示(如安全警告),不进公告条。 */
  popup?: AnnouncementPopup;
}

export const DISMISSED_KEY = "fluxdown-dismissed-announcements";

/** 官方域名;首项为主域名(www 由反代 301 到主域名,fluxdown.zerx.dev 为旧域名,长期保留)。 */
export const OFFICIAL_SITES = ["https://fluxdown.com", "https://www.fluxdown.com", "https://fluxdown.zerx.dev"] as const;

export const ANNOUNCEMENTS: Announcement[] = [
  {
    id: CLOUD_PROMO.announcementId,
    date: "2026-11-04",
    active: true,
    endsAt: CLOUD_PROMO.endAt,
    link: CLOUD_PROMO.planPath,
    text: {
      en: cloudPromoCopy.en.annText,
      zh: cloudPromoCopy.zh.annText,
    },
  },
  {
    id: "gpui-client",
    date: "2026-09-30",
    active: true,
    link: "/announcements/gpui-client",
    text: {
      en: "FluxDown 0.5.0 ships the native GPUI client — a note from the author on why the whole interface was rebuilt.",
      zh: "FluxDown 0.5.0 原生 GPUI 客户端正式发布——作者来信:为什么我把整个界面从头重写了一遍。",
    },
  },
  {
    id: "pricing-vote-open",
    date: "2026-07-29",
    active: true,
    link: "/pricing/vote",
    text: {
      en: "Pricing for the premium cloud features is being shaped with the community. Cast a vote and share your take.",
      zh: "高级云端功能的定价正在社区共创中,来投一票、聊聊你的想法。",
    },
  },
  {
    id: "security-warning-fake-site",
    date: "2026-06-25",
    active: false,
    link: "/security-alert",
    text: {
      en: "Security alert: the fake site fluxdown.com.cn distributes the “SilverFox” trojan. Do not download anything from it.",
      zh: "安全警告:仿冒网站 fluxdown.com.cn 正在传播「银狐」木马,切勿从该网站下载任何文件。",
    },
    popup: {
      title: { en: "Security alert", zh: "安全警告" },
      body: {
        en: "Unofficial sites are impersonating FluxDown and distributing installers that carry the “SilverFox” remote-access trojan. Don't download or run anything from them.",
        zh: "有非官方网站冒充 FluxDown,传播携带「银狐」远控木马的恶意安装包。请勿在这些网站下载或运行任何文件,谨防资金与隐私损失。",
      },
      blocked: ["fluxdown.com.cn"],
    },
  },
  {
    id: "telegram-group-created",
    date: "2026-03-27",
    active: true,
    link: "/telegram-group",
    text: {
      en: "The FluxDown Telegram group is live. Join the international community.",
      zh: "FluxDown Telegram 群已创建,欢迎加入国际社区交流。",
    },
  },
  {
    id: "qq-group-created",
    date: "2026-02-20",
    active: true,
    link: "/qq-group",
    text: {
      en: "The FluxDown QQ group is live — group number 832143651.",
      zh: "FluxDown QQ 群已创建,群号 832143651,欢迎加入社区交流。",
    },
  },
  {
    id: "vote-community-group",
    date: "2026-02-16",
    active: false,
    link: "/vote",
    text: {
      en: "Vote: should we start a WeChat group, a QQ group or an Official Account?",
      zh: "投票:建微信群、QQ 群还是公众号?",
    },
  },
  {
    id: "logo-vote-active",
    date: "2026-02-15",
    active: false,
    link: "/logo-vote",
    text: {
      en: "Logo vote: pick the new FluxDown logo or submit your own design.",
      zh: "Logo 投票:为 FluxDown 新 Logo 投票,或提交你的原创设计。",
    },
  },
];

const byDateDesc = (a: Announcement, b: Announcement) => b.date.localeCompare(a.date);

function withinWindow(a: Announcement, nowMs = Date.now()): boolean {
  if (!a.endsAt) return true;
  const end = Date.parse(a.endsAt);
  return Number.isFinite(end) && nowMs <= end;
}

/** 列表用：标记是否仍在进行（含 endsAt 窗口）。 */
export function isAnnouncementLive(a: Announcement, nowMs = Date.now()): boolean {
  return a.active && withinWindow(a, nowMs);
}

/** 列表顺序:进行中在前,各自按日期倒序。 */
export function sortedAnnouncements(nowMs = Date.now()): Announcement[] {
  return [...ANNOUNCEMENTS].sort((a, b) => {
    const aLive = isAnnouncementLive(a, nowMs);
    const bLive = isAnnouncementLive(b, nowMs);
    if (aLive !== bLive) return aLive ? -1 : 1;
    return byDateDesc(a, b);
  });
}

/** 公告条候选:最新一条进行中、非弹窗、未过 endsAt；促销由 CloudPromoBanner 独占顶栏下一条，避免双层同文。 */
export function barAnnouncement(nowMs = Date.now()): Announcement | undefined {
  return ANNOUNCEMENTS.filter(
    (a) =>
      isAnnouncementLive(a, nowMs) &&
      !a.popup &&
      a.id !== CLOUD_PROMO.announcementId,
  ).sort(byDateDesc)[0];
}

/** 弹窗候选:进行中且带 popup 的公告(按日期倒序,客户端再排除已关闭的)。 */
export function popupAnnouncements(nowMs = Date.now()): Announcement[] {
  return ANNOUNCEMENTS.filter((a) => isAnnouncementLive(a, nowMs) && a.popup).sort(byDateDesc);
}
