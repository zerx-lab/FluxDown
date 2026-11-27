import { useId, useState } from "react";
import { ExternalLink, FileText } from "lucide-react";
import type { Lang } from "@/i18n/config";
import type { AssistantMessages } from "@/i18n/messages/assistant";
import { accountApi, accountErrorText } from "@/lib/account/client";
import type { AiIssue } from "@/lib/account/types";
import { lookup } from "@/components/account/dashboard/format";
import type { Guard } from "@/components/account/dashboard/resource";
import { ErrorLine, Spinner } from "@/components/account/dashboard/ui";
import Markdown from "./Markdown";

interface Props {
  issue: AiIssue;
  lang: Lang;
  t: AssistantMessages;
  guard: Guard;
  onChange: (issue: AiIssue) => void;
}

/** 模型起草的问题单：草稿可编辑后提交或放弃；其余状态只读并给出 GitHub 链接 / 审核备注。 */
export default function IssueCard({ issue, lang, t, guard, onChange }: Props) {
  const id = useId();
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState(issue.title);
  const [body, setBody] = useState(issue.body);
  const [busy, setBusy] = useState<"submit" | "discard" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const draft = issue.status === "draft";
  const kind = issue.kind === "bug" || issue.kind === "feature" ? t.issue.kinds[issue.kind] : t.issue.kinds.other;

  const act = async (action: "submit" | "discard") => {
    setBusy(action);
    setError(null);
    const edit = editing ? { title: title.trim(), body } : {};
    const res =
      action === "submit"
        ? await accountApi<AiIssue>(`ai/issues/${encodeURIComponent(issue.id)}/confirm`, { body: edit })
        : await accountApi<AiIssue>(`ai/issues/${encodeURIComponent(issue.id)}/discard`, { method: "POST" });
    setBusy(null);
    if (res.ok) {
      setEditing(false);
      onChange(res.data);
    } else if (!guard(res)) {
      setError(accountErrorText(lang, res.code));
    }
  };

  /** 取消编辑时丢弃改动，回到草稿原文。 */
  const toggleEdit = () => {
    if (editing) {
      setTitle(issue.title);
      setBody(issue.body);
    }
    setEditing(!editing);
  };

  return (
    <section aria-labelledby={`${id}-title`} className="border border-line bg-elev">
      <header className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-line px-4 py-3">
        <FileText size={15} aria-hidden className="text-muted" />
        <span className="text-[12px] font-medium uppercase tracking-wide text-subtle">{kind}</span>
        <span className="ml-auto text-[12.5px] text-muted">{lookup(t.issue.status, issue.status)}</span>
      </header>
      <div className="px-4 py-4">
        {editing ? (
          <div className="space-y-4">
            <div>
              <label htmlFor={`${id}-title`} className="mb-2 block text-[13px] font-medium">
                {t.issue.titleLabel}
              </label>
              <input id={`${id}-title`} className="field" value={title} maxLength={136} onChange={(e) => setTitle(e.target.value)} />
            </div>
            <div>
              <label htmlFor={`${id}-body`} className="mb-2 block text-[13px] font-medium">
                {t.issue.bodyLabel}
              </label>
              <textarea
                id={`${id}-body`}
                className="field min-h-48 font-mono text-[13px] leading-relaxed"
                value={body}
                maxLength={20_000}
                onChange={(e) => setBody(e.target.value)}
              />
            </div>
          </div>
        ) : (
          <>
            <h3 id={`${id}-title`} className="text-[15px] font-semibold tracking-tight [overflow-wrap:anywhere]">
              {issue.title}
            </h3>
            {draft && <p className="mt-1 text-[13px] text-muted">{t.issue.draftDesc}</p>}
            {draft && (
              <div className="mt-3 max-h-72 overflow-y-auto border-t border-line pt-3">
                <Markdown text={issue.body} labels={t.chat} />
              </div>
            )}
          </>
        )}
        {issue.reviewNote && (
          <p className="mt-3 text-[13px] text-muted">
            <span className="font-medium text-fg">{t.issue.reviewNote}:</span> {issue.reviewNote}
          </p>
        )}
        {issue.githubUrl && issue.githubNumber !== null && (
          <a href={issue.githubUrl} target="_blank" rel="noopener noreferrer" className="mt-3 inline-flex items-center gap-1.5 text-[13px] text-accent-ink underline underline-offset-2">
            {t.issue.openGithub(issue.githubNumber)}
            <ExternalLink size={13} aria-hidden />
          </a>
        )}
        {draft && (
          <div className="mt-4 flex flex-wrap items-center gap-2">
            <button type="button" onClick={() => void act("submit")} disabled={busy !== null || (editing && title.trim().length < 4)} className="btn btn-primary btn-sm disabled:opacity-60">
              {busy === "submit" && <Spinner size={13} />}
              {t.issue.submit}
            </button>
            <button type="button" onClick={toggleEdit} disabled={busy !== null} className="btn btn-secondary btn-sm disabled:opacity-60">
              {editing ? t.issue.cancelEdit : t.issue.edit}
            </button>
            <button type="button" onClick={() => void act("discard")} disabled={busy !== null} className="btn btn-secondary btn-sm ml-auto disabled:opacity-60">
              {busy === "discard" && <Spinner size={13} />}
              {t.issue.discard}
            </button>
          </div>
        )}
        <ErrorLine text={error} />
      </div>
    </section>
  );
}
