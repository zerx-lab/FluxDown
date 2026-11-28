// 助手对话里的「开通会员」卡片（GPUI `render_membership_offer`）：只给标题 / 说明 / 主按钮，不带套餐数据
// （价格以打开时实时目录为准）；点击后复用账户页现有的 `PlanDialog`（选套餐 → 扫码 → 成功），关闭即回到对话。

import { Crown } from 'lucide-react'
import { useState } from 'react'
import { useT } from '../../i18n'
import { useAgent } from '../../lib/rpc'
import { Button, Card, Icon } from '../../ui'
import { PlanDialog } from '../settings/sections/account/PlanDialog'

export function MembershipCard({ disabled }: { disabled: boolean }) {
  const t = useT()
  const session = useAgent((snapshot) => snapshot.session, null)
  const [open, setOpen] = useState(false)
  // 助手页本身要求已登录；会话缺失（刚退出登录）时没有可购买的主体，不显示卡片。
  if (session === null) return null
  return (
    <Card className="flex w-full min-w-0 flex-col gap-2 p-3">
      <div className="flex items-center gap-1.5 text-sm font-medium text-foreground">
        <Icon icon={Crown} size="sm" className="text-accent-text" />
        {t('assistantMembershipTitle')}
      </div>
      <div className="text-sm break-words text-muted-foreground">{t('assistantMembershipBody')}</div>
      <div className="flex flex-wrap gap-2">
        <Button variant="primary" disabled={disabled} onClick={() => setOpen(true)}>
          {t('assistantMembershipAction')}
        </Button>
      </div>
      {open && !disabled ? <PlanDialog session={session} onClose={() => setOpen(false)} /> : null}
    </Card>
  )
}
