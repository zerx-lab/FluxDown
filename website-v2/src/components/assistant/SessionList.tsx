import { useEffect, useRef, useState } from "react";
import { MessageSquarePlus, Search, Trash2, X } from "lucide-react";
import type { Lang } from "@/i18n/config";
import type { AssistantMessages } from "@/i18n/messages/assistant";
import { accountErrorText } from "@/lib/account/client";
import { formatRelative } from "@/components/account/dashboard/format";
import { ErrorLine, Skeleton, Spinner } from "@/components/account/dashboard/ui";
import ConfirmButton from "./ConfirmButton";
import { NEW_CHAT_ARIA_SHORTCUT, newChatShortcutLabel } from "./shortcuts";
import type { SessionsState } from "./useAssistant";

/** 输入停顿多久后才发起搜索请求。 */
const SEARCH_DEBOUNCE_MS = 300;

interface Props {
  lang: Lang;
  t: AssistantMessages;
  sessions: SessionsState;
  selectedId: string | null;
  /** 正在回答的会话（本页发起 / 续读的提问）。 */
  streamingId: string | null;
  onSelect: (id: string | null) => void;
  onRetry: () => void;
  onMore: () => void;
  onSearch: (query: string) => void;
  /** 清空全部会话；失败返回错误码。 */
  onClearAll: () => Promise<string | null>;
  /** 铺满父容器（悬浮面板的历史视图）；缺省在窄屏限高，给下方对话区让位。 */
  fill?: boolean;
}

/** 会话列表：新对话 + 搜索 + 本人会话（最近更新在前，分页加载）+ 清空全部（再点一次确认）。 */
export default function SessionList({ lang, t, sessions, selectedId, streamingId, onSelect, onRetry, onMore, onSearch, onClearAll, fill = false }: Props) {
  const [query, setQuery] = useState(sessions.query);
  const [clearing, setClearing] = useState(false);
  const [clearError, setClearError] = useState<string | null>(null);
  const searchRef = useRef(onSearch);
  searchRef.current = onSearch;
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  useEffect(() => {
    const timer = setTimeout(() => searchRef.current(query), SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [query]);

  const searching = sessions.query !== "";
  const clearAll = async () => {
    setClearing(true);
    setClearError(null);
    const code = await onClearAll();
    if (!mounted.current) return;
    setClearing(false);
    if (code !== null) setClearError(accountErrorText(lang, code));
    else setQuery("");
  };

  return (
    <nav aria-label={t.sidebar.label} className={`flex min-h-0 flex-col ${fill ? "flex-1 px-4 py-4" : ""}`}>
      <button
        type="button"
        onClick={() => onSelect(null)}
        aria-pressed={selectedId === null}
        title={`${t.sidebar.newChat} (${newChatShortcutLabel()})`}
        aria-keyshortcuts={NEW_CHAT_ARIA_SHORTCUT}
        className="btn btn-secondary w-full justify-start"
      >
        <MessageSquarePlus size={15} aria-hidden />
        {t.sidebar.newChat}
      </button>
      <div className="relative mt-3">
        <Search size={14} aria-hidden className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-subtle" />
        <input
          type="search"
          value={query}
          maxLength={100}
          onChange={(event) => setQuery(event.target.value)}
          placeholder={t.sidebar.searchPlaceholder}
          aria-label={t.sidebar.searchLabel}
          className="w-full rounded-lg border border-line bg-transparent py-1.5 pl-8 pr-8 text-[13px] outline-none placeholder:text-subtle focus:border-line-strong"
        />
        {query !== "" && (
          <button
            type="button"
            onClick={() => setQuery("")}
            aria-label={t.chat.renameCancel}
            className="absolute right-1.5 top-1/2 grid size-6 -translate-y-1/2 place-items-center rounded text-subtle hover:text-fg"
          >
            <X size={13} aria-hidden />
          </button>
        )}
      </div>
      <div className={`mt-3 min-h-0 flex-1 overflow-y-auto ${fill ? "" : "max-lg:max-h-56"}`}>
        {sessions.status === "loading" && (
          <div className="space-y-2" aria-hidden>
            {[0, 1, 2, 3].map((i) => (
              <Skeleton key={i} className="h-11 w-full" />
            ))}
          </div>
        )}
        {sessions.status === "error" && (
          <div role="alert" className="px-1 text-[13px] text-muted">
            <p>{t.sidebar.loadFailed}</p>
            <p className="mt-1 text-subtle">{accountErrorText(lang, sessions.errorCode ?? "fallback")}</p>
            <button type="button" onClick={onRetry} className="btn btn-secondary btn-sm mt-3">
              {t.sidebar.retry}
            </button>
          </div>
        )}
        {sessions.status === "ready" && sessions.items.length === 0 && (
          <p className="px-1 text-[13px] text-muted">{searching ? t.sidebar.searchEmpty : t.sidebar.empty}</p>
        )}
        {sessions.status === "ready" && sessions.items.length > 0 && (
          <ul className="space-y-0.5">
            {sessions.items.map((session) => {
              const current = session.id === selectedId;
              const answering = session.id === streamingId || session.active;
              return (
                <li key={session.id}>
                  <button
                    type="button"
                    onClick={() => onSelect(session.id)}
                    aria-current={current ? "page" : undefined}
                    className={`group block w-full rounded-lg px-3 py-2 text-left transition-colors ${current ? "bg-inset" : "hover:bg-inset/60"}`}
                  >
                    <span title={session.title} className={`block truncate text-[13.5px] ${current ? "font-medium text-fg" : "text-muted"}`}>
                      {session.title || t.sidebar.untitled}
                    </span>
                    <span className="mt-0.5 flex items-center gap-2 text-[11.5px] text-subtle">
                      <span>{formatRelative(session.updatedAt, lang)}</span>
                      {session.channel !== "website" && <span className="rounded border border-line px-1 leading-4">{t.sidebar.fromApp}</span>}
                      {answering && (
                        <span className="flex items-center gap-1 text-accent-ink">
                          <span aria-hidden className="size-1.5 rounded-full bg-current" />
                          {t.sidebar.answering}
                        </span>
                      )}
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        )}
        {sessions.status === "ready" && sessions.items.length < sessions.total && (
          <button type="button" onClick={onMore} disabled={sessions.loadingMore} className="btn btn-secondary btn-sm mt-3 w-full disabled:opacity-60">
            {sessions.loadingMore && <Spinner size={13} />}
            {t.sidebar.loadMore}
          </button>
        )}
      </div>
      {sessions.status === "ready" && sessions.total > 0 && !searching && (
        <div className="mt-3 border-t border-line pt-3">
          <ConfirmButton
            label={t.sidebar.clearAll}
            confirmLabel={t.sidebar.clearAllConfirm}
            busy={clearing}
            className="btn btn-secondary btn-sm w-full justify-start text-muted"
            onConfirm={() => void clearAll()}
          >
            <Trash2 size={13} aria-hidden />
            {t.sidebar.clearAll}
          </ConfirmButton>
          <ErrorLine text={clearError} />
        </div>
      )}
    </nav>
  );
}
