import { useId, type ReactNode } from "react";
import { Bot, Lock } from "lucide-react";
import type { Lang } from "@/i18n/config";
import type { AssistantMessages } from "@/i18n/messages/assistant";
import { href } from "@/i18n/routing";
import type { AiConfig, AiSessionKind } from "@/lib/account/types";
import { docsPath } from "@/lib/site-nav";
import { Panel } from "@/components/account/dashboard/ui";

/** 居中的说明态（图标 + 标题 + 说明 + 动作）；`bare` 去掉外框，嵌在悬浮面板里用。 */
export function StatePanel({
  icon, title, desc, action, bare = false,
}: {
  icon: ReactNode;
  title: string;
  desc: string;
  action?: ReactNode;
  bare?: boolean;
}) {
  const body = (
    <>
      <span aria-hidden className="grid size-12 place-items-center rounded-full bg-inset text-muted">
        {icon}
      </span>
      <h2 className="mt-5 text-lg font-semibold tracking-tight">{title}</h2>
      <p className="mt-2 text-sm leading-relaxed text-muted">{desc}</p>
      {action && <div className="mt-6 flex flex-wrap justify-center gap-2">{action}</div>}
    </>
  );
  if (bare) return <div className="flex flex-1 flex-col items-center justify-center px-6 py-10 text-center">{body}</div>;
  return <Panel className="mx-auto flex max-w-[520px] flex-col items-center px-6 py-12 text-center sm:px-10">{body}</Panel>;
}

/** 未开放 / 套餐不含：说明 + 去文档 / 去定价。 */
export function UnavailableState({ lang, t, config, bare }: { lang: Lang; t: AssistantMessages; config: AiConfig; bare?: boolean }) {
  if (config.reason === "plan_disabled") {
    return (
      <StatePanel
        bare={bare}
        icon={<Lock size={22} />}
        title={t.states.planTitle}
        desc={t.states.planDesc}
        action={<a href={href("/pricing", lang)} className="btn btn-primary">{t.states.planCta}</a>}
      />
    );
  }
  return (
    <StatePanel
      bare={bare}
      icon={<Bot size={22} />}
      title={t.states.unavailableTitle}
      desc={t.states.unavailableDesc}
      action={<a href={href(docsPath(lang), lang)} className="btn btn-secondary">{t.states.unavailableCta}</a>}
    />
  );
}

/** 新对话的欢迎区：对话类型与建议问题；`compact` 用于悬浮面板（更小的留白与标题）。 */
export function Greeting({
  t, kind, onKind, onPick, compact = false,
}: {
  t: AssistantMessages;
  kind: AiSessionKind;
  onKind: (kind: AiSessionKind) => void;
  onPick: (text: string) => void;
  compact?: boolean;
}) {
  const name = useId();
  return (
    <div className={`min-h-0 flex-1 overflow-y-auto ${compact ? "px-4 py-6" : "px-4 py-10 sm:px-6"}`}>
      <div className={`mx-auto flex max-w-[560px] flex-col ${compact ? "items-start text-left" : "items-center text-center"}`}>
        <span aria-hidden className={`grid place-items-center rounded-full bg-accent-soft text-accent-ink ${compact ? "size-10" : "size-12"}`}>
          <Bot size={compact ? 19 : 22} />
        </span>
        <h2 className={`mt-4 font-semibold tracking-tight ${compact ? "text-[17px]" : "text-xl"}`}>{t.chat.greetingTitle}</h2>
        <p className="mt-1.5 text-[13.5px] leading-relaxed text-muted">{t.chat.greetingDesc}</p>
        <fieldset className="mt-5 inline-flex rounded-lg border border-line p-0.5">
          <legend className="sr-only">{t.chat.kindLabel}</legend>
          {(["ask", "feedback"] as const).map((value) => (
            <label key={value} className={`cursor-pointer rounded-md px-3 py-1.5 text-[13px] ${kind === value ? "bg-inset font-medium text-fg" : "text-muted"}`}>
              <input type="radio" name={name} value={value} checked={kind === value} onChange={() => onKind(value)} className="sr-only" />
              {value === "ask" ? t.chat.kindAsk : t.chat.kindFeedback}
            </label>
          ))}
        </fieldset>
        {kind === "ask" && (
          <ul className="mt-5 w-full space-y-2">
            {t.chat.suggestions.map((text) => (
              <li key={text}>
                <button type="button" onClick={() => onPick(text)} className="w-full rounded-lg border border-line px-4 py-2.5 text-left text-[13.5px] text-muted transition-colors hover:border-line-strong hover:text-fg">
                  {text}
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
