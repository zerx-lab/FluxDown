// 助手起草的问题单卡片（GPUI `render_issue` / `render_issue_status`）：草稿可提交 / 编辑 / 放弃，处理后显示状态。
// 标题、正文、审核备注与错误都是用户 / 模型 / 审核人数据：保留换行、长词与长链接折行显示。

import { CircleAlert, CircleCheck, Clock, ExternalLink, X } from 'lucide-react'
import { useState } from 'react'
import { useT } from '../../i18n'
import type { AssistantIssueDto } from '../../lib/rpc'
import { Badge, Button, Card, FieldLabel, Icon, Input, Textarea } from '../../ui'
import type { AssistantController, ViewState } from './controller'
import { ResultLabel } from './Steps'

function IssueEditor({ issue, busy, stale, controller }: { issue: AssistantIssueDto; busy: boolean; stale: boolean; controller: AssistantController }) {
  const t = useT()
  const [title, setTitle] = useState(issue.title)
  const [body, setBody] = useState(issue.body)
  const trimmedTitle = title.trim()
  const trimmedBody = body.trim()
  return (
    <div className="flex min-w-0 flex-col gap-2">
      <FieldLabel>{t('assistantIssueTitleLabel')}</FieldLabel>
      <Input value={title} onChange={(event) => setTitle(event.target.value)} />
      <FieldLabel>{t('assistantIssueBodyLabel')}</FieldLabel>
      <Textarea rows={6} value={body} onChange={(event) => setBody(event.target.value)} />
      <div className="flex flex-wrap gap-2">
        <Button
          variant="primary"
          loading={busy}
          disabled={stale || busy || trimmedTitle === '' || trimmedBody === ''}
          onClick={() => controller.confirmIssue(issue.id, { title: trimmedTitle, body: trimmedBody })}
        >
          {t('assistantIssueSubmit')}
        </Button>
        <Button variant="outline" disabled={busy} onClick={() => controller.cancelIssueEdit()}>
          {t('assistantCancel')}
        </Button>
      </div>
    </div>
  )
}

const STATUS: Record<string, { icon: typeof Clock; key: string; tone: string }> = {
  pending_review: { icon: Clock, key: 'assistantIssuePending', tone: 'text-warning' },
  submitted: { icon: CircleCheck, key: 'assistantIssueSubmitted', tone: 'text-success' },
  rejected: { icon: X, key: 'assistantIssueRejected', tone: 'text-destructive' },
  failed: { icon: CircleAlert, key: 'assistantIssueFailed', tone: 'text-destructive' },
}
const DISCARDED = { icon: X, key: 'assistantIssueDiscarded', tone: 'text-muted-foreground' }

function IssueStatus({ issue, busy, stale, controller }: { issue: AssistantIssueDto; busy: boolean; stale: boolean; controller: AssistantController }) {
  const t = useT()
  if (issue.status === 'draft') {
    return (
      <div className="flex flex-wrap gap-2">
        <Button variant="primary" loading={busy} disabled={stale || busy} onClick={() => controller.confirmIssue(issue.id)}>
          {t('assistantIssueSubmit')}
        </Button>
        <Button variant="outline" disabled={stale || busy} onClick={() => controller.editIssue(issue.id)}>
          {t('assistantIssueEdit')}
        </Button>
        <Button variant="ghost" disabled={stale || busy} onClick={() => controller.discardIssue(issue.id)}>
          {t('assistantIssueDiscard')}
        </Button>
      </div>
    )
  }
  const { icon, key, tone } = STATUS[issue.status] ?? DISCARDED
  const note = issue.reviewNote?.trim() ? t('assistantIssueReviewNote', { note: issue.reviewNote }) : null
  const failure = issue.status === 'failed' && issue.error?.trim() ? issue.error : null
  const detail = note ?? failure
  // 只接受 https 链接（与 GPUI 一致）。
  const url = issue.githubUrl?.startsWith('https://') ? issue.githubUrl : null
  return (
    <div className="flex min-w-0 flex-col gap-1.5">
      <ResultLabel icon={icon} text={t(key)} tone={tone} />
      {detail !== null ? <div className="text-xs break-words whitespace-pre-wrap text-text-tertiary">{detail}</div> : null}
      {url !== null ? (
        <a href={url} target="_blank" rel="noopener noreferrer" className="flex w-fit items-center gap-1 text-xs text-accent-text coarse:min-h-touch">
          <Icon icon={ExternalLink} size="sm" />
          {t('assistantIssueOpenGithub', { number: issue.githubNumber ?? '' })}
        </a>
      ) : null}
      {issue.status === 'failed' ? (
        <Button variant="outline" className="w-fit" loading={busy} disabled={stale || busy} onClick={() => controller.confirmIssue(issue.id)}>
          {t('assistantRetry')}
        </Button>
      ) : null}
    </div>
  )
}

export function IssueCard({ issue, view, controller }: { issue: AssistantIssueDto; view: ViewState; controller: AssistantController }) {
  const t = useT()
  const busy = view.issueBusy.has(issue.id)
  const stale = view.env.stale
  const editing = view.issueEditing === issue.id && issue.status === 'draft'
  const labels = issue.labels ?? []
  return (
    <Card className="flex w-full min-w-0 flex-col gap-2 p-3">
      <div className="flex flex-wrap items-center gap-1.5">
        <Badge>{t(issue.kind === 'feature' ? 'assistantIssueFeature' : 'assistantIssueBug')}</Badge>
        {issue.status === 'draft' ? <span className="text-xs text-text-tertiary">{t('assistantIssueDraftHint')}</span> : null}
      </div>
      {editing ? (
        <IssueEditor key={issue.id} issue={issue} busy={busy} stale={stale} controller={controller} />
      ) : (
        <>
          <div className="text-sm font-medium break-words whitespace-pre-wrap text-foreground">{issue.title}</div>
          <div className="text-sm break-words whitespace-pre-wrap text-muted-foreground">{issue.body}</div>
          {labels.length > 0 ? (
            <div className="flex flex-wrap gap-1.5">
              {labels.map((label) => (
                <Badge key={label}>{label}</Badge>
              ))}
            </div>
          ) : null}
          <IssueStatus issue={issue} busy={busy} stale={stale} controller={controller} />
        </>
      )}
    </Card>
  )
}
