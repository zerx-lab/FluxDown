import { useT } from '../../../../i18n'
import { optionalText, SettingsRow, setDaemon, useDaemonValue } from '../../kit'
import { UserAgentPresetControl } from '../../../userAgent/UserAgentPresetControl'

const UA_KEY = 'global_user_agent'

export function UserAgentRow() {
  const t = useT()
  const current = useDaemonValue(UA_KEY)
  return <SettingsRow title={t('userAgent')} description={optionalText(t, 'userAgentDesc')}>
    <UserAgentPresetControl value={current} onChange={(value) => setDaemon(UA_KEY, value)} allowSave={false} defaultLabelKey="userAgentPresetDefault" inputPlaceholderKey="userAgentPlaceholder" />
  </SettingsRow>
}
