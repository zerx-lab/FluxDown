import { useCallback, useEffect, useRef, useState } from "react";
import type { Lang } from "@/i18n/config";
import { accountHref, type AccountPage } from "@/lib/account/client";

/** 验证码位数(与 FluxCloud 一致)。 */
export const CODE_LENGTH = 6;
/** 同一邮箱发码冷却(秒)。 */
export const RESEND_COOLDOWN = 60;
/** 密码最短长度。 */
export const PASSWORD_MIN = 8;
/** 昵称最长字符数(trim 后 1–32)。 */
export const NICKNAME_MAX = 32;

/** 只保留数字并截到验证码位数;粘贴 `123 456`、全角空白都能得到 `123456`。 */
export function digitsOnly(value: string): string {
  return value.replace(/\D/g, "").slice(0, CODE_LENGTH);
}

/** 邮箱比较口径:忽略首尾空白与大小写。 */
export function normalizeEmail(value: string): string {
  return value.trim().toLowerCase();
}

/** 验证码有效期(秒)-> 展示用的整分钟数,至少 1。 */
export function ttlMinutes(ttlSeconds: number): number {
  return Math.max(1, Math.round(ttlSeconds / 60));
}

/** 带 `?next=` 与可选 `?email=` 的账户页面地址;`next` 在三页之间互相跳转时保留。 */
export function authHref(lang: Lang, page: AccountPage, next: string | null, email?: string | null): string {
  const base = accountHref(lang, page, next);
  const mail = email?.trim();
  if (!mail) return base;
  return `${base}${base.includes("?") ? "&" : "?"}email=${encodeURIComponent(mail)}`;
}

/** 失败结果里调用方用得到的字段(`AccountResult` 的失败分支)。 */
export interface Failure {
  code: string;
  retryAfter: number | null;
}

/** 登录后要回去的原始 `next` 与预填邮箱:只在挂载后读取地址,避免 SSR 与水合不一致。 */
export function useQueryParams(): { next: string | null; email: string } {
  const [params, setParams] = useState<{ next: string | null; email: string }>({ next: null, email: "" });
  useEffect(() => {
    const query = new URLSearchParams(location.search);
    setParams({ next: query.get("next"), email: (query.get("email") ?? "").slice(0, 254) });
  }, []);
  return params;
}

/**
 * 倒计时(秒)。`start(n)` 重置为 n;按墙钟推算剩余,后台标签页被节流后恢复时不会走慢。
 * 每次回调都严格递减,避免定时器提前触发导致停在原值。
 */
export function useCooldown(): { left: number; start: (seconds: number) => void } {
  const [left, setLeft] = useState(0);
  const until = useRef(0);

  useEffect(() => {
    if (left <= 0) return;
    const timer = window.setTimeout(() => {
      const remaining = Math.max(0, Math.ceil((until.current - Date.now()) / 1000));
      setLeft((prev) => Math.min(prev - 1, remaining));
    }, 1000);
    return () => window.clearTimeout(timer);
  }, [left]);

  const start = useCallback((seconds: number) => {
    const s = Math.max(0, Math.ceil(seconds));
    until.current = Date.now() + s * 1000;
    setLeft(s);
  }, []);

  return { left, start };
}

/**
 * 步骤切换后把焦点移到新步骤里第一个 `[data-step-focus]` 元素(输入框或主按钮)。
 * 首次渲染不抢焦点;隐藏面板里的候选元素会被跳过。
 */
export function useStepFocus<T extends HTMLElement>(step: string) {
  const ref = useRef<T>(null);
  const previous = useRef(step);
  useEffect(() => {
    if (previous.current === step) return;
    previous.current = step;
    const target = Array.from(ref.current?.querySelectorAll<HTMLElement>("[data-step-focus]") ?? []).find(
      (el) => el.getClientRects().length > 0,
    );
    target?.focus();
  }, [step]);
  return ref;
}
