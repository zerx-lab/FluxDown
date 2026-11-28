import { useCallback, useEffect, useRef, useState } from "react";
import { HTML_LANG, type Lang } from "@/i18n/config";
import { accountApi, accountStream, type AccountResult } from "@/lib/account/client";
import type {
  AiConfig, AiExport, AiFailReason, AiIssue, AiMessage, AiRating, AiSession, AiSessionDetail, AiSessionKind, AiSessionPage,
} from "@/lib/account/types";
import type { Failure, Guard } from "@/components/account/dashboard/resource";
import { attachmentUrl } from "./attachments";
import { downloadText, type ExportFormat } from "./download";
import { applyEvent, asStreamEvent, resumeTarget, settle, startRun, type LiveRun, type Segment } from "./live";

const PAGE_SIZE = 30;
/** BFF 单页上限（`ai-routes.ts::MAX_PAGE`）：刷新列表时按已加载的条数重拉，最多这么多。 */
const MAX_PAGE = 100;
/** 流中途断开后的续读：连续这么多次没有新事件就放弃，标为中断。 */
const MAX_RESUME_IDLE = 3;
const RESUME_DELAY_MS = 1000;
/** 会话仍在回答、却没有可挂接的流时，轮询详情的间隔。 */
const ACTIVE_POLL_MS = 4000;

export interface SessionsState {
  status: "loading" | "ready" | "error";
  items: AiSession[];
  total: number;
  errorCode: string | null;
  loadingMore: boolean;
  /** 当前生效的搜索词（空 = 不搜索）。 */
  query: string;
}

export interface DetailState {
  status: "idle" | "loading" | "ready" | "error";
  data: AiSessionDetail | null;
  errorCode: string | null;
}

/**
 * 非 done 收尾的提示（停止 / 失败 / 中断），显示在该会话末尾直到下一次提问或切换会话。
 * 云端会把已推送的部分回答落库（`interrupted`），详情里有该次执行的消息时只显示这行提示；
 * 旧版云端不落库，`segments` 保留这次已经显示的过程（工具步骤与中间文本）。
 */
export interface RunNotice {
  sessionId: string;
  /** 这次执行的 ID（用来判断历史里是否已经有它的消息）。 */
  runId: string | null;
  phase: "stopped" | "failed" | "interrupted";
  reason: AiFailReason | null;
  segments: Segment[];
  /** 这次执行里 `offer_membership` 已成功：提示之上仍显示开通会员卡片。 */
  membershipOffer: boolean;
}

/** 本地已知的回答评价：消息 ID → 评价（`null` = 已清除）。 */
export type Ratings = ReadonlyMap<string, AiRating | null>;

/** 一条回答当前的评价：本地覆盖优先，否则用消息自带的。 */
export function ratingOf(ratings: Ratings, messageId: string, saved: AiRating | null | undefined): AiRating | null {
  return ratings.has(messageId) ? (ratings.get(messageId) ?? null) : (saved ?? null);
}

/** 提问的附加选项：重新生成 / 编辑重发。 */
export interface SendOptions {
  /** 被替换的 user 消息 ID：它及之后的消息在云端被取代。 */
  replaceFrom?: string;
  /** 沿用被替换提问的附图（此时 `images` 须为空）。 */
  keepImages?: boolean;
  /** 沿用附图时，进行中的气泡里显示的缩略图地址。 */
  previewImages?: string[];
}

export interface AssistantState {
  /** 最近一次拉取的 `ai/config`（每次提问后刷新今日用量）。 */
  config: AiConfig;
  sessions: SessionsState;
  /** null = 新对话（尚未建会话）。 */
  selectedId: string | null;
  detail: DetailState;
  live: LiveRun | null;
  notice: RunNotice | null;
  /** 本地已知的回答评价（消息 ID → 评价，`null` = 已清除），盖在详情消息自带的 `rating` 之上；用 `ratingOf` 读取。 */
  ratings: Ratings;
  /** 最近一次提问在开始回答前被拒的原因。 */
  sendError: Failure | null;
  /** 任何会话正在回答（本页发起或续读的流）：同一时间只允许一条流。 */
  busy: boolean;
  /** 当前选中的会话正在回答（本页的流，或云端还在跑、没有可挂接的流）：输入框显示停止。 */
  answering: boolean;
  select: (sessionId: string | null) => void;
  loadSessions: () => Promise<void>;
  loadMore: () => Promise<void>;
  /** 按标题 / 消息内容搜索会话；空串回到完整列表。 */
  search: (query: string) => void;
  loadDetail: (sessionId: string, quiet?: boolean) => Promise<boolean>;
  /** `images`：data URL（已在浏览器侧按 `config` 限制校验 / 压缩）。 */
  send: (text: string, kind: AiSessionKind, images?: string[], options?: SendOptions) => Promise<boolean>;
  stop: () => void;
  remove: (sessionId: string) => Promise<string | null>;
  /** 清空全部会话；失败返回错误码。 */
  clearAll: () => Promise<string | null>;
  /** 重命名（标题 trim 后 1–80 字符）；失败返回错误码。 */
  rename: (sessionId: string, title: string) => Promise<string | null>;
  /** 导出并下载；失败返回错误码。 */
  exportSession: (sessionId: string, format: ExportFormat) => Promise<string | null>;
  /** 评价一条回答（assistant 消息 ID）；`null` 清除。失败返回错误码（已回滚）。 */
  rate: (messageId: string, rating: AiRating | null) => Promise<string | null>;
  updateIssue: (issue: AiIssue) => void;
}

const sleep = (ms: number, signal: AbortSignal) => {
  const { promise, resolve } = Promise.withResolvers<void>();
  const done = () => {
    clearTimeout(timer);
    signal.removeEventListener("abort", done);
    resolve();
  };
  const timer = setTimeout(done, ms);
  signal.addEventListener("abort", done, { once: true });
  return promise;
};

const searchSuffix = (query: string): string => (query === "" ? "" : `&q=${encodeURIComponent(query)}`);

function noticeOf(run: LiveRun): RunNotice | null {
  if (run.phase === "done" || run.phase === "streaming") return null;
  return {
    sessionId: run.sessionId,
    runId: run.runId,
    phase: run.phase,
    reason: run.failReason,
    segments: run.segments,
    membershipOffer: run.membershipOffer,
  };
}

/**
 * 驱动一次执行的流：先按 `open` 打开（提问 POST 或续读 GET），把事件归约进 `run`；
 * 流没带终止事件就断了（网络中断 / 代理掐断）且已有事件序号时，用 `GET ai/runs/{runId}/events?after=<seq>` 续读，
 * 连续 `MAX_RESUME_IDLE` 次没有新事件、被上游拒绝（含 410 执行已过期）或用户中止才放弃。
 */
async function drive(
  initial: LiveRun,
  open: { path: string; body?: unknown },
  signal: AbortSignal,
  onRun: (run: LiveRun) => void,
): Promise<{ run: LiveRun; result: AccountResult<undefined> }> {
  let run = initial;
  let target = open;
  let idle = 0;
  for (;;) {
    const before = run.lastSeq;
    const result = await accountStream(
      target.path,
      target.body,
      (raw, id) => {
        const event = asStreamEvent(raw);
        if (!event) return;
        run = applyEvent(run, event, id);
        onRun(run);
      },
      signal,
    );
    if (run.phase !== "streaming" || signal.aborted) return { run, result };
    // 上游明确拒绝（额度 / 忙 / 已过期……）、还没收到任何事件、或上游不带事件序号（没法安全续读）：不再重试
    if ((!result.ok && result.status !== 0) || !run.started || run.runId === null || run.lastSeq === 0) return { run, result };
    idle = run.lastSeq === before ? idle + 1 : 0;
    if (idle >= MAX_RESUME_IDLE) return { run, result };
    await sleep(RESUME_DELAY_MS, signal);
    if (signal.aborted) return { run, result };
    target = { path: `ai/runs/${encodeURIComponent(run.runId)}/events?after=${run.lastSeq}` };
  }
}

/**
 * 助手页的数据与动作：会话列表、当前会话详情、进行中的提问（SSE）与收尾刷新。
 * 「新对话」不预先建会话，首次发送时才 `POST ai/sessions`。`initialSessionId` 挂载即打开该会话
 * （已不存在时回到新对话）。
 *
 * 流只在本页（组件）存活期间维持：卸载（关闭悬浮面板 / 换页）即断开读流，云端宽限期内继续执行；
 * 之后重新挂载并打开该会话时，若详情仍是「回答中」，用 `GET ai/runs/{runId}/events?after=0` 接回整个过程。
 */
export function useAssistant(lang: Lang, guard: Guard, initialConfig: AiConfig, initialSessionId: string | null = null): AssistantState {
  const [config, setConfig] = useState(initialConfig);
  const [sessions, setSessions] = useState<SessionsState>({
    status: "loading", items: [], total: 0, errorCode: null, loadingMore: false, query: "",
  });
  const [selectedId, setSelectedId] = useState<string | null>(initialSessionId);
  const [detail, setDetail] = useState<DetailState>({
    status: initialSessionId === null ? "idle" : "loading", data: null, errorCode: null,
  });
  const [live, setLive] = useState<LiveRun | null>(null);
  const [notice, setNotice] = useState<RunNotice | null>(null);
  const [sendError, setSendError] = useState<Failure | null>(null);
  const [ratings, setRatings] = useState<Ratings>(new Map());
  const ratingsRef = useRef(ratings);
  const ratingBusy = useRef(new Set<string>());

  const guardRef = useRef(guard);
  guardRef.current = guard;
  const sessionsRef = useRef(sessions);
  sessionsRef.current = sessions;
  const selectedRef = useRef(selectedId);
  selectedRef.current = selectedId;
  const liveRef = useRef<LiveRun | null>(null);
  const queryRef = useRef("");
  const detailSeq = useRef(0);
  const listSeq = useRef(0);
  const abortRef = useRef<AbortController | null>(null);
  const streamingRef = useRef(false);
  const unmounted = useRef(false);
  /** 已经（由本页发起或续读过）接管的执行：`<sessionId>:<runId>`，不再重复续读。 */
  const attachedRef = useRef<string | null>(null);

  const putLive = useCallback((run: LiveRun | null) => {
    liveRef.current = run;
    setLive(run);
  }, []);

  useEffect(() => {
    unmounted.current = false;
    return () => {
      unmounted.current = true;
      abortRef.current?.abort();
    };
  }, []);

  const loadSessions = useCallback(async () => {
    const id = ++listSeq.current;
    const query = queryRef.current;
    // 已「加载更多」的页不收起：按已加载的条数重拉第一段，超出的尾部原样保留
    const limit = Math.min(MAX_PAGE, Math.max(PAGE_SIZE, sessionsRef.current.items.length));
    const res = await accountApi<AiSessionPage>(`ai/sessions?limit=${limit}&offset=0${searchSuffix(query)}`);
    if (id !== listSeq.current) return;
    if (res.ok) {
      setSessions((s) => {
        const fresh = new Set(res.data.items.map((item) => item.id));
        const tail = s.items.slice(limit).filter((item) => !fresh.has(item.id));
        return { status: "ready", items: [...res.data.items, ...tail], total: res.data.total, errorCode: null, loadingMore: false, query };
      });
    } else if (!guardRef.current(res)) {
      setSessions((s) => ({ ...s, status: s.items.length ? "ready" : "error", errorCode: res.code, loadingMore: false }));
    }
  }, []);

  const loadMore = useCallback(async () => {
    const offset = sessionsRef.current.items.length;
    const query = queryRef.current;
    setSessions((s) => ({ ...s, loadingMore: true }));
    const id = ++listSeq.current;
    const res = await accountApi<AiSessionPage>(`ai/sessions?limit=${PAGE_SIZE}&offset=${offset}${searchSuffix(query)}`);
    if (id !== listSeq.current) return;
    if (res.ok) {
      setSessions((s) => {
        const known = new Set(s.items.map((item) => item.id));
        const items = [...s.items, ...res.data.items.filter((item) => !known.has(item.id))];
        return { status: "ready", items, total: res.data.total, errorCode: null, loadingMore: false, query };
      });
    } else if (!guardRef.current(res)) {
      setSessions((s) => ({ ...s, errorCode: res.code, loadingMore: false }));
    }
  }, []);

  const search = useCallback(
    (query: string) => {
      const next = query.trim();
      if (next === queryRef.current) return;
      queryRef.current = next;
      sessionsRef.current = { status: "loading", items: [], total: 0, errorCode: null, loadingMore: false, query: next };
      setSessions(sessionsRef.current);
      void loadSessions();
    },
    [loadSessions],
  );

  const refreshConfig = useCallback(async () => {
    const res = await accountApi<AiConfig>("ai/config");
    if (res.ok) setConfig(res.data);
    else guardRef.current(res);
  }, []);

  /** 拉取会话详情；成功返回 true（调用方据此收起进行中的展示）。 */
  const loadDetail = useCallback(async (sessionId: string, quiet = false): Promise<boolean> => {
    const id = ++detailSeq.current;
    if (!quiet) setDetail({ status: "loading", data: null, errorCode: null });
    const res = await accountApi<AiSessionDetail>(`ai/sessions/${encodeURIComponent(sessionId)}`);
    if (id !== detailSeq.current || selectedRef.current !== sessionId) return false;
    if (res.ok) {
      setDetail({ status: "ready", data: res.data, errorCode: null });
      return true;
    }
    if (guardRef.current(res)) return false;
    if (res.status === 404 && !quiet) {
      selectedRef.current = null;
      setSelectedId(null);
      setDetail({ status: "idle", data: null, errorCode: null });
      setSessions((s) => ({ ...s, items: s.items.filter((item) => item.id !== sessionId) }));
      return false;
    }
    setDetail((d) => (quiet && d.data ? d : { status: "error", data: null, errorCode: res.code }));
    return false;
  }, []);

  useEffect(() => {
    void loadSessions();
    if (selectedRef.current !== null) void loadDetail(selectedRef.current);
  }, [loadSessions, loadDetail]);

  const select = useCallback(
    (sessionId: string | null) => {
      if (sessionId === selectedRef.current) return;
      selectedRef.current = sessionId;
      setSelectedId(sessionId);
      setSendError(null);
      // 回答期间切走的会话：流结束时详情没刷新（当时不是当前会话），切回来先收起已结束的展示，
      // 否则详情里已有的这次问答会和进行中的展示各画一遍；非 done 的结局转成末尾提示。
      const finished = liveRef.current;
      if (finished && sessionId !== null && finished.sessionId === sessionId && finished.phase !== "streaming") {
        putLive(null);
        setNotice(noticeOf(finished));
      } else {
        setNotice(null);
      }
      if (sessionId === null) {
        detailSeq.current += 1;
        setDetail({ status: "idle", data: null, errorCode: null });
      } else {
        void loadDetail(sessionId);
      }
    },
    [loadDetail, putLive],
  );

  const putRatings = useCallback((next: Ratings) => {
    ratingsRef.current = next;
    setRatings(next);
  }, []);

  const patchSession = useCallback((session: AiSession) => {
    setSessions((s) => ({ ...s, items: s.items.map((item) => (item.id === session.id ? session : item)) }));
    setDetail((d) => (d.data && d.data.session.id === session.id ? { ...d, data: { ...d.data, session } } : d));
  }, []);

  /** 一次执行收尾：刷新详情 / 列表 / 用量；详情成功后收起进行中的展示，非 done 的结局留一行提示。 */
  const finalize = useCallback(
    async (run: LiveRun) => {
      putLive(run);
      const [loaded] = await Promise.all([loadDetail(run.sessionId, true), loadSessions(), refreshConfig()]);
      if (unmounted.current) return;
      if (loaded && liveRef.current === run) putLive(null);
      setNotice(noticeOf(run));
    },
    [loadDetail, loadSessions, putLive, refreshConfig],
  );

  /** 进行中的执行每有新状态：落到界面，并记下已接管（详情里「回答中」不再触发续读）。 */
  const track = useCallback(
    (run: LiveRun) => {
      if (run.runId !== null) attachedRef.current = `${run.sessionId}:${run.runId}`;
      putLive(run);
    },
    [putLive],
  );

  /** 发送一条提问；请求被拒（未开始回答）时返回 false，调用方把文字与图片放回输入框。 */
  const send = useCallback(
    async (text: string, kind: AiSessionKind, images: string[] = [], options: SendOptions = {}): Promise<boolean> => {
      if (streamingRef.current) return false;
      streamingRef.current = true;
      setSendError(null);
      setNotice(null);
      // 每次提问都带当前站点语言：会话中途切换语言后，云端按新语言回答。
      const locale = HTML_LANG[lang];
      const controller = new AbortController();
      try {
        let sessionId = selectedRef.current;
        if (sessionId === null) {
          const created = await accountApi<AiSession>("ai/sessions", { body: { kind, locale } });
          if (!created.ok) {
            if (!guardRef.current(created)) setSendError(created);
            return false;
          }
          const session = created.data;
          sessionId = session.id;
          selectedRef.current = sessionId;
          setSelectedId(sessionId);
          setDetail({ status: "ready", data: { session, messages: [], issues: [] }, errorCode: null });
          setSessions((s) => ({ ...s, items: [session, ...s.items], total: s.total + 1 }));
        }

        abortRef.current = controller;
        const { replaceFrom, keepImages, previewImages } = options;
        if (replaceFrom !== undefined) {
          // 被替换的消息在云端已不可见：先从界面拿掉，被拒时再重拉还原
          setDetail((d) => {
            const index = d.data?.messages.findIndex((message) => message.id === replaceFrom) ?? -1;
            return d.data && index >= 0 ? { ...d, data: { ...d.data, messages: d.data.messages.slice(0, index) } } : d;
          });
        }
        const initial = startRun(sessionId, text, keepImages ? (previewImages ?? []) : images);
        putLive(initial);
        const body: Record<string, unknown> = { text, locale };
        if (images.length > 0) body.images = images;
        if (replaceFrom !== undefined) body.replaceFrom = replaceFrom;
        if (keepImages) body.keepImages = true;
        const { run, result } = await drive(initial, { path: `ai/sessions/${encodeURIComponent(sessionId)}/messages`, body }, controller.signal, track);
        abortRef.current = null;
        if (unmounted.current) return true;

        if (!result.ok && !run.started && !controller.signal.aborted) {
          putLive(null);
          if (!guardRef.current(result)) setSendError(result);
          if (replaceFrom !== undefined) void loadDetail(sessionId, true);
          return false;
        }
        await finalize(settle(run, controller.signal.aborted));
        return true;
      } finally {
        streamingRef.current = false;
      }
    },
    [lang, finalize, loadDetail, putLive, track],
  );

  /** 接回云端还在跑的执行：从头补发事件，重建进行中的展示。 */
  const resume = useCallback(
    async (sessionId: string, target: { runId: string; question: string; imageIds: string[] }) => {
      streamingRef.current = true;
      const controller = new AbortController();
      abortRef.current = controller;
      try {
        const initial = startRun(sessionId, target.question, target.imageIds.map(attachmentUrl), target.runId);
        putLive(initial);
        const { run, result } = await drive(
          initial,
          { path: `ai/runs/${encodeURIComponent(target.runId)}/events?after=0` },
          controller.signal,
          track,
        );
        abortRef.current = null;
        if (unmounted.current) return;
        if (!result.ok && !run.started && !controller.signal.aborted) {
          // 执行已过期（410）或连不上：没法接回，给「中断」提示，并按最新详情显示
          if (guardRef.current(result)) {
            putLive(null);
            return;
          }
          await finalize({ ...settle(run, false), phase: "interrupted" });
          return;
        }
        await finalize(settle(run, controller.signal.aborted));
      } finally {
        streamingRef.current = false;
      }
    },
    [finalize, putLive, track],
  );

  const sessionActive = detail.data !== null && detail.data.session.id === selectedId && detail.data.session.active;

  // 打开一个云端仍在回答的官网会话（换页 / 关闭再打开悬浮面板 / 刷新后）：接回它的流，显示「回答中 / 停止」
  useEffect(() => {
    const data = detail.data;
    if (!data || live !== null || streamingRef.current || !data.session.active) return;
    if (data.session.id !== selectedId || data.session.channel !== "website") return;
    const target = resumeTarget(data.messages);
    if (!target) return;
    const key = `${data.session.id}:${target.runId}`;
    if (attachedRef.current === key) return;
    attachedRef.current = key;
    void resume(data.session.id, target);
  }, [detail.data, live, selectedId, resume]);

  // 云端还在回答、本页没有可挂接的流（续读被拒、来自桌面端的会话）：隔一会儿刷新详情，答完自然变回可输入
  useEffect(() => {
    if (!sessionActive || live !== null || selectedId === null) return;
    const timer = setInterval(() => {
      if (!streamingRef.current) void loadDetail(selectedId, true);
    }, ACTIVE_POLL_MS);
    return () => clearInterval(timer);
  }, [sessionActive, live, selectedId, loadDetail]);

  const stop = useCallback(() => {
    const sessionId = liveRef.current?.sessionId ?? selectedRef.current;
    abortRef.current?.abort();
    if (sessionId) void accountApi(`ai/sessions/${encodeURIComponent(sessionId)}/cancel`, { method: "POST" });
  }, []);

  /** 删除会话；失败返回错误码。 */
  const remove = useCallback(
    async (sessionId: string): Promise<string | null> => {
      const res = await accountApi(`ai/sessions/${encodeURIComponent(sessionId)}`, { method: "DELETE" });
      if (!res.ok) return guardRef.current(res) ? null : res.code;
      if (liveRef.current?.sessionId === sessionId) {
        abortRef.current?.abort();
        putLive(null);
      }
      setSessions((s) => ({ ...s, items: s.items.filter((item) => item.id !== sessionId), total: Math.max(0, s.total - 1) }));
      if (selectedRef.current === sessionId) select(null);
      return null;
    },
    [putLive, select],
  );

  /** 清空全部会话（云端先取消进行中的执行）；失败返回错误码。 */
  const clearAll = useCallback(async (): Promise<string | null> => {
    const res = await accountApi("ai/sessions", { method: "DELETE" });
    if (!res.ok) return guardRef.current(res) ? null : res.code;
    abortRef.current?.abort();
    putLive(null);
    setNotice(null);
    setSessions((s) => ({ ...s, status: "ready", items: [], total: 0, errorCode: null }));
    select(null);
    return null;
  }, [putLive, select]);

  const rename = useCallback(
    async (sessionId: string, title: string): Promise<string | null> => {
      const res = await accountApi<AiSession>(`ai/sessions/${encodeURIComponent(sessionId)}`, { method: "PATCH", body: { title } });
      if (!res.ok) return guardRef.current(res) ? null : res.code;
      patchSession(res.data);
      return null;
    },
    [patchSession],
  );

  const exportSession = useCallback(async (sessionId: string, format: ExportFormat): Promise<string | null> => {
    const res = await accountApi<AiExport>(`ai/sessions/${encodeURIComponent(sessionId)}/export?format=${format}`);
    if (!res.ok) return guardRef.current(res) ? null : res.code;
    downloadText(res.data.fileName, format, res.data.content);
    return null;
  }, []);

  /**
   * 评价一条回答（`rating` = null 清除）；乐观更新，失败回滚并返回错误码。
   * 同一条回答的上一次请求未结束时忽略新的点击。
   */
  const rate = useCallback(
    async (messageId: string, rating: AiRating | null): Promise<string | null> => {
      const sessionId = selectedRef.current;
      if (sessionId === null) return "fallback";
      if (ratingBusy.current.has(messageId)) return null;
      ratingBusy.current.add(messageId);
      const previous = ratingsRef.current;
      putRatings(new Map(previous).set(messageId, rating));
      try {
        const res = await accountApi(
          `ai/sessions/${encodeURIComponent(sessionId)}/messages/${encodeURIComponent(messageId)}/rating`,
          { method: "PUT", body: { rating } },
        );
        if (res.ok) return null;
        // 回滚只撤销这一条回答的覆盖，不动期间其它回答的评价
        const next = new Map(ratingsRef.current);
        if (previous.has(messageId)) next.set(messageId, previous.get(messageId) ?? null);
        else next.delete(messageId);
        putRatings(next);
        return guardRef.current(res) ? null : res.code;
      } finally {
        ratingBusy.current.delete(messageId);
      }
    },
    [putRatings],
  );

  /** 问题单确认 / 放弃后的最新状态写回详情与进行中的展示。 */
  const updateIssue = useCallback(
    (issue: AiIssue) => {
      const swap = (issues: AiIssue[]) => issues.map((item) => (item.id === issue.id ? issue : item));
      setDetail((d) => (d.data ? { ...d, data: { ...d.data, issues: swap(d.data.issues) } } : d));
      const current = liveRef.current;
      if (current) putLive({ ...current, issues: swap(current.issues) });
    },
    [putLive],
  );

  const busy = live?.phase === "streaming";
  const liveHere = live !== null && live.sessionId === selectedId;
  const answering = liveHere ? live.phase === "streaming" : !busy && sessionActive;

  return {
    config, sessions, selectedId, detail, live, notice, sendError, ratings, busy, answering,
    select, loadSessions, loadMore, search, loadDetail, send, stop, remove, clearAll, rename, exportSession, rate, updateIssue,
  };
}

/** 历史里是否已有这次执行的回答 / 过程消息（有就不再重复画失败 / 中断时留在内存里的片段）。 */
export function runPersisted(messages: readonly AiMessage[], runId: string | null): boolean {
  return runId !== null && messages.some((message) => message.runId === runId && (message.role === "assistant" || message.role === "assistant_step"));
}
