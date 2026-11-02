import { useT } from '../../../../../i18n'
import { EVIDENCE_FACT_KEYS, evidenceFacts } from '../../../../../lib/btGuard'
import type { JsonValue } from '../../../../../lib/rpc'
import { formatBytes } from '../../../../downloads/model/task'

/** 证据 JSON（对象或 JSON 文本）→ 「字段：值」列表；没有可展示的字段时不渲染。 */
export function EvidenceFacts({ evidence }: { evidence: JsonValue | undefined }) {
  const t = useT()
  const facts = evidenceFacts(evidence, formatBytes)
  if (facts.length === 0) return null
  return (
    <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-0.5 text-xs">
      {facts.map((fact) => (
        <div key={fact.id} className="contents">
          <dt className="text-text-tertiary">{t(EVIDENCE_FACT_KEYS[fact.id])}</dt>
          <dd className="tabular min-w-0 break-words whitespace-pre-wrap text-foreground">{fact.value}</dd>
        </div>
      ))}
    </dl>
  )
}
