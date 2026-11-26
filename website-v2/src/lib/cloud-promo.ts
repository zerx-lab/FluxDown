/**
 * 首页 Hero 促销胶囊（展示层）。
 *
 * 是否展示、活动名、当前档位价、本档余量、截止时间全部在运行期取自 `GET /api/cloud/plans`：
 * FluxCloud 只在活动 active（启用 + 时间窗口内 + 有可售档位）时下发 `campaign`，
 * 后台开 / 停 / 改活动后官网无需重新构建。本文件只做挑选与文案。
 *
 * 克制原则：只占 Hero 角标一个位置；无活动时保持新功能角标；
 * 截止时间只在最后几天提示，余量用中性措辞，不做倒计时与闪烁。
 */
import type { CloudPlan } from "@/components/pricing/types";
import { formatMinor, INTL_LOCALE } from "@/components/pay/money";
import type { Lang } from "@/i18n/config";

export interface HeroPromo {
  campaignName: string;
  planName: string;
  saleMinor: number;
  listMinor: number;
  currency: string;
  /** 当前限量档剩余份数；null = 不限量档。 */
  stageLeft: number | null;
  /** 活动截止（ISO）；null = 无截止。 */
  endAt: string | null;
}

/** 截止提示窗口：仅最后 7 天提一句，平时不制造紧迫感。 */
export const PROMO_DEADLINE_HINT_MS = 7 * 24 * 60 * 60 * 1000;

const isAmount = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v) && v >= 0;

/**
 * 目录 → Hero 促销：按目录顺序取第一个「可购买、确有折扣、未过截止、本档未售罄」的活动套餐。
 * 目录形状不符或无活动返回 null（保持默认角标）。
 */
export function pickHeroPromo(plans: unknown, nowMs: number = Date.now()): HeroPromo | null {
  if (!Array.isArray(plans)) return null;
  for (const item of plans) {
    if (!item || typeof item !== "object") continue;
    const plan = item as Partial<CloudPlan>;
    const c = plan.campaign;
    if (!c || typeof c !== "object" || plan.purchasable === false) continue;
    const listMinor = plan.priceMinor;
    const saleMinor = c.effectivePriceMinor;
    if (!isAmount(listMinor) || !isAmount(saleMinor) || saleMinor >= listMinor) continue;

    let endAt: string | null = null;
    if (c.endAt) {
      const end = Date.parse(c.endAt);
      // 目录有 60 秒缓存，这里再按本机时间兜一次截止
      if (!Number.isFinite(end) || end <= nowMs) continue;
      endAt = c.endAt;
    }

    const idx = c.currentStageIndex;
    const quota = Array.isArray(c.stages) ? c.stages[idx]?.quota : undefined;
    const sold = Array.isArray(c.stageSold) ? (c.stageSold[idx] ?? 0) : 0;
    const stageLeft = isAmount(quota) ? Math.max(0, quota - sold) : null;
    // 本档已满：缓存里的价马上会换档，不展示可能过期的价
    if (stageLeft === 0) continue;

    return {
      campaignName: typeof c.name === "string" ? c.name.trim() : "",
      planName: typeof plan.name === "string" ? plan.name.trim() : "",
      saleMinor,
      listMinor,
      currency: (typeof plan.currency === "string" && plan.currency.trim()) || "CNY",
      stageLeft,
      endAt,
    };
  }
  return null;
}

export const heroPromoCopy = {
  en: {
    tag: "Sale",
    /** 目录名称只有中文，英文站用固定标题。 */
    title: "FluxCloud limited offer",
    left: (n: number) => `${n} left at this price`,
    endsOn: (date: string) => `ends ${date}`,
    aria: (title: string, sale: string, list: string) => `${title}: ${sale}, regularly ${list}. View pricing`,
  },
  zh: {
    tag: "限时",
    title: "云端限时优惠",
    left: (n: number) => `本价位余 ${n} 份`,
    endsOn: (date: string) => `${date}截止`,
    aria: (title: string, sale: string, list: string) => `${title}：${sale}，原价 ${list}，查看定价`,
  },
} as const;

export interface HeroPromoView {
  title: string;
  sale: string;
  list: string;
  /** 余量或临近截止的一句补充；无则 null。 */
  meta: string | null;
  aria: string;
}

/** 促销 → 胶囊各段文案。中文站用后台活动名 + 套餐名，英文站用固定标题。 */
export function heroPromoView(p: HeroPromo, lang: Lang, nowMs: number = Date.now()): HeroPromoView {
  const t = heroPromoCopy[lang];
  const title =
    lang === "zh" && p.campaignName ? [p.campaignName, p.planName].filter(Boolean).join(" · ") : t.title;
  const sale = formatMinor(p.saleMinor, p.currency, lang);
  const list = formatMinor(p.listMinor, p.currency, lang);

  let meta: string | null = null;
  if (p.stageLeft != null) {
    meta = t.left(p.stageLeft);
  } else if (p.endAt && Date.parse(p.endAt) - nowMs <= PROMO_DEADLINE_HINT_MS) {
    const date = new Intl.DateTimeFormat(INTL_LOCALE[lang], {
      month: lang === "zh" ? "long" : "short",
      day: "numeric",
      timeZone: "Asia/Shanghai",
    }).format(new Date(p.endAt));
    meta = t.endsOn(date);
  }

  return { title, sale, list, meta, aria: t.aria(title, sale, list) };
}
