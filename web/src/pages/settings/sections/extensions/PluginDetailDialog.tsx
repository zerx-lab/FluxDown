// 插件详情对话框：已安装插件与市场条目共用（manifest 级信息 + 权限 + 使用须知）。

import type { ReactNode } from 'react'
import type { PluginArchiveRulesDto } from '../../../../lib/rpc'
import { Badge, Button, ConfirmFooter, Dialog, toast } from '../../../../ui'
import { useT } from '../../../../i18n'
import { ExtLink } from './common'
import type { PluginDetail } from './detail'
import { permissionKeys, safeHttpUrl, yankedLabelKey } from './logic'

function InfoRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex w-full items-baseline gap-3">
      <div className="w-24 shrink-0 text-xs text-muted-foreground">{label}</div>
      <div className="min-w-0 flex-1 break-words text-sm text-foreground">{children}</div>
    </div>
  )
}

function SectionTitle({ children }: { children: ReactNode }) {
  return <div className="pt-2 text-caption font-medium text-text-tertiary">{children}</div>
}

export function PluginDetailDialog({
  detail,
  onClose,
  onAudit,
  archiveRules,
  archiveDisabled = true,
  archiveBusy = false,
  archiveMessage = null,
  onArchiveRefresh,
}: {
  detail: PluginDetail | null
  onClose: () => void
  /** 已安装插件才提供：打开该插件的审计记录。 */
  onAudit?: (identity: string) => void
  archiveRules?: PluginArchiveRulesDto | null
  archiveDisabled?: boolean
  archiveBusy?: boolean
  archiveMessage?: 'success' | 'error' | null
  onArchiveRefresh?: () => void
}) {
  const t = useT()
  const shown = detail
  const yankedKey = shown ? yankedLabelKey(shown.yanked) : null
  return (
    <Dialog
      open={detail !== null}
      onOpenChange={(open) => !open && onClose()}
      title={shown?.name ?? ''}
      footer={
        <ConfirmFooter
          cancelLabel={null}
          okLabel={t('close')}
          onCancel={onClose}
          onOk={onClose}
          extra={
            shown && onAudit ? (
              <Button variant="outline" onClick={() => onAudit(shown.identity)}>
                {t('pluginAuditOpenForPlugin')}
              </Button>
            ) : undefined
          }
        />
      }
    >
      {shown ? (
        <div className="flex w-full flex-col gap-2 pb-2">
          <div className="flex flex-wrap items-center gap-2">
            <span className="tabular text-xs text-muted-foreground">v{shown.version}</span>
            {yankedKey ? <Badge tone="destructive">{t(yankedKey)}</Badge> : null}
            {shown.tags.map((tag) => (
              <Badge key={tag}>{tag}</Badge>
            ))}
          </div>
          <InfoRow label={t('pluginDetailIdentity')}>{shown.identity}</InfoRow>
          {shown.author ? <InfoRow label={t('pluginDetailAuthor')}>{shown.author}</InfoRow> : null}
          {shown.homepage ? (
            <InfoRow label={t('pluginDetailHomepage')}>
              {safeHttpUrl(shown.homepage) ? <ExtLink href={shown.homepage} className="text-sm" /> : shown.homepage}
            </InfoRow>
          ) : null}
          {shown.publishTime ? <InfoRow label={t('pluginDetailPublishTime')}>{shown.publishTime}</InfoRow> : null}
          {shown.minAppVersion ? <InfoRow label={t('pluginDetailMinAppVersion')}>{shown.minAppVersion}</InfoRow> : null}
          {shown.settingsCount > 0 ? (
            <InfoRow label={t('pluginDetailSettings')}>{t('pluginDetailSettingsCount', { count: shown.settingsCount })}</InfoRow>
          ) : null}
          {shown.description ? (
            <>
              <SectionTitle>{t('pluginDetailDescription')}</SectionTitle>
              <div className="whitespace-pre-wrap break-words text-sm text-foreground">{shown.description}</div>
            </>
          ) : null}
          {shown.permissions.length > 0 ? (
            <>
              <SectionTitle>{t('pluginDetailPermissions')}</SectionTitle>
              {shown.permissions.map((permission) => {
                const keys = permissionKeys(permission)
                return (
                  <div key={permission} className="flex w-full flex-col gap-0.5">
                    <div className="text-sm font-medium text-foreground">{keys ? t(keys.name) : permission}</div>
                    <div className="text-xs text-muted-foreground">{t(keys ? keys.desc : 'pluginPermUnknownDesc')}</div>
                  </div>
                )
              })}
            </>
          ) : null}
          {shown.pageScripts.length > 0 ? (
            <>
              <SectionTitle>{t('pluginConsentPageScripts')}</SectionTitle>
              {shown.pageScripts.map((script) => (
                <div key={script.id} className="flex w-full flex-col gap-0.5">
                  <div className="text-sm font-medium text-foreground">{script.title}</div>
                  <div className="break-all text-xs text-muted-foreground">{script.matches.join(', ')}</div>
                </div>
              ))}
            </>
          ) : null}
          {archiveRules || archiveBusy || archiveMessage ? (
            <>
              <SectionTitle>{t('pluginArchiveRulesTitle')}</SectionTitle>
              {archiveRules ? (
                <>
                  <InfoRow label={t('pluginArchiveRulesSequence')}>{archiveRules.sequence}</InfoRow>
                  <InfoRow label={t('pluginArchiveRulesCount')}>{archiveRules.ruleCount}</InfoRow>
                  {archiveRules.lastCheckedAt !== null ? <InfoRow label={t('pluginArchiveRulesLastChecked')}>{new Date(archiveRules.lastCheckedAt).toLocaleString()}</InfoRow> : null}
                  {archiveRules.lastUpdatedAt !== null ? <InfoRow label={t('pluginArchiveRulesLastUpdated')}>{new Date(archiveRules.lastUpdatedAt).toLocaleString()}</InfoRow> : null}
                  {archiveRules.lastError ? <InfoRow label={t('pluginArchiveRulesError')}>{archiveRules.lastError}</InfoRow> : null}
                  <InfoRow label={t('pluginArchiveRulesUpdateUrl')}>{archiveRules.updateUrl ?? t('pluginArchiveRulesNone')}</InfoRow>
                  <InfoRow label={t('pluginArchiveRulesAutomatic')}>{t(archiveRules.automaticUpdates ? 'pluginArchiveRulesEnabled' : 'pluginArchiveRulesDisabled')}</InfoRow>
                  <InfoRow label={t('pluginArchiveRulesPublicKey')}>
                    <div className="flex min-w-0 items-start gap-2">
                      <code className="min-w-0 break-all">{archiveRules.publicKey}</code>
                      <Button variant="outline" onClick={() => void navigator.clipboard.writeText(archiveRules.publicKey).then(() => toast.success(t('pluginArchiveRulesCopied'))).catch(() => toast.error(t('pluginArchiveRulesCopyFailed')))}>{t('pluginArchiveRulesCopy')}</Button>
                    </div>
                  </InfoRow>
                </>
              ) : archiveBusy ? <div className="text-xs text-muted-foreground">{t('pluginArchiveRulesRefreshing')}</div> : null}
              <div className="flex items-center gap-2 pt-1">
                <Button variant="outline" disabled={archiveDisabled || archiveBusy} onClick={onArchiveRefresh}>{archiveBusy ? t('pluginArchiveRulesRefreshing') : t('pluginArchiveRulesRefresh')}</Button>
                {archiveMessage === 'success' ? <span className="text-xs text-muted-foreground">{t('pluginArchiveRulesRefreshSuccess')}</span> : null}
                {archiveMessage === 'error' ? <span className="text-xs text-destructive">{t('pluginArchiveRulesRefreshError')}</span> : null}
              </div>
            </>
          ) : null}
          <SectionTitle>{t('pluginDetailUsage')}</SectionTitle>
          <div className="text-xs text-muted-foreground">{t('pluginDetailUsageBody')}</div>
        </div>
      ) : null}
    </Dialog>
  )
}
