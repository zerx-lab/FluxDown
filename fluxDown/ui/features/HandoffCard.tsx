/** 「交给桌面端解析」卡片：按 HandoffState 显示状态；仅 match 时给出按钮。 */
import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { browser } from "wxt/browser";
import type { HandoffState } from "@/background/protocol";
import { sendAction } from "@/utils/messaging";
import { Button } from "@/ui/components/Button";
import { Icon } from "@/ui/components/Icon";
import { showToast } from "@/ui/components/Toast";
import { useT } from "@/ui/i18n";
import { usePlatform } from "@/ui/hooks/usePlatform";
import "./features.css";

const RESOLVE_POLL_MS = 1000;
/** 桌面端解析后还要等用户选清晰度（选择框 60 s 倒计时），超过此上限不再显示解析中。 */
const RESOLVE_TRACK_MAX_MS = 120_000;
/** 引擎任务状态：等待中 / 准备中（插件解析在途即为准备中）仍算解析中；失败单独提示。 */
const STATUS_PENDING = 0;
const STATUS_ERROR = 4;
const STATUS_PREPARING = 5;

type TrackedTask = { status: number; errorMessage: string } | null;

/** 从 background 的 `nmh-tasks` 应答里取出指定任务；连接失败或列表里没有时为 null。 */
async function fetchTask(taskId: string): Promise<TrackedTask> {
  let res: unknown;
  try {
    res = await browser.runtime.sendMessage({ type: "nmh-tasks" });
  } catch {
    return null;
  }
  if (typeof res !== "object" || res === null || !("tasks" in res) || !Array.isArray(res.tasks)) return null;
  for (const task of res.tasks) {
    if (typeof task !== "object" || task === null || !("taskId" in task) || task.taskId !== taskId) continue;
    const status = "status" in task && typeof task.status === "number" ? task.status : -1;
    const errorMessage = "errorMessage" in task && typeof task.errorMessage === "string" ? task.errorMessage : "";
    return { status, errorMessage };
  }
  return null;
}

export function HandoffCard(props: { handoff: HandoffState; tabId: number }): JSX.Element | null {
  const t = useT();
  const { desktopApp } = usePlatform();
  const [busy, setBusy] = useState(false);
  /** 正在桌面端解析的任务 ID；为空表示没有在途解析。 */
  const [resolving, setResolving] = useState<string | null>(null);
  const h = props.handoff;

  useEffect(() => {
    if (!resolving) return;
    const startedAt = Date.now();
    let stopped = false;
    let inFlight = false;
    const tick = async () => {
      if (inFlight || stopped) return;
      inFlight = true;
      try {
        const task = await fetchTask(resolving);
        if (stopped) return;
        const stillResolving = task !== null && (task.status === STATUS_PENDING || task.status === STATUS_PREPARING);
        if (stillResolving && Date.now() - startedAt < RESOLVE_TRACK_MAX_MS) return;
        if (task?.status === STATUS_ERROR) {
          showToast(t("handoff.resolveFailed", { message: task.errorMessage || t("list.sendFailed") }), "error");
        }
        setResolving(null);
      } finally {
        inFlight = false;
      }
    };
    const timer = setInterval(() => void tick(), RESOLVE_POLL_MS);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  }, [resolving]);

  const send = async () => {
    setBusy(true);
    try {
      const res = await sendAction<"handoffPage">({ action: "handoffPage", tabId: props.tabId });
      if (res?.success) {
        if (res.taskId) setResolving(res.taskId);
        else showToast(t(res.channel === "remote" ? "notify.downloadSent" : "handoff.sent"), "success");
      } else showToast(res?.message || t("list.sendFailed"), "error");
    } catch {
      showToast(t("list.sendFailed"), "error");
    } finally {
      setBusy(false);
    }
  };

  let text: string;
  switch (h.kind) {
    case "choose-target":
      text = t("remote.manualHint");
      break;
    case "unavailable":
      text = t(`handoff.unavailable.${h.reason}`);
      break;
    case "no-match":
      text = t("handoff.noMatch", { count: String(h.installed) });
      break;
    case "disabled":
      text = t("handoff.disabled", { name: h.name });
      break;
    case "match":
      text = resolving ? t("handoff.resolving") : t("handoff.match", { name: h.name });
      break;
  }

  // 交给桌面解析依赖 NMH；Android 无桌面 App，也不探测完成前先隐藏
  if (!desktopApp && h.kind !== "choose-target") return null;

  return (
    <div class={`fd-handoff fd-handoff-${h.kind}`}>
      <Icon name="external" size={16} />
      <div class="fd-handoff-text">
        <div class="fd-handoff-title">{t(h.kind === "choose-target" ? "contextMenu.sendPageToFluxDown" : "handoff.title")}</div>
        <div class="fd-handoff-desc">{text}</div>
        {resolving ? (
          <div class="fd-progress fd-handoff-progress">
            <div class="fd-progress-fill fd-indeterminate" style={{ width: "100%" }} />
          </div>
        ) : null}
      </div>
      {h.kind === "match" || h.kind === "choose-target" ? (
        <Button size="sm" variant="primary" disabled={busy || resolving !== null} onClick={() => void send()}>
          {resolving ? t("handoff.resolvingButton") : t(h.kind === "choose-target" ? "remote.manualChoose" : "handoff.button")}
        </Button>
      ) : null}
    </div>
  );
}
