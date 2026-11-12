// 设置 › BT › 防吸血 › 客户端规则：本机名字规则（可编辑、调序）与云端下发规则（只读）。
// 排序：Web 用上移 / 下移按钮。

import { ArrowDown, ArrowUp, Ellipsis, Fingerprint } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useT } from '../../../../../i18n'
import { MAX_CLIENT_RULES, moveClientRule, newClientRule, nextClientRuleId, parseClientRules, removeClientRule } from '../../../../../lib/btGuardClientRules'
import type { ClientRule } from '../../../../../lib/btGuardClientRules'
import type { BtGuardClientRuleDto, BtGuardStateDto } from '../../../../../lib/rpc'
import { toastRpcError } from '../../../../../lib/rpcToast'
import { ActionMenu, Badge, Button, EmptyState, Icon, Tooltip, useIsMobile } from '../../../../../ui'
import type { MenuEntry } from '../../../../../ui'
import { SettingsCustomRow, SettingsSection, useDaemonBool, useDaemonValue } from '../../../kit'
import { ACTION_LABEL, ClientRuleDialog, FIELD_LABEL, OP_LABEL, WHEN_LABEL } from './ClientRuleDialog'

function RuleSummary({ rule }: { rule: BtGuardClientRuleDto }) {
  const t = useT()
  const minutes = rule.action === 'ban' ? ` · ${rule.banMinutes} ${t('timeUnitMinutes')}` : ''
  return (
    <div className="flex min-w-0 flex-1 basis-40 flex-col gap-0.5">
      <div className="flex min-w-0 items-center gap-2 text-sm">
        <Badge className="shrink-0">
          {t(FIELD_LABEL[rule.field])} · {t(OP_LABEL[rule.op])}
        </Badge>
        <span title={rule.value} className="min-w-0 truncate whitespace-pre font-mono text-foreground">
          {rule.value}
        </span>
      </div>
      <div className="flex min-w-0 items-center gap-2 text-xs text-muted-foreground">
        <span className="shrink-0">
          {t(WHEN_LABEL[rule.when])}
          {minutes}
        </span>
        {rule.reason ? (
          <span title={rule.reason} className="min-w-0 truncate">
            {rule.reason}
          </span>
        ) : null}
      </div>
    </div>
  )
}

function ActionBadge({ rule }: { rule: BtGuardClientRuleDto }) {
  const t = useT()
  return (
    <Badge tone={rule.action === 'allow' ? 'neutral' : 'accent'} className="shrink-0">
      {t(ACTION_LABEL[rule.action])}
    </Badge>
  )
}

function LocalRuleRow({
  rule,
  index,
  count,
  disabled,
  onEdit,
  onDelete,
  onMove,
}: {
  rule: ClientRule
  index: number
  count: number
  disabled: boolean
  onEdit: () => void
  onDelete: () => void
  onMove: (delta: -1 | 1) => void
}) {
  const t = useT()
  const mobile = useIsMobile()
  const first = index === 0
  const last = index === count - 1
  const entries: MenuEntry[] = [
    { type: 'item', key: 'up', label: t('moveUpAction'), onSelect: () => onMove(-1), disabled: disabled || first },
    { type: 'item', key: 'down', label: t('moveDownAction'), onSelect: () => onMove(1), disabled: disabled || last },
    { type: 'item', key: 'edit', label: t('btGuardClientRuleEdit'), onSelect: onEdit, disabled },
    { type: 'separator', key: 'sep' },
    { type: 'item', key: 'delete', label: t('btGuardClientRuleDelete'), onSelect: onDelete, destructive: true, disabled },
  ]
  return (
    <div className="flex flex-wrap items-center gap-x-2 gap-y-1 rounded-md px-2 py-1.5 hover:bg-row-hover">
      <Badge className="shrink-0 font-mono tabular-nums">{index + 1}</Badge>
      <RuleSummary rule={rule} />
      <div className="flex shrink-0 items-center gap-1">
        <ActionBadge rule={rule} />
        {mobile ? (
          <ActionMenu
            title={rule.value}
            entries={entries}
            trigger={
              <Button variant="ghost" iconOnly aria-label={t('moreActions')} title={t('moreActions')} className="text-muted-foreground hover:text-foreground">
                <Icon icon={Ellipsis} />
              </Button>
            }
          />
        ) : (
          <>
            <Tooltip content={t('moveUpAction')}>
              <Button variant="ghost" iconOnly className="text-muted-foreground hover:text-foreground" aria-label={t('moveUpAction')} disabled={disabled || first} onClick={() => onMove(-1)}>
                <Icon icon={ArrowUp} size="md" />
              </Button>
            </Tooltip>
            <Tooltip content={t('moveDownAction')}>
              <Button variant="ghost" iconOnly className="text-muted-foreground hover:text-foreground" aria-label={t('moveDownAction')} disabled={disabled || last} onClick={() => onMove(1)}>
                <Icon icon={ArrowDown} size="md" />
              </Button>
            </Tooltip>
            <Button variant="outline" disabled={disabled} onClick={onEdit}>
              {t('btGuardClientRuleEdit')}
            </Button>
            <Button variant="ghost" className="text-destructive" disabled={disabled} onClick={onDelete}>
              {t('btGuardClientRuleDelete')}
            </Button>
          </>
        )}
      </div>
    </div>
  )
}

interface EditorState {
  serial: number
  rule: ClientRule
  isNew: boolean
}

/** 云端规则列表每次展开的条数。 */
const CLOUD_RULES_PAGE = 10

/** `disabled`：总开关关闭；指纹规则为 off 时同样置灰。 */
export function ClientRulesSection({ data, disabled }: { data: BtGuardStateDto | null; disabled: boolean }) {
  const t = useT()
  const raw = useDaemonValue('bt_guard_client_rules')
  const fingerprint = useDaemonValue('bt_guard_rule_fingerprint')
  const cloudEnabled = useDaemonBool('bt_guard_cloud_enabled')
  const list = useMemo(() => parseClientRules(raw), [raw])
  const [editor, setEditor] = useState<EditorState | null>(null)
  const [cloudShown, setCloudShown] = useState(CLOUD_RULES_PAGE)
  const off = disabled || fingerprint === 'off'

  const write = async (operation: () => Promise<void>) => {
    try {
      await operation()
    } catch (failure) {
      toastRpcError(failure)
    }
  }
  const open = (rule: ClientRule, isNew: boolean) => setEditor((prev) => ({ serial: (prev?.serial ?? 0) + 1, rule, isNew }))

  const cloudRules = data?.cloudRules ?? []
  const cloudInactive = !cloudEnabled || data?.cloud.stale === true

  return (
    <>
      <SettingsSection title={t('btGuardClientRules')} subtitle={t('btGuardClientRulesDesc')}>
        <SettingsCustomRow className={off ? 'flex flex-col gap-1 px-2 py-2 opacity-60' : 'flex flex-col gap-1 px-2 py-2'}>
          {list.length === 0 ? (
            <EmptyState icon={Fingerprint} title={t('btGuardClientRulesEmpty')} />
          ) : (
            list.map((rule, index) => (
              <LocalRuleRow
                key={rule.id}
                rule={rule}
                index={index}
                count={list.length}
                disabled={off}
                onEdit={() => open(rule, false)}
                onDelete={() => void write(() => removeClientRule(rule.id))}
                onMove={(delta) => void write(() => moveClientRule(rule.id, delta))}
              />
            ))
          )}
          <div className="flex flex-wrap justify-end gap-2 px-2 pt-2 narrow:[&>*]:flex-1">
            <Button variant="primary" disabled={off || list.length >= MAX_CLIENT_RULES} onClick={() => open(newClientRule(nextClientRuleId(list)), true)}>
              {t('btGuardClientRuleAdd')}
            </Button>
          </div>
        </SettingsCustomRow>
      </SettingsSection>
      {cloudRules.length > 0 ? (
        <SettingsSection title={t('btGuardCloudRules')} subtitle={t('btGuardCloudRulesDesc')}>
          {cloudInactive ? (
            <SettingsCustomRow>
              <span className="text-xs text-warning">{t('btGuardCloudRulesInactive')}</span>
            </SettingsCustomRow>
          ) : null}
          <SettingsCustomRow className={off || cloudInactive ? 'flex flex-col gap-1 px-2 py-2 opacity-60' : 'flex flex-col gap-1 px-2 py-2'}>
            {cloudRules.slice(0, cloudShown).map((rule) => (
              <div key={rule.id} className="flex flex-wrap items-center gap-x-2 gap-y-1 rounded-md px-2 py-1.5">
                <RuleSummary rule={rule} />
                <ActionBadge rule={rule} />
              </div>
            ))}
            {cloudShown < cloudRules.length ? (
              <div className="flex justify-end px-2 pt-1">
                <Button variant="outline" onClick={() => setCloudShown((count) => count + CLOUD_RULES_PAGE)}>
                  {t('btGuardLoadMore', { shown: cloudShown, total: cloudRules.length })}
                </Button>
              </div>
            ) : null}
          </SettingsCustomRow>
        </SettingsSection>
      ) : null}
      {editor ? <ClientRuleDialog key={editor.serial} rule={editor.rule} isNew={editor.isNew} onClose={() => setEditor(null)} /> : null}
    </>
  )
}
