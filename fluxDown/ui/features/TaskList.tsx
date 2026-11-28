/**
 * 任务列表（迁移自旧 popup 任务面板）：下载中 / 最近完成，1 s 轮询，可暂停 / 继续 / 删除 / 打开。
 * 轮询只在页面可见时进行；popup 用 compact（下载中 + 最近 3 个完成），侧边栏用 full。
 * 「清理」只把当前已完成记录记入扩展本地隐藏名单（storage.local），不删除主程序里的任务。
 */
import type { JSX } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { browser } from "wxt/browser";
import type { TaskBrief } from "@/utils/native-messaging";
import { Button } from "@/ui/components/Button";
import { EmptyState } from "@/ui/components/EmptyState";
import { Icon } from "@/ui/components/Icon";
import { IconButton } from "@/ui/components/IconButton";
import { Select } from "@/ui/components/Select";
import { showToast } from "@/ui/components/Toast";
import { QuickDownload } from "./QuickDownload";
import { formatBytes, formatSpeed, progressPercent } from "@/ui/format";
import { useT } from "@/ui/i18n";
import "./features.css";
import { hideClearedCompleted, isCompletedFileMissing, nextClearedCompletedIds } from "./task-state";

type TaskSource = "local" | "remote";

interface TaskPanelResponse {
  connected: boolean;
  tasks: (Omit<TaskBrief, "speed"> & { speed?: number })[];
  remoteUrl?: string;
  switchable: boolean;
  managementUnavailable?: boolean;
}

interface NmhOpResponse {
  ok: boolean;
  message?: string;
}

const STATUS = { PENDING: 0, DOWNLOADING: 1, PAUSED: 2, COMPLETED: 3, ERROR: 4, PREPARING: 5 } as const;
const POLL_MS = 1000;
/** 扩展本地「已清理的已完成任务 id」名单；popup 与侧边栏共用，经 storage.onChanged 同步。 */
const CLEARED_KEY = "taskListClearedCompleted";

function toIdList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((x): x is string => typeof x === "string") : [];
}

async function taskPanelList(source: TaskSource): Promise<TaskPanelResponse> {
  try {
    const res = (await browser.runtime.sendMessage({ type: "task-panel-list", source })) as TaskPanelResponse | undefined;
    return res ?? { connected: false, tasks: [], switchable: false };
  } catch {
    return { connected: false, tasks: [], switchable: false };
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
  const [state, setState] = useState<TaskPanelResponse | null>(null);
  const [source, setSource] = useState<TaskSource>("local");
  const selectedSource = useRef<TaskSource>("local");
  const [starting, setStarting] = useState(false);
  const inFlight = useRef(false);
  const startTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const [cleared, setCleared] = useState<string[]>([]);

  useEffect(() => {
    let alive = true;
    browser.storage.local
      .get(CLEARED_KEY)
      .then((r) => alive && setCleared(toIdList(r[CLEARED_KEY])))
      .catch(() => {});
    const onChanged = (changes: Record<string, { newValue?: unknown }>, area: string) => {
      if (area === "local" && CLEARED_KEY in changes) setCleared(toIdList(changes[CLEARED_KEY].newValue));
    };
    browser.storage.onChanged.addListener(onChanged);
    return () => {
      alive = false;
      browser.storage.onChanged.removeListener(onChanged);
    };
  }, []);

  const poll = async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    const requestedSource = selectedSource.current;
    try {
      const res = await taskPanelList(requestedSource);
      if (requestedSource !== selectedSource.current) return;
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

  const remoteUrl = state?.remoteUrl;
  const op = async (kind: "pause" | "resume" | "remove", taskId: string) => {
    if (remoteUrl && kind !== "remove") return;
    const res = await nmhMessage(remoteUrl
      ? { type: "task-panel-remove-remote", taskId, remoteUrl }
      : { type: "nmh-task-op", op: kind, taskId });
    if (!res.ok) showToast(res.message || t("popup.task.opFailed"), "error");
    else void poll();
  };

  const fileOp = async (kind: "open" | "reveal", taskId: string) => {
    const res = await nmhMessage({ type: kind === "open" ? "nmh-open-file" : "nmh-reveal-file", taskId });
    if (!res.ok) showToast(res.message || t("popup.task.opFailed"), "error");
  };

  const clearCompleted = async () => {
    if (!state) return;
    const next = nextClearedCompletedIds(state.tasks, cleared);
    setCleared(next);
    try {
      await browser.storage.local.set({ [CLEARED_KEY]: next });
    } catch (e) {
      showToast(String(e), "error");
    }
  };

  const startApp = () => {
    if (starting) return;
    setStarting(true);
    void nmhMessage({ type: "nmh-warmup" });
    // 8 s 内仍未连上则恢复按钮，允许重试
    startTimer.current = setTimeout(() => setStarting(false), 8000);
  };

  if (!state) return <div class="fd-empty">{t("header.checking")}</div>;

  const sourcePicker = state.switchable ? (
    <Select<TaskSource>
      value={source}
      label={t("remote.title")}
      options={[
        { value: "local", label: t("popup.tasks.localSource") },
        { value: "remote", label: t("popup.tasks.remoteSource") },
      ]}
      onChange={(value) => {
        selectedSource.current = value;
        setSource(value);
        void poll();
      }}
    />
  ) : null;

  if (state.switchable && (source === "remote") !== Boolean(remoteUrl)) {
    return <div class="fd-tasks">{sourcePicker}<div class="fd-empty">{t("header.checking")}</div></div>;
  }

  const quick = props.quickDownload && state.connected
    ? <QuickDownload channel={remoteUrl ? "remote" : undefined} onSent={() => void poll()} />
    : null;

  if (!state.connected) {
    return (
      <div class="fd-tasks">
        {sourcePicker}
        {remoteUrl ? (
          <EmptyState
            icon="alert"
            title={t("popup.tasks.remoteUnavailable")}
            hint={state.managementUnavailable ? t("remote.managementUnavailableHint") : undefined}
          />
        ) : (
          <EmptyState icon="alert" title={t("popup.tasks.appNotRunning")}>
            <Button size="sm" variant="primary" disabled={starting} onClick={startApp}>
              {starting ? t("popup.tasks.starting") : t("popup.tasks.startApp")}
            </Button>
            <a class="fd-task-download-link" href="https://fluxdown.com/download/" target="_blank" rel="noreferrer noopener">
              {t("popup.tasks.downloadApp")}
            </a>
          </EmptyState>
        )}
      </div>
    );
  }

  const active = state.tasks.filter((x) => x.status !== STATUS.COMPLETED);
  const completed = hideClearedCompleted(state.tasks, cleared)
    .filter((x) => x.status === STATUS.COMPLETED)
    .slice(0, props.density === "compact" ? 3 : 20);

  return (
    <div class="fd-tasks">
      {sourcePicker}
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
            else right = `${task.speed === undefined ? "" : `${formatSpeed(task.speed)} · `}${pct.toFixed(0)}%`;
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
                {!remoteUrl && (
                  <IconButton
                    icon={paused ? "play" : "pause"}
                    label={paused ? t("popup.task.resume") : t("popup.task.pause")}
                    size={13}
                    disabled={error}
                    onClick={() => void op(paused ? "resume" : "pause", task.taskId)}
                  />
                )}
                <IconButton icon="trash" label={t("popup.task.remove")} size={13} onClick={() => void op("remove", task.taskId)} />
              </div>
            );
          })}
        </section>
      ) : null}
      {completed.length > 0 ? (
        <section class="fd-group">
          <div class="fd-group-head">
            <h4 class="fd-group-title">{t(remoteUrl ? "popup.tasks.remoteCompleted" : "popup.tasks.completed")}</h4>
            <Button size="sm" variant="ghost" title={t("popup.tasks.clearCompletedHint")} onClick={() => void clearCompleted()}>
              {t("popup.tasks.clearCompleted")}
            </Button>
          </div>
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
                {remoteUrl ? (
                  <IconButton icon="trash" label={t("popup.task.remove")} size={13} onClick={() => void op("remove", task.taskId)} />
                ) : <>
                  <Button size="sm" variant="ghost" disabled={missing} onClick={() => void fileOp("open", task.taskId)}>
                    {t("popup.task.open")}
                  </Button>
                  <Button size="sm" variant="ghost" onClick={() => void fileOp("reveal", task.taskId)}>
                    {t("popup.task.reveal")}
                  </Button>
                </>}
              </div>
            );
          })}
        </section>
      ) : null}
    </div>
  );
}
