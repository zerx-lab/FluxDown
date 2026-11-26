import { useCallback, useEffect, useRef, useState } from "react";
import type { AccountResult } from "@/lib/account/client";

/** 失败的请求结果(`AccountResult` 的 `ok: false` 分支)。 */
export type Failure = Extract<AccountResult<unknown>, { ok: false }>;

/** 返回 true = 已由根组件处理(401 跳登录 / 403 停用态),调用方不再展示错误。 */
export type Guard = (failure: Failure) => boolean;

export type ResourceStatus = "idle" | "loading" | "ready" | "error";

/**
 * 分区数据:挂载时 `load()` 首次拉取并缓存,切换分区不重复请求;`reload()` 强制刷新。
 * 刷新失败时保留旧数据(`status` 仍为 `ready`)并在 `errorCode` 里带上原因。
 */
export interface Resource<T> {
  status: ResourceStatus;
  data: T | null;
  /** 最近一次失败的错误码;成功后清空。 */
  errorCode: string | null;
  /** 已有数据时的后台刷新。 */
  refreshing: boolean;
  /** 仅 idle / error 且无在途请求时发请求。 */
  load: () => void;
  reload: () => void;
  /** 已加载过才刷新(付款后同步用量等)。 */
  reloadIfLoaded: () => void;
  /** 本地改写缓存(改名 / 移除后不必整表重拉)。 */
  mutate: (update: (data: T) => T) => void;
}

interface State<T> {
  status: ResourceStatus;
  data: T | null;
  errorCode: string | null;
  refreshing: boolean;
}

export function useResource<T>(fetcher: () => Promise<AccountResult<T>>, guard: Guard): Resource<T> {
  const [state, setState] = useState<State<T>>({ status: "idle", data: null, errorCode: null, refreshing: false });
  const latest = useRef(state);
  latest.current = state;
  const fetcherRef = useRef(fetcher);
  fetcherRef.current = fetcher;
  const guardRef = useRef(guard);
  guardRef.current = guard;
  const seq = useRef(0);
  const inflight = useRef(false);

  useEffect(
    () => () => {
      seq.current += 1; // 卸载后丢弃在途响应
    },
    [],
  );

  const run = useCallback(async () => {
    const id = ++seq.current;
    inflight.current = true;
    setState((s) => ({ ...s, status: s.data == null ? "loading" : "ready", refreshing: s.data != null, errorCode: null }));
    const res = await fetcherRef.current();
    if (id !== seq.current) return;
    inflight.current = false;
    if (res.ok) {
      setState({ status: "ready", data: res.data, errorCode: null, refreshing: false });
    } else if (!guardRef.current(res)) {
      setState((s) => ({ ...s, status: s.data == null ? "error" : "ready", refreshing: false, errorCode: res.code }));
    }
  }, []);

  const load = useCallback(() => {
    const { status } = latest.current;
    if (!inflight.current && (status === "idle" || status === "error")) void run();
  }, [run]);

  const reload = useCallback(() => {
    void run();
  }, [run]);

  const reloadIfLoaded = useCallback(() => {
    if (latest.current.status !== "idle") void run();
  }, [run]);

  const mutate = useCallback((update: (data: T) => T) => {
    setState((s) => (s.data == null ? s : { ...s, data: update(s.data) }));
  }, []);

  return { ...state, load, reload, reloadIfLoaded, mutate };
}
