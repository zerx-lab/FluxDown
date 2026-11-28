import { useCallback, useState } from "react";
import type { Lang } from "@/i18n/config";
import { accountApi, accountErrorText, accountHref } from "@/lib/account/client";

/** 退出登录:成功(或会话本来就已失效)后整页跳到登录页;失败给出可读错误并允许重试。 */
export function useSignOut(lang: Lang) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const run = useCallback(async () => {
    setBusy(true);
    setError(null);
    const res = await accountApi("logout", { method: "POST" });
    if (res.ok || res.status === 401) {
      location.assign(accountHref(lang, "login"));
      return; // 导航期间保持 busy,避免重复点击
    }
    setError(accountErrorText(lang, res.code));
    setBusy(false);
  }, [lang]);

  return { busy, error, run };
}
