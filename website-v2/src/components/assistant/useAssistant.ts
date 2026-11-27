import { useCallback, useEffect, useRef, useState } from "react";
import { HTML_LANG, type Lang } from "@/i18n/config";
import { accountApi, accountStream } from "@/lib/account/client";
import type {
  AiConfig, AiFailReason, AiIssue, AiRating, AiSession, AiSessionDetail, AiSessionKind, AiSessionPage,
} from "@/lib/account/types";
import type { Failure, Guard } from "@/components/account/dashboard/resource";
import { applyEvent, asStreamEvent, settle, startRun, type LiveRun, type Segment } from "./live";

const PAGE_SIZE = 30;

export interface SessionsState {
  status: "loading" | "ready" | "error";
  items: AiSession[];
  total: number;
  errorCode: string | null;
  loadingMore: boolean;
}

export interface DetailState {
  status: "idle" | "loading" | "ready" | "error";
  data: AiSessionDetail | null;
  errorCode: string | null;
}

/**
 * 非 done 收尾的提示（停止 / 失败 / 中断），显示在该会话末尾直到下一次提问或切换会话。
 * 失败的回答不落库，`segments` 保留这次已经显示的过程（工具步骤与中间文本）。
 */
export interface RunNotice {
  sessionId: string;
  phase: "stopped" | "failed" | "interrupted";
  reason: AiFailReason | null;
  segments: Segment[];
}

/** 本地已知的回答评价：消息 ID → 评价（`null` = 已清除）。 */
export type Ratings = ReadonlyMap<string, AiRating | null>;

/** 一条回答当前的评价：本地覆盖优先，否则用消息自带的。 */
export function ratingOf(ratings: Ratings, messageId: string, saved: AiRating | null | undefined): AiRating | null {
  return ratings.has(messageId) ? (ratings.get(messageId) ?? null) : (saved ?? null);
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
  select: (sessionId: string | null) => void;
  loadSessions: () => Promise<void>;
  loadMore: () => Promise<void>;
  loadDetail: (sessionId: string, quiet?: boolean) => Promise<boolean>;
  /** `images`：data URL（已在浏览器侧按 `config` 限制校验 / 压缩）。 */
  send: (text: string, kind: AiSessionKind, images?: string[]) => Promise<boolean>;
  stop: () => void;
  remove: (sessionId: string) => Promise<string | null>;
  /** 评价一条回答（assistant 消息 ID）；`null` 清除。失败返回错误码（已回滚）。 */
  rate: (messageId: string, rating: AiRating | null) => Promise<string | null>;
  updateIssue: (issue: AiIssue) => void;
}

/**
 * 助手页的数据与动作：会话列表、当前会话详情、进行中的提问（SSE）与收尾刷新。
 * 「新对话」不预先建会话，首次发送时才 `POST ai/sessions`。`initialSessionId` 挂载即打开该会话
 * （已不存在时回到新对话）。
 */
export function useAssistant(lang: Lang, guard: Guard, initialConfig: AiConfig, initialSessionId: string | null = null): AssistantState {
  const [config, setConfig] = useState(initialConfig);
  const [sessions, setSessions] = useState<SessionsState>({
    status: "loading", items: [], total: 0, errorCode: null, loadingMore: false,
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
  const selectedRef = useRef(selectedId);
  selectedRef.current = selectedId;
  const detailSeq = useRef(0);
  const listSeq = useRef(0);
  const abortRef = useRef<AbortController | null>(null);
  const streamingRef = useRef(false);

  const loadSessions = useCallback(async () => {
    const id = ++listSeq.current;
    const res = await accountApi<AiSessionPage>(`ai/sessions?limit=${PAGE_SIZE}&offset=0`);
    if (id !== listSeq.current) return;
    if (res.ok) {
      setSessions({ status: "ready", items: res.data.items, total: res.data.total, errorCode: null, loadingMore: false });
    } else if (!guardRef.current(res)) {
      setSessions((s) => ({ ...s, status: s.items.length ? "ready" : "error", errorCode: res.code, loadingMore: false }));
    }
  }, []);

  const loadMore = useCallback(async () => {
    const offset = sessions.items.length;
    setSessions((s) => ({ ...s, loadingMore: true }));
    const id = ++listSeq.current;
    const res = await accountApi<AiSessionPage>(`ai/sessions?limit=${PAGE_SIZE}&offset=${offset}`);
    if (id !== listSeq.current) return;
    if (res.ok) {
      setSessions((s) => {
        const known = new Set(s.items.map((item) => item.id));
        const items = [...s.items, ...res.data.items.filter((item) => !known.has(item.id))];
        return { status: "ready", items, total: res.data.total, errorCode: null, loadingMore: false };
      });
    } else if (!guardRef.current(res)) {
      setSessions((s) => ({ ...s, errorCode: res.code, loadingMore: false }));
    }
  }, [sessions.items.length]);

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
      setNotice(null);
      if (sessionId === null) {
        detailSeq.current += 1;
        setDetail({ status: "idle", data: null, errorCode: null });
      } else {
        void loadDetail(sessionId);
      }
    },
    [loadDetail],
  );

  const putRatings = useCallback((next: Ratings) => {
    ratingsRef.current = next;
    setRatings(next);
  }, []);

  /** 发送一条提问；请求被拒（未开始回答）时返回 false，调用方把文字与图片放回输入框。 */
  const send = useCallback(
    async (text: string, kind: AiSessionKind, images: string[] = []): Promise<boolean> => {
      if (streamingRef.current) return false;
      streamingRef.current = true;
      setSendError(null);
      setNotice(null);
      // 每次提问都带当前站点语言：会话中途切换语言后，云端按新语言回答。
      const locale = HTML_LANG[lang];
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

        const controller = new AbortController();
        abortRef.current = controller;
        let run = startRun(sessionId, text, images);
        setLive(run);
        const res = await accountStream(
          `ai/sessions/${encodeURIComponent(sessionId)}/messages`,
          images.length > 0 ? { text, images, locale } : { text, locale },
          (raw) => {
            const event = asStreamEvent(raw);
            if (!event) return;
            run = applyEvent(run, event);
            setLive(run);
          },
          controller.signal,
        );
        abortRef.current = null;

        if (!res.ok && !run.started && !controller.signal.aborted) {
          setLive(null);
          if (!guardRef.current(res)) setSendError(res);
          return false;
        }
        run = settle(run, controller.signal.aborted);
        setLive(run);

        const finished = run;
        const [loaded] = await Promise.all([loadDetail(sessionId, true), loadSessions(), refreshConfig()]);
        if (loaded) setLive((current) => (current === finished ? null : current));
        if (finished.phase !== "done" && finished.phase !== "streaming") {
          setNotice({ sessionId, phase: finished.phase, reason: finished.failReason, segments: finished.segments });
        }
        return true;
      } finally {
        streamingRef.current = false;
      }
    },
    [lang, loadDetail, loadSessions, refreshConfig],
  );

  const stop = useCallback(() => {
    const sessionId = live?.sessionId;
    abortRef.current?.abort();
    if (sessionId) void accountApi(`ai/sessions/${encodeURIComponent(sessionId)}/cancel`, { method: "POST" });
  }, [live?.sessionId]);

  /** 删除会话；失败返回错误码。 */
  const remove = useCallback(
    async (sessionId: string): Promise<string | null> => {
      const res = await accountApi(`ai/sessions/${encodeURIComponent(sessionId)}`, { method: "DELETE" });
      if (!res.ok) return guardRef.current(res) ? null : res.code;
      setSessions((s) => ({ ...s, items: s.items.filter((item) => item.id !== sessionId), total: Math.max(0, s.total - 1) }));
      if (selectedRef.current === sessionId) select(null);
      return null;
    },
    [select],
  );

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
  const updateIssue = useCallback((issue: AiIssue) => {
    const swap = (issues: AiIssue[]) => issues.map((item) => (item.id === issue.id ? issue : item));
    setDetail((d) => (d.data ? { ...d, data: { ...d.data, issues: swap(d.data.issues) } } : d));
    setLive((run) => (run ? { ...run, issues: swap(run.issues) } : run));
  }, []);

  return {
    config, sessions, selectedId, detail, live, notice, sendError, ratings,
    select, loadSessions, loadMore, loadDetail, send, stop, remove, rate, updateIssue,
  };
}
