import { INTL_LOCALE, type Lang } from "@/i18n/config";

/** Minor units (cents / fen) → localized currency string; whole amounts drop decimals. */
export function formatMinor(minor: number, currency: string, lang: Lang): string {
  try {
    return new Intl.NumberFormat(INTL_LOCALE[lang], {
      style: "currency",
      currency,
      minimumFractionDigits: minor % 100 === 0 ? 0 : 2,
    }).format(minor / 100);
  } catch {
    return `${(minor / 100).toFixed(2)} ${currency}`;
  }
}

/** ISO date/time → localized short date; empty string for unparsable input. */
export function formatDate(iso: string, lang: Lang): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleDateString(INTL_LOCALE[lang]);
}

/** USDT micros (1e-6) → "5.58 USDT". */
export function formatUsdt(micros: number): string {
  return `${(micros / 1e6).toFixed(2)} USDT`;
}

/**
 * Mirrors FluxCloud `usdt_micros_for`: ceil(payable * usdt / base), rounded up to a 0.01 USDT
 * (10_000 micros) multiple, minimum 10_000.
 */
export function estimateUsdtMicros(payableMinor: number, baseMinor: number, baseUsdtMicros: number): number {
  const step = 10_000n;
  const base = BigInt(baseMinor);
  const num = BigInt(Math.max(0, payableMinor)) * BigInt(baseUsdtMicros);
  const raw = (num + base - 1n) / base;
  const rounded = ((raw + step - 1n) / step) * step;
  return Number(rounded < step ? step : rounded);
}
