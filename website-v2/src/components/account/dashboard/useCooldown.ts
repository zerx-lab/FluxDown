import { useCallback, useEffect, useState } from "react";

/** 发码冷却倒计时(秒):`start(n)` 开始,`left` 为剩余整秒,归零后可再次发送。 */
export function useCooldown() {
  const [until, setUntil] = useState(0);
  const [now, setNow] = useState(0);

  useEffect(() => {
    if (until <= Date.now()) return;
    const tick = () => {
      const current = Date.now();
      setNow(current);
      if (current >= until) window.clearInterval(timer);
    };
    const timer = window.setInterval(tick, 500);
    tick();
    return () => window.clearInterval(timer);
  }, [until]);

  const start = useCallback((seconds: number) => {
    const current = Date.now();
    setNow(current);
    setUntil(current + seconds * 1000);
  }, []);

  return { left: Math.max(0, Math.ceil((until - now) / 1000)), start };
}
