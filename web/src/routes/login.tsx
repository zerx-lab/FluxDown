// #screen-login —— 服务器地址 + 令牌登录卡片，对齐 design/web/index.html。
import { useNavigate } from '@tanstack/react-router'
import { type FormEvent, useState } from 'react'
import { api, ApiError } from '../lib/api'
import { saveCredentials } from '../lib/auth'
import { translateBackendMessage, useI18n } from '../lib/i18n'

export function LoginScreen() {
  const { t } = useI18n()
  const navigate = useNavigate()
  const [base, setBase] = useState(() => window.location.origin)
  const [token, setToken] = useState('')
  const [remember, setRemember] = useState(true)
  const [error, setError] = useState('')
  const [pending, setPending] = useState(false)

  async function handleSubmit(e: FormEvent) {
    e.preventDefault()
    setError('')
    setPending(true)
    try {
      const trimmed = base.trim()
      const effectiveBase = trimmed === window.location.origin ? '' : trimmed
      await api.probe(effectiveBase, token)
      saveCredentials(effectiveBase, token, remember)
      navigate({ to: '/' })
    } catch (err) {
      setError(err instanceof ApiError ? translateBackendMessage(err.message) : t('login.connectFailed'))
    } finally {
      setPending(false)
    }
  }

  return (
    <section className="wscreen active" id="screen-login">
      <div className="login-bg" />
      <div className="login-card">
        <span className="login-logo">
          <svg viewBox="30 30 452 452" role="img" xmlns="http://www.w3.org/2000/svg">
            <rect x="56" y="56" width="400" height="400" rx="88" fill="#3B82F6" />
            <path
              d="M 226 131 Q 226 119 238 119 L 274 119 Q 286 119 286 131 L 286 296 L 331 251 Q 340 242 349 251 L 363 265 Q 372 274 363 283 L 265 381 Q 256 390 247 381 L 149 283 Q 140 274 149 265 L 163 251 Q 172 242 181 251 L 226 296 Z"
              fill="#F2F4F8"
            />
          </svg>
        </span>
        <h2>{t('login.title')}</h2>
        <p className="login-sub">{t('login.subtitle')}</p>
        <form className="contents" onSubmit={handleSubmit}>
          <label className="field-label" htmlFor="login-base">
            {t('login.serverAddress')}
          </label>
          <input
            id="login-base"
            className="text-input"
            type="text"
            spellCheck={false}
            required
            value={base}
            onChange={(e) => setBase(e.target.value)}
          />
          <label className="field-label" htmlFor="login-token">
            {t('login.token')}
          </label>
          <input
            id="login-token"
            className="text-input"
            type="password"
            spellCheck={false}
            required
            value={token}
            onChange={(e) => setToken(e.target.value)}
          />
          <label className="remember">
            <input type="checkbox" checked={remember} onChange={(e) => setRemember(e.target.checked)} />
            <i />
            {t('login.remember')}
          </label>
          {error ? <p className="mt-[-6px] mb-3 text-[12px] text-danger">{error}</p> : null}
          <button className="btn primary block" type="submit" disabled={pending}>
            {pending ? t('login.connecting') : t('login.connect')}
          </button>
        </form>
        <p className="login-hint">{t('login.hint')}</p>
      </div>
      <div className="login-feats">
        <span>
          <b>{t('login.featEngine')}</b>{t('login.featEngineDesc')}
        </span>
        <span>
          <b>{t('login.featRealtime')}</b>{t('login.featRealtimeDesc')}
        </span>
        <span>
          <b>{t('login.featPrivacy')}</b>{t('login.featPrivacyDesc')}
        </span>
      </div>
    </section>
  )
}
