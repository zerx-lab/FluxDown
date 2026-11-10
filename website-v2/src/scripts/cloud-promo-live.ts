/**
 * 把页面上 data-cloud-promo 节点的价签改成 /api/cloud/plans 里专业版当前价。
 * SSG 不写死数字；拉失败则保持占位（不展示假价）。
 */
import type { Lang } from "@/i18n/config";
import { withBase } from "@/lib/base";
import {
  cloudPromoCopy,
  formatPromoYuan,
  isCloudPromoActive,
  promoPricesFromPlan,
  type PromoPrices,
} from "@/lib/cloud-promo";

function pageLang(): Lang {
  const lang = document.documentElement.lang?.toLowerCase() ?? "";
  return lang.startsWith("zh") ? "zh" : "en";
}

function applyPrices(prices: PromoPrices, lang: Lang): void {
  const sale = formatPromoYuan(prices.saleMinor, prices.currency, lang);
  const list = prices.hasDiscount
    ? formatPromoYuan(prices.listMinor, prices.currency, lang)
    : null;
  const t = cloudPromoCopy[lang];

  for (const root of document.querySelectorAll<HTMLElement>("[data-cloud-promo]")) {
    const saleEl = root.querySelector<HTMLElement>("[data-promo-sale]");
    if (saleEl) saleEl.textContent = sale;

    const listEl = root.querySelector<HTMLElement>("[data-promo-list]");
    if (listEl) {
      if (list) {
        listEl.textContent = list;
        listEl.hidden = false;
      } else {
        listEl.textContent = "";
        listEl.hidden = true;
      }
    }

    const fill = root.getAttribute("data-promo-fill");
    const textEl = root.querySelector("[data-promo-text]");
    if (textEl && fill === "heroBadge") textEl.textContent = t.heroBadge(sale, list);
    if (textEl && fill === "finalCta") textEl.textContent = t.finalCta(sale);

    root.setAttribute("aria-label", t.line(sale, list));
    root.dataset.promoReady = "1";
  }
}

export async function hydrateCloudPromoPrices(): Promise<void> {
  if (!isCloudPromoActive()) return;
  if (!document.querySelector("[data-cloud-promo]")) return;

  let plans: unknown;
  try {
    const res = await fetch(withBase("/api/cloud/plans"), {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    if (!res.ok) return;
    plans = await res.json();
  } catch {
    return;
  }
  if (!Array.isArray(plans)) return;

  let prices: PromoPrices | null = null;
  for (const item of plans) {
    if (!item || typeof item !== "object") continue;
    prices = promoPricesFromPlan(item as Parameters<typeof promoPricesFromPlan>[0]);
    if (prices) break;
  }
  if (!prices) return;
  applyPrices(prices, pageLang());
}

/** 到期隐藏 + 拉价，首页 / 定价页挂一次即可。 */
export function bootCloudPromo(): void {
  const now = Date.now();
  for (const node of document.querySelectorAll("[data-cloud-promo][data-ends-at]")) {
    const end = Date.parse(node.getAttribute("data-ends-at") || "");
    if (Number.isFinite(end) && now > end) node.remove();
  }
  void hydrateCloudPromoPrices();
}
