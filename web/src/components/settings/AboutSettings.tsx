// 关于：版本信息 + 退出登录。
import { useQuery } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { api } from '../../lib/api'
import { clearCredentials } from '../../lib/auth'
import { useI18n } from '../../lib/i18n'
import { disconnectWs } from '../../lib/ws'
import { SetRow } from './controls'

export function AboutSettings() {
  const navigate = useNavigate()
  const { t } = useI18n()
  const { data: info, isLoading } = useQuery({ queryKey: ['info'], queryFn: api.info })

  function logout() {
    clearCredentials()
    disconnectWs()
    navigate({ to: '/login' })
  }

  return (
    <>
      <h2 className="set-title">{t('set.about')}</h2>
      <p className="set-desc">FluxDown Server — Downloads, Supercharged.</p>
      <div className="set-group">
        <SetRow title={t('set.about.version')}>
          <span className="set-value">{isLoading ? t('common.loading') : info ? `${info.app} ${info.version}` : '—'}</span>
        </SetRow>
      </div>
      <div className="set-group">
        <SetRow title={t('set.about.logout')} desc={t('set.about.logoutDesc')}>
          <button type="button" className="btn danger sm" onClick={logout}>
            {t('set.about.logout')}
          </button>
        </SetRow>
      </div>
      <p className="set-desc" style={{ marginTop: 14 }}>
        {t('set.about.tagline')}
      </p>
    </>
  )
}
