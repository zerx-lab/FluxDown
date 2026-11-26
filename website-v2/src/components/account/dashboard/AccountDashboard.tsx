import { useCallback, useEffect, useRef, useState } from "react";
import type { Lang } from "@/i18n/config";
import { account } from "@/i18n/messages/account";
import { accountApi, accountErrorText, accountHref, currentPath, type AccountResult } from "@/lib/account/client";
import type { AccountDevice, AccountMe, AccountNotifyOverview, AccountOrder } from "@/lib/account/types";
import type { CloudPlan } from "@/components/pricing/types";
import { withBase } from "@/lib/base";
import type { DashboardCtx } from "./context";
import DevicesSection from "./DevicesSection";
import IdentityBar from "./IdentityBar";
import MembershipSection from "./MembershipSection";
import NotificationsSection from "./NotificationsSection";
import OverviewSection from "./OverviewSection";
import { useResource, type Failure } from "./resource";
import SectionNav from "./SectionNav";
import SecuritySection from "./SecuritySection";
import { DashboardSkeleton, DisabledState, LoadErrorState } from "./states";
import { HEADING_ID } from "./ui";
import { useSection } from "./useSection";

type Phase =
  | { kind: "loading" }
  | { kind: "ready"; me: AccountMe }
  | { kind: "disabled" }
  | { kind: "error"; code: string };

const fail = (status: number, code: string): AccountResult<never> => ({ ok: false, status, code, retryAfter: null });

/** 公开的套餐目录(与定价页同一接口),不走账户 BFF。 */
async function fetchPlans(): Promise<AccountResult<CloudPlan[]>> {
  try {
    const res = await fetch(withBase("/api/cloud/plans"), { headers: { Accept: "application/json" } });
    if (!res.ok) return fail(res.status, "upstream_error");
    const plans: unknown = await res.json();
    return Array.isArray(plans) ? { ok: true, data: plans as CloudPlan[] } : fail(res.status, "upstream_invalid");
  } catch {
    return fail(0, "network");
  }
}

async function fetchDevices(): Promise<AccountResult<AccountDevice[]>> {
  const res = await accountApi<{ devices: AccountDevice[] }>("devices");
  return res.ok ? { ok: true, data: res.data.devices } : res;
}

const fetchOrders = () => accountApi<AccountOrder[]>("orders");
const fetchNotify = () => accountApi<AccountNotifyOverview>("notifications");

/** 账户中心:挂载即 `GET me`;401 跳登录,403 停用说明,其余失败可重试。 */
export default function AccountDashboard({ lang }: { lang: Lang }) {
  const t = account[lang];
  const [phase, setPhase] = useState<Phase>({ kind: "loading" });
  const [section, go] = useSection();
  const meSeq = useRef(0);

  // 401 `unauthorized` = 未登录 / 会话已失效:整页跳登录并带上当前地址;403 `account_disabled` = 停用说明态。
  const guard = useCallback(
    (failure: Failure) => {
      if (failure.status === 401 && failure.code === "unauthorized") {
        location.replace(accountHref(lang, "login", currentPath()));
        return true;
      }
      if (failure.status === 403 && failure.code === "account_disabled") {
        setPhase({ kind: "disabled" });
        return true;
      }
      return false;
    },
    [lang],
  );

  const loadMe = useCallback(async () => {
    const id = ++meSeq.current;
    setPhase({ kind: "loading" });
    const res = await accountApi<AccountMe>("me");
    if (id !== meSeq.current) return;
    if (res.ok) setPhase({ kind: "ready", me: res.data });
    else if (!guard(res)) setPhase({ kind: "error", code: res.code });
  }, [guard]);

  useEffect(() => {
    void loadMe();
    return () => {
      meSeq.current += 1; // 卸载后丢弃在途响应
    };
  }, [loadMe]);

  const refreshMe = useCallback(async () => {
    const res = await accountApi<AccountMe>("me");
    if (res.ok) setPhase((p) => (p.kind === "ready" ? { kind: "ready", me: res.data } : p));
    else guard(res);
  }, [guard]);

  const setMe = useCallback((me: AccountMe) => setPhase({ kind: "ready", me }), []);

  const devices = useResource(fetchDevices, guard);
  const notify = useResource(fetchNotify, guard);
  const orders = useResource(fetchOrders, guard);
  const plans = useResource(fetchPlans, guard);

  // 切换分区后把焦点移到分区标题,读屏与键盘用户不丢位置(首次渲染不抢焦点)。
  const shown = useRef(section);
  useEffect(() => {
    if (shown.current === section) return;
    shown.current = section;
    document.getElementById(HEADING_ID)?.focus({ preventScroll: true });
  }, [section]);

  if (phase.kind === "loading") return <DashboardSkeleton label={t.states.loading} />;
  if (phase.kind === "disabled") return <DisabledState lang={lang} />;
  if (phase.kind === "error") {
    return <LoadErrorState lang={lang} message={accountErrorText(lang, phase.code)} onRetry={() => void loadMe()} />;
  }

  const ctx: DashboardCtx = { lang, t, me: phase.me, setMe, refreshMe, guard, go, devices, notify, orders, plans };

  return (
    <div>
      <IdentityBar ctx={ctx} />
      <div className="mt-8 grid gap-8 lg:grid-cols-[220px_minmax(0,1fr)] lg:gap-12">
        <SectionNav section={section} labels={t.nav} go={go} />
        <div className="min-w-0">
          {section === "overview" && <OverviewSection ctx={ctx} />}
          {section === "membership" && <MembershipSection ctx={ctx} />}
          {section === "devices" && <DevicesSection ctx={ctx} />}
          {section === "notifications" && <NotificationsSection ctx={ctx} />}
          {section === "security" && <SecuritySection ctx={ctx} />}
        </div>
      </div>
    </div>
  );
}
