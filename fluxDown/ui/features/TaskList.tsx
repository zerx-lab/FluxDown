/**
 * 任务列表（迁移自旧 popup 任务面板）：下载中 / 最近完成，1 s 轮询，可暂停 / 继续 / 删除 / 打开。
 * 轮询只在页面可见时进行；popup 用 compact（下载中 + 最近 3 个完成），侧边栏用 full。
 */
import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { browser } from "wxt/browser";
import { sendAction } from "@/utils/messaging";
import { Button } from "@/ui/components/Button";
import { EmptyState } from "@/ui/components/EmptyState";
import { Icon } from "@/ui/components/Icon";
import { IconButton } from "@/ui/components/IconButton";
import { showToast } from "@/ui/components/Toast";
import { QuickDownload } from "./QuickDownload";
import { formatBytes, formatSpeed, progressPercent } from "@/ui/format";
import { useT } from "@/ui/i18n";
import "./features.css";
import { isCompletedFileMissing } from "./task-state";

/** popup ↔ background 的任务对象（与 background/tasks.ts 一致）。 */
interface TaskBrief {
  taskId: string;
  fileName: string;
  status: number;
  downloadedBytes: number;
  totalBytes: number;
  speed: number;
  errorMessage?: string;
  createdAt: string;
  fileMissing?: boolean;
}

interface NmhTasksResponse {
  ok: boolean;
  connected: boolean;
  tasks: TaskBrief[];
}

interface NmhOpResponse {
  ok: boolean;
  message?: string;
}

const STATUS = { PENDING: 0, DOWNLOADING: 1, PAUSED: 2, COMPLETED: 3, ERROR: 4, PREPARING: 5 } as const;
const POLL_MS = 1000;

async function nmhTasks(): Promise<NmhTasksResponse> {
  try {
    const res = (await browser.runtime.sendMessage({ type: "nmh-tasks" })) as NmhTasksResponse | undefined;
    return res ?? { ok: false, connected: false, tasks: [] };
  } catch {
    return { ok: false, connected: false, tasks: [] };
  }
}

async function nmhMessage(msg: Record<string, string>): Promise<NmhOpResponse> {
  try {
    return ((await browser.runtime.sendMessage(msg)) as NmhOpResponse | undefined) ?? { ok: false };
  } catch (e) {
    return { ok: false, message: String(e) };
  }
}

interface Props {
  density: "compact" | "full";
  /** 快速下载输入框（连接时显示）。 */
  quickDownload?: boolean;
  /** 未完成任务数变化（未连接视为 0）；popup 用于页签计数。 */
  onActiveCountChange?: (count: number) => void;
}

export function TaskList(props: Props): JSX.Element {
  const t = useT();
  const [state, setState] = useState<NmhTasksResponse | null>(null);
  const [starting, setStarting] = useState(false);
  const inFlight = useRef(false);
  const startTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  const poll = async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    try {
      const res = await nmhTasks();
      setState(res);
      if (res.connected) {
        setStarting(false);
        if (startTimer.current) clearTimeout(startTimer.current);
      }
    } finally {
      inFlight.current = false;
    }
  };

  useEffect(() => {
    let timer: ReturnType<typeof setInterval> | undefined;
    const start = () => {
      if (timer !== undefined) return;
      void poll();
      timer = setInterval(() => void poll(), POLL_MS);
    };
    const stop = () => {
      if (timer !== undefined) clearInterval(timer);
      timer = undefined;
    };
    const onVis = () => (document.hidden ? stop() : start());
    document.addEventListener("visibilitychange", onVis);
    if (!document.hidden) start();
    return () => {
      document.removeEventListener("visibilitychange", onVis);
      stop();
      if (startTimer.current) clearTimeout(startTimer.current);
    };
  }, []);

  const onActiveCountChange = props.onActiveCountChange;
  const activeCount = state?.connected ? state.tasks.filter((x) => x.status !== STATUS.COMPLETED).length : 0;
  useEffect(() => {
    onActiveCountChange?.(activeCount);
  }, [activeCount, onActiveCountChange]);

  const op = async (kind: "pause" | "resume" | "remove", taskId: string) => {
    const res = await nmhMessage({ type: "nmh-task-op", op: kind, taskId });
    if (!res.ok) showToast(res.message || t("popup.task.opFailed"), "error");
    else void poll();
  };

  const fileOp = async (kind: "open" | "reveal", taskId: string) => {
    const res = await nmhMessage({ type: kind === "open" ? "nmh-open-file" : "nmh-reveal-file", taskId });
    if (!res.ok) showToast(res.message || t("popup.task.opFailed"), "error");
  };

  const startApp = () => {
    if (starting) return;
    setStarting(true);
    void nmhMessage({ type: "nmh-warmup" });
    // 8 s 内仍未连上则恢复按钮，允许重试
    startTimer.current = setTimeout(() => setStarting(false), 8000);
  };

  if (!state) return <div class="fd-empty">{t("header.checking")}</div>;

  const quick = props.quickDownload && state.connected ? <QuickDownload onSent={() => void poll()} /> : null;

  if (!state.connected) {
    return (
      <EmptyState icon="alert" title={t("popup.tasks.appNotRunning")}>
        <Button size="sm" variant="primary" disabled={starting} onClick={startApp}>
          {starting ? t("popup.tasks.starting") : t("popup.tasks.startApp")}
        </Button>
      </EmptyState>
    );
  }

  const active = state.tasks.filter((x) => x.status !== STATUS.COMPLETED);
  const completed = state.tasks.filter((x) => x.status === STATUS.COMPLETED).slice(0, props.density === "compact" ? 3 : 20);

  return (
    <div class="fd-tasks">
      {quick}
      {active.length === 0 && completed.length === 0 ? <EmptyState icon="download" title={t("popup.tasks.empty")} /> : null}
      {active.length > 0 ? (
        <section class="fd-group">
          <h4 class="fd-group-title">{t("popup.tasks.downloading")}</h4>
          {active.map((task) => {
            const pct = progressPercent(task.downloadedBytes, task.totalBytes);
            const paused = task.status === STATUS.PAUSED;
            const error = task.status === STATUS.ERROR;
            const preparing = task.status === STATUS.PENDING || task.status === STATUS.PREPARING;
            let right: string;
            if (error) right = "";
            else if (paused) right = t("popup.task.paused");
            else if (task.status === STATUS.PENDING) right = t("popup.task.pending");
            else if (task.status === STATUS.PREPARING) right = t("popup.task.preparing");
            else right = `${formatSpeed(task.speed)} · ${pct.toFixed(0)}%`;
            return (
              <div class={`fd-task${paused ? " fd-paused" : ""}${error ? " fd-error" : ""}`} key={task.taskId}>
                <Icon fileName={task.fileName} size={18} />
                <div class="fd-task-main">
                  <div class="fd-row-title" title={task.fileName}>
                    {task.fileName}
                  </div>
                  <div class="fd-progress">
                    <div class={`fd-progress-fill${preparing ? " fd-indeterminate" : ""}`} style={{ width: preparing ? "100%" : `${pct}%` }} />
                  </div>
                  <div class="fd-task-meta">
                    <span>
                      {formatBytes(task.downloadedBytes)} / {task.totalBytes > 0 ? formatBytes(task.totalBytes) : "--"}
                    </span>
                    <span>{right}</span>
                  </div>
                  {error ? (
                    <div class="fd-task-error" title={task.errorMessage}>
                      {task.errorMessage || t("popup.task.errorGeneric")}
                    </div>
                  ) : null}
                </div>
                <IconButton
                  icon={paused ? "play" : "pause"}
                  label={paused ? t("popup.task.resume") : t("popup.task.pause")}
                  size={13}
                  disabled={error}
                  onClick={() => void op(paused ? "resume" : "pause", task.taskId)}
                />
                <IconButton icon="trash" label={t("popup.task.remove")} size={13} onClick={() => void op("remove", task.taskId)} />
              </div>
            );
          })}
        </section>
      ) : null}
      {completed.length > 0 ? (
        <section class="fd-group">
          <h4 class="fd-group-title">{t("popup.tasks.completed")}</h4>
          {completed.map((task) => {
            const missing = isCompletedFileMissing(task);
            return (
              <div class="fd-task" key={task.taskId}>
                <Icon name={missing ? "alert" : "checkCircle"} size={18} class={missing ? "" : "fd-ok"} />
                <div class="fd-task-main">
                  <div class="fd-row-title" title={task.fileName}>
                    {task.fileName}
                  </div>
                  <div class="fd-task-meta">
                    <span>{formatBytes(task.totalBytes > 0 ? task.totalBytes : task.downloadedBytes)}</span>
                  </div>
                  {missing ? <div class="fd-task-error">{t("popup.task.fileMissing")}</div> : null}
                </div>
                <Button size="sm" variant="ghost" disabled={missing} onClick={() => void fileOp("open", task.taskId)}>
                  {t("popup.task.open")}
                </Button>
                <Button size="sm" variant="ghost" onClick={() => void fileOp("reveal", task.taskId)}>
                  {t("popup.task.reveal")}
                </Button>
              </div>
            );
          })}
        </section>
      ) : null}
    </div>
  );
}
