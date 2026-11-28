import { useEffect, useRef, useState } from "react";
import { Check, Download, Pencil, Trash2, X } from "lucide-react";
import type { Lang } from "@/i18n/config";
import type { AssistantMessages } from "@/i18n/messages/assistant";
import { accountErrorText } from "@/lib/account/client";
import type { AiConfig, AiSession } from "@/lib/account/types";
import { ErrorLine, Spinner } from "@/components/account/dashboard/ui";
import ConfirmButton from "./ConfirmButton";
import type { ExportFormat } from "./download";

/** FluxCloud 会话标题 1..=80 字符。 */
const MAX_TITLE_CHARS = 80;

interface Props {
  lang: Lang;
  t: AssistantMessages;
  session: AiSession | null;
  config: AiConfig;
  /** 悬浮面板：不显示标题行与今日用量，只给动作（标题已在面板副标题里）。 */
  compact?: boolean;
  onDelete: (id: string) => Promise<string | null>;
  onRename: (id: string, title: string) => Promise<string | null>;
  onExport: (id: string, format: ExportFormat) => Promise<string | null>;
}

/** 对话区顶栏：标题（可重命名）、今日用量、导出与删除（删除需再点一次确认）；评价在每条回答下。 */
export default function ChatHeader({ lang, t, session, config, compact = false, onDelete, onRename, onExport }: Props) {
  const [busy, setBusy] = useState<"delete" | "rename" | "export" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [renaming, setRenaming] = useState(false);
  const [draft, setDraft] = useState("");
  const [exportOpen, setExportOpen] = useState(false);
  const [exported, setExported] = useState(false);
  const titleInput = useRef<HTMLInputElement>(null);
  const mounted = useRef(true);
  const sessionId = session?.id ?? null;

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  useEffect(() => {
    setRenaming(false);
    setExportOpen(false);
    setExported(false);
    setError(null);
  }, [sessionId]);

  useEffect(() => {
    if (renaming) titleInput.current?.select();
  }, [renaming]);

  useEffect(() => {
    if (!exported) return;
    const timer = setTimeout(() => setExported(false), 2500);
    return () => clearTimeout(timer);
  }, [exported]);

  const run = async (kind: NonNullable<typeof busy>, action: () => Promise<string | null>): Promise<boolean> => {
    setBusy(kind);
    setError(null);
    const code = await action();
    if (!mounted.current) return false;
    setBusy(null);
    if (code !== null) setError(accountErrorText(lang, code));
    return code === null;
  };

  const startRename = () => {
    if (!session) return;
    setDraft(session.title);
    setRenaming(true);
    setExportOpen(false);
  };

  const saveRename = async () => {
    if (!session) return;
    const title = draft.trim();
    if (title === "" || title === session.title) {
      setRenaming(false);
      return;
    }
    if (await run("rename", () => onRename(session.id, title))) setRenaming(false);
  };

  const exportAs = async (format: ExportFormat) => {
    if (!session) return;
    setExportOpen(false);
    if (await run("export", () => onExport(session.id, format))) setExported(true);
  };

  const title = session ? session.title || t.sidebar.untitled : t.sidebar.newChat;
  const iconBtn = "ai-icon-btn shrink-0";
  const actionable = session !== null && session.messageCount > 0;

  return (
    <div className={compact ? "border-b border-line px-4 py-1.5" : "border-b border-line px-4 py-2.5 sm:px-6"}>
      <div className="flex min-h-9 items-center gap-2">
        {renaming ? (
          <form
            className="flex min-w-0 flex-1 items-center gap-1"
            onSubmit={(event) => {
              event.preventDefault();
              void saveRename();
            }}
          >
            <input
              ref={titleInput}
              value={draft}
              maxLength={MAX_TITLE_CHARS}
              disabled={busy === "rename"}
              placeholder={t.chat.renamePlaceholder}
              aria-label={t.chat.rename}
              onChange={(event) => setDraft(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  event.preventDefault();
                  event.stopPropagation();
                  setRenaming(false);
                }
              }}
              className="min-w-0 flex-1 rounded-md border border-line-strong bg-elev px-2 py-1 text-[14px] outline-none focus:border-accent"
            />
            <button type="submit" disabled={busy === "rename" || draft.trim() === ""} aria-label={t.chat.renameSave} title={t.chat.renameSave} className={`${iconBtn} disabled:opacity-50`}>
              {busy === "rename" ? <Spinner size={13} /> : <Check size={15} aria-hidden />}
            </button>
            <button type="button" onClick={() => setRenaming(false)} aria-label={t.chat.renameCancel} title={t.chat.renameCancel} className={iconBtn}>
              <X size={15} aria-hidden />
            </button>
          </form>
        ) : (
          <>
            {compact ? (
              <span className="min-w-0 flex-1 truncate text-[12.5px] text-subtle" aria-live="polite">
                {exported ? t.chat.exported : ""}
              </span>
            ) : (
              <h2 title={session?.title} className="min-w-0 flex-1 truncate text-[14px] font-semibold">
                {title}
              </h2>
            )}
            {!compact && exported && (
              <span role="status" className="shrink-0 text-[12px] text-ok">
                {t.chat.exported}
              </span>
            )}
            {!compact && config.dailyLimit > 0 && (
              <span className="hidden shrink-0 text-[12px] text-subtle sm:inline">{t.chat.quota(config.usedToday, config.dailyLimit)}</span>
            )}
            {session && (
              <>
                <button type="button" onClick={startRename} aria-label={t.chat.rename} title={t.chat.rename} className={iconBtn}>
                  <Pencil size={14} aria-hidden />
                </button>
                {actionable && (
                  <div className="relative">
                    <button
                      type="button"
                      onClick={() => setExportOpen((open) => !open)}
                      aria-label={t.chat.export}
                      title={t.chat.export}
                      aria-expanded={exportOpen}
                      aria-haspopup="true"
                      disabled={busy === "export"}
                      className={`${iconBtn} disabled:opacity-50`}
                    >
                      {busy === "export" ? <Spinner size={13} /> : <Download size={15} aria-hidden />}
                    </button>
                    {exportOpen && (
                      <div
                        role="group"
                        aria-label={t.chat.export}
                        onKeyDown={(event) => {
                          if (event.key === "Escape") setExportOpen(false);
                        }}
                        onBlur={(event) => {
                          if (!event.currentTarget.parentElement?.contains(event.relatedTarget as Node | null)) setExportOpen(false);
                        }}
                        className="absolute right-0 top-full z-20 mt-1 flex min-w-28 flex-col rounded-lg border border-line-strong bg-elev p-1 shadow-[var(--shadow-float)]"
                      >
                        <button type="button" autoFocus onClick={() => void exportAs("markdown")} className="rounded-md px-3 py-1.5 text-left text-[13px] text-muted hover:bg-inset hover:text-fg">
                          {t.chat.exportMarkdown}
                        </button>
                        <button type="button" onClick={() => void exportAs("json")} className="rounded-md px-3 py-1.5 text-left text-[13px] text-muted hover:bg-inset hover:text-fg">
                          {t.chat.exportJson}
                        </button>
                      </div>
                    )}
                  </div>
                )}
                <ConfirmButton
                  label={t.chat.delete}
                  confirmLabel={t.chat.deleteConfirm}
                  busy={busy === "delete"}
                  className={iconBtn}
                  confirmClassName="btn btn-secondary btn-sm shrink-0"
                  onConfirm={() => void run("delete", () => onDelete(session.id))}
                >
                  <Trash2 size={15} aria-hidden />
                </ConfirmButton>
              </>
            )}
          </>
        )}
      </div>
      <ErrorLine text={error} />
    </div>
  );
}
