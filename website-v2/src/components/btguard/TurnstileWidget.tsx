/**
 * Cloudflare Turnstile 小部件（显式渲染）：下载与举报表单共用。
 * 父组件经 `ref.reset()` 在每次请求后换新 token（token 一次性）；脚本加载失败或小部件报错走 `onError`。
 */
import { useEffect, useImperativeHandle, useRef, type Ref } from "react";

interface TurnstileApi {
  render(
    container: HTMLElement,
    options: {
      sitekey: string;
      theme: "auto";
      language: string;
      callback: (token: string) => void;
      "expired-callback": () => void;
      "error-callback": () => void;
    },
  ): string;
  reset(widgetId: string): void;
  remove(widgetId: string): void;
}

declare global {
  interface Window {
    turnstile?: TurnstileApi;
  }
}

const TURNSTILE_SRC = "https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit";
let turnstileLoading: Promise<TurnstileApi> | null = null;

async function loadTurnstile(): Promise<TurnstileApi> {
  if (window.turnstile) return window.turnstile;
  if (!turnstileLoading) {
    const { promise, resolve, reject } = Promise.withResolvers<TurnstileApi>();
    const script = document.createElement("script");
    script.src = TURNSTILE_SRC;
    script.async = true;
    script.onload = () => (window.turnstile ? resolve(window.turnstile) : reject(new Error("turnstile unavailable")));
    script.onerror = () => reject(new Error("turnstile script failed to load"));
    document.head.appendChild(script);
    turnstileLoading = promise;
  }
  try {
    return await turnstileLoading;
  } catch (error) {
    turnstileLoading = null;
    throw error;
  }
}

export interface TurnstileHandle {
  reset(): void;
}

export default function TurnstileWidget({
  siteKey,
  lang,
  onToken,
  onError,
  ref,
}: {
  siteKey: string;
  lang: "en" | "zh";
  /** token 就绪时给值，过期 / 出错 / 重置后给 null。 */
  onToken: (token: string | null) => void;
  onError: () => void;
  ref?: Ref<TurnstileHandle>;
}) {
  const box = useRef<HTMLDivElement>(null);
  const widgetId = useRef<string | null>(null);
  const callbacks = useRef({ onToken, onError });
  callbacks.current = { onToken, onError };

  useImperativeHandle(ref, () => ({
    reset() {
      callbacks.current.onToken(null);
      if (widgetId.current !== null) window.turnstile?.reset(widgetId.current);
    },
  }));

  useEffect(() => {
    const container = box.current;
    if (!container) return;
    let cancelled = false;
    loadTurnstile().then(
      (api) => {
        if (cancelled) return;
        widgetId.current = api.render(container, {
          sitekey: siteKey,
          theme: "auto",
          language: lang === "zh" ? "zh-cn" : "en",
          callback: (value) => callbacks.current.onToken(value),
          "expired-callback": () => callbacks.current.onToken(null),
          "error-callback": () => {
            callbacks.current.onToken(null);
            callbacks.current.onError();
          },
        });
      },
      () => {
        if (!cancelled) callbacks.current.onError();
      },
    );
    return () => {
      cancelled = true;
      if (widgetId.current !== null) {
        window.turnstile?.remove(widgetId.current);
        widgetId.current = null;
      }
    };
  }, [siteKey, lang]);

  return <div ref={box} />;
}
