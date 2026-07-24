// 云账户：登录/注册/设备验证/设备管理，与本地下载器登录态完全独立（面板本身作为
// 一台 web 设备接入 FluxCloud，见 lib/cloud/*）。未登录时纯介绍 + 登录/注册卡片，不影响
// 面板本地功能；已登录展示资料卡 + 设备列表 + 云服务器地址。

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, Check, ChevronRight, Cloud, Copy, Monitor, Pencil, Search, Smartphone, Trash2, X } from 'lucide-react'
import { type FormEvent, useEffect, useMemo, useRef, useState } from 'react'
import { cn } from '../../lib/cn'
import { cloudApi, getCloudBaseUrl, isCloudBaseUrlCustom, resetCloudBaseUrl, setCloudBaseUrl } from '../../lib/cloud/client'
import { suggest } from '../../lib/cloud/nickname'
import { applyCloudSession, clearCloudSession, cloudDeviceId, getCloudRefreshToken, setShowDeviceSync, useCloudSession, useShowDeviceSync } from '../../lib/cloud/session'
import { CloudApiError, type CloudDevice } from '../../lib/cloud/types'
import { confirmDialog } from '../../lib/confirm'
import { fmtIsoTime, fmtRelativeTime } from '../../lib/format'
import type { I18nKey } from '../../lib/i18n'
import { useI18n } from '../../lib/i18n'
import { DirectDevicesSection } from './DirectDevicesSection'
import { SetRow, SetSwitch, TextInput } from './controls'

const DEVICES_QUERY_KEY = ['cloud', 'devices']

// ---------------------------------------------------------------------------
// 错误码 → 本地化文案；未识别的 code 回退服务端原文 message。
// ---------------------------------------------------------------------------

const CLOUD_ERROR_KEYS: Record<string, I18nKey> = {
  invalid_credentials: 'cloud.err.invalidCredentials',
  invalid_code: 'cloud.err.invalidCode',
  rate_limited: 'cloud.err.rateLimited',
  email_taken: 'cloud.err.emailTaken',
  account_disabled: 'cloud.err.accountDisabled',
  registration_closed: 'cloud.err.registrationClosed',
  registration_incomplete: 'cloud.err.registrationIncomplete',
  network_error: 'cloud.err.network',
}

function cloudErrorText(t: (key: I18nKey, params?: Record<string, string | number>) => string, err: unknown): string {
  if (err instanceof CloudApiError) {
    const key = CLOUD_ERROR_KEYS[err.code]
    if (key) return t(key)
    if (err.code === 'validation') return err.message || t('cloud.err.validation')
    return err.message || t('cloud.err.unknown')
  }
  return t('cloud.err.network')
}

// ---------------------------------------------------------------------------
// 平台标签：契约已知取值 windows|macos|linux|android|ios|web，未知值原样展示。
// ---------------------------------------------------------------------------

const PLATFORM_LABEL_KEYS: Record<string, I18nKey> = {
  windows: 'cloud.platform.windows',
  macos: 'cloud.platform.macos',
  linux: 'cloud.platform.linux',
  android: 'cloud.platform.android',
  ios: 'cloud.platform.ios',
  web: 'cloud.platform.web',
}

function platformLabel(t: (key: I18nKey) => string, platform?: string): string {
  if (!platform) return '—'
  const key = PLATFORM_LABEL_KEYS[platform]
  return key ? t(key) : platform
}

export function CloudAccountSettings() {
  const { t } = useI18n()
  const session = useCloudSession()
  return (
    <div className="max-w-[640px]">
      <h2 className="set-title">{t('set.account')}</h2>
      <p className="set-desc">{t('set.account.desc')}</p>
      {session.status === 'authenticated' && session.user ? (
        <LoggedInPanel user={session.user} />
      ) : (
        <AuthPanel />
      )}
      <DirectDevicesSection />
      <CloudServerAddressGroup />
    </div>
  )
}

// ---------------------------------------------------------------------------
// 未登录：介绍卡片 + 登录/注册卡片（卡片内直接呈现表单，无需二次“返回选择”）
// ---------------------------------------------------------------------------

type AuthView = 'intro' | 'login' | 'register'

function AuthPanel() {
  const { t } = useI18n()
  const [view, setView] = useState<AuthView>('intro')
  const [prefillEmail, setPrefillEmail] = useState('')
  const [prefillPassword, setPrefillPassword] = useState('')
  const [incomplete, setIncomplete] = useState(false)

  function goRegister(email: string, password = '', fromIncomplete = false) {
    setPrefillEmail(email)
    setPrefillPassword(password)
    setIncomplete(fromIncomplete)
    setView('register')
  }

  if (view === 'login') {
    return (
      <LoginCard
        onSwitchToRegister={(email) => goRegister(email)}
        onRegistrationIncomplete={(email, password) => goRegister(email, password, true)}
      />
    )
  }
  if (view === 'register') {
    return (
      <RegisterCard
        initialEmail={prefillEmail}
        initialPassword={prefillPassword}
        incomplete={incomplete}
        onSwitchToLogin={() => setView('login')}
      />
    )
  }
  return (
    <div className="cloud-card">
      <div className="cloud-card-head">
        <span className="cloud-card-icon">
          <Cloud size={20} />
        </span>
        <h3>{t('cloud.introTitle')}</h3>
        <p>{t('cloud.introDesc')}</p>
      </div>
      <div className="flex items-center justify-center gap-2">
        <button type="button" className="btn primary" onClick={() => setView('login')}>
          {t('cloud.login')}
        </button>
        <button type="button" className="btn ghost" onClick={() => setView('register')}>
          {t('cloud.register')}
        </button>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// 验证码输入面板（登录新设备验证 / 验证码登录 / 注册验证码 共用）
// 说明性文字由外层卡片头（cloud-card-head）承载，这里只负责输入 + 状态 + 操作。
// ---------------------------------------------------------------------------

function VerificationCodeStep({
  code,
  onCodeChange,
  ttlSeconds,
  sentAt,
  busy,
  error,
  onResend,
  onSubmit,
  onBack,
}: {
  code: string
  onCodeChange: (v: string) => void
  ttlSeconds: number
  sentAt: number
  busy: boolean
  error: string
  onResend: () => void
  onSubmit: () => void
  onBack: () => void
}) {
  const { t } = useI18n()
  const [ttlLeft, setTtlLeft] = useState(ttlSeconds)
  const [resendLeft, setResendLeft] = useState(60)

  // sentAt 每次发码/重发都会变化（即便 ttlSeconds 数值相同），据此重置倒计时。
  useEffect(() => {
    setTtlLeft(ttlSeconds)
    setResendLeft(60)
  }, [ttlSeconds, sentAt])

  useEffect(() => {
    const timer = window.setInterval(() => {
      setTtlLeft((v) => Math.max(0, v - 1))
      setResendLeft((v) => Math.max(0, v - 1))
    }, 1000)
    return () => window.clearInterval(timer)
  }, [])

  return (
    <div className="flex flex-col">
      {/* 验证码步骤才出现的返回入口：轻量文字链接，非按钮行。 */}
      <button type="button" className="link-btn mb-4 inline-flex w-fit items-center gap-1 text-[12px]" disabled={busy} onClick={onBack}>
        <ArrowLeft size={12} /> {t('common.back')}
      </button>
      <label className="field-label" style={{ marginTop: 0 }}>
        {t('cloud.codePlaceholder')}
      </label>
      <input
        className="text-input"
        inputMode="numeric"
        autoFocus
        placeholder={t('cloud.codePlaceholder')}
        value={code}
        disabled={busy}
        onChange={(e) => onCodeChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') onSubmit()
        }}
      />
      <div className="mt-2 flex items-center justify-between">
        <span className="text-[11px] text-text3">{ttlLeft > 0 ? t('cloud.codeExpireIn', { s: ttlLeft }) : t('cloud.codeExpired')}</span>
        <button type="button" className="link-btn text-[12px]" disabled={resendLeft > 0 || busy} onClick={onResend}>
          {resendLeft > 0 ? t('cloud.resendCodeIn', { s: resendLeft }) : t('cloud.resendCode')}
        </button>
      </div>
      {error ? <p className="mt-2 text-[12px] text-danger">{error}</p> : null}
      <button type="button" className="btn primary block mt-5" disabled={busy || !code.trim()} onClick={onSubmit}>
        {busy ? t('common.loading') : t('cloud.verifySubmit')}
      </button>
    </div>
  )
}

// ---------------------------------------------------------------------------
// 登录卡片：验证码 / 密码 两 Tab（等宽分段控件）；密码登录命中新设备转入验证码步骤，
// 命中 registration_incomplete 转去注册（预填邮箱密码）。表单第一步无返回按钮，
// 仅验证码步骤需要返回（见 VerificationCodeStep）。
// ---------------------------------------------------------------------------

type LoginStep = 'form' | 'codeVerify' | 'deviceVerify'
type LoginTab = 'code' | 'password'

/** 纯数字视为 Origin ID；预填注册邮箱框时排除（注册接口仅认邮箱，见契约 v1.2）。 */
function looksLikeOriginId(v: string): boolean {
  return /^\d+$/.test(v.trim())
}

function LoginCard({
  onSwitchToRegister,
  onRegistrationIncomplete,
}: {
  onSwitchToRegister: (email: string) => void
  onRegistrationIncomplete: (email: string, password: string) => void
}) {
  const { t, locale } = useI18n()
  const [tab, setTab] = useState<LoginTab>('code')
  const [step, setStep] = useState<LoginStep>('form')
  const [account, setAccount] = useState('')
  const [password, setPassword] = useState('')
  const [code, setCode] = useState('')
  const [ttl, setTtl] = useState(0)
  const [sentAt, setSentAt] = useState(0)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)

  async function sendLoginCode() {
    const e = account.trim()
    if (!e) return
    setBusy(true)
    setError('')
    try {
      const res = await cloudApi.codeSend(e)
      setTtl(res.ttlSeconds)
      setSentAt(Date.now())
      setStep('codeVerify')
    } catch (err) {
      setError(cloudErrorText(t, err))
    } finally {
      setBusy(false)
    }
  }

  async function submitCodeLogin() {
    const e = account.trim()
    if (!e || !code.trim()) return
    setBusy(true)
    setError('')
    try {
      // 邮箱不存在时命中自动注册，恒传建议昵称（服务端仅在自动注册新用户时采用，
      // 已存在用户忽略该字段，恒传安全）。
      const auth = await cloudApi.codeVerify(e, code.trim(), suggest(locale))
      applyCloudSession(auth)
    } catch (err) {
      setError(cloudErrorText(t, err))
    } finally {
      setBusy(false)
    }
  }

  async function performLogin() {
    const e = account.trim()
    if (!e || !password) return
    setBusy(true)
    setError('')
    try {
      const result = await cloudApi.login(e, password)
      if (result.status === 'ok') {
        applyCloudSession(result.auth)
        return
      }
      setTtl(result.ttlSeconds)
      setSentAt(Date.now())
      setStep('deviceVerify')
    } catch (err) {
      if (err instanceof CloudApiError && err.code === 'registration_incomplete') {
        onRegistrationIncomplete(looksLikeOriginId(e) ? '' : e, password)
        return
      }
      setError(cloudErrorText(t, err))
    } finally {
      setBusy(false)
    }
  }

  async function submitDeviceVerify() {
    if (!code.trim()) return
    setBusy(true)
    setError('')
    try {
      const auth = await cloudApi.loginVerify(account.trim(), password, code.trim())
      applyCloudSession(auth)
    } catch (err) {
      setError(cloudErrorText(t, err))
    } finally {
      setBusy(false)
    }
  }

  function backToForm() {
    setStep('form')
    setError('')
    setCode('')
  }

  const useCode = tab === 'code'
  const headTitle = step === 'deviceVerify' ? t('cloud.deviceVerifyTitle') : step === 'codeVerify' ? t('cloud.loginTabCode') : t('cloud.loginTitle')
  const headSubtitle =
    step === 'deviceVerify'
      ? looksLikeOriginId(account)
        ? t('cloud.deviceVerifySubtitleAccount')
        : t('cloud.deviceVerifySubtitle', { email: account.trim() })
      : step === 'codeVerify'
        ? t('cloud.codeLoginSubtitle', { email: account.trim() })
        : t('cloud.loginSubtitle')

  return (
    <div className="cloud-card">
      <div className="cloud-card-head">
        <span className="cloud-card-icon">
          <Cloud size={20} />
        </span>
        <h3>{headTitle}</h3>
        <p>{headSubtitle}</p>
      </div>

      {step === 'codeVerify' ? (
        <VerificationCodeStep
          code={code}
          onCodeChange={setCode}
          ttlSeconds={ttl}
          sentAt={sentAt}
          busy={busy}
          error={error}
          onResend={() => void sendLoginCode()}
          onSubmit={() => void submitCodeLogin()}
          onBack={backToForm}
        />
      ) : step === 'deviceVerify' ? (
        <VerificationCodeStep
          code={code}
          onCodeChange={setCode}
          ttlSeconds={ttl}
          sentAt={sentAt}
          busy={busy}
          error={error}
          onResend={() => void performLogin()}
          onSubmit={() => void submitDeviceVerify()}
          onBack={backToForm}
        />
      ) : (
        <>
          <div className="seg-tabs mb-5">
            <button
              type="button"
              className={cn('seg-tab', useCode && 'active')}
              onClick={() => {
                setTab('code')
                setError('')
              }}
            >
              {t('cloud.loginTabCode')}
            </button>
            <button
              type="button"
              className={cn('seg-tab', !useCode && 'active')}
              onClick={() => {
                setTab('password')
                setError('')
              }}
            >
              {t('cloud.loginTabPassword')}
            </button>
          </div>
          <form
            className="flex flex-col"
            onSubmit={(e: FormEvent) => {
              e.preventDefault()
              if (useCode) void sendLoginCode()
              else void performLogin()
            }}
          >
            <label className="field-label" style={{ marginTop: 0 }}>
              {useCode ? t('cloud.emailPlaceholder') : t('cloud.accountPlaceholder')}
            </label>
            <input
              className="text-input"
              type={useCode ? 'email' : 'text'}
              required
              spellCheck={false}
              autoComplete="username"
              placeholder={useCode ? t('cloud.emailPlaceholder') : t('cloud.accountPlaceholder')}
              value={account}
              disabled={busy}
              onChange={(e) => setAccount(e.target.value)}
            />
            {useCode ? null : (
              <>
                <label className="field-label">{t('cloud.passwordPlaceholder')}</label>
                <input
                  className="text-input"
                  type="password"
                  required
                  placeholder={t('cloud.passwordPlaceholder')}
                  value={password}
                  disabled={busy}
                  onChange={(e) => setPassword(e.target.value)}
                />
              </>
            )}
            {error ? <p className="mt-2 text-[12px] text-danger">{error}</p> : null}
            <button type="submit" className="btn primary block mt-5" disabled={busy}>
              {busy ? t('common.loading') : useCode ? t('cloud.sendCode') : t('cloud.login')}
            </button>
            <p className="mt-4 text-center text-[11.5px] text-text3">
              {t('cloud.noAccountYet')}{' '}
              <button type="button" className="link-btn" onClick={() => onSwitchToRegister(looksLikeOriginId(account) ? '' : account.trim())}>
                {t('cloud.register')}
              </button>
            </p>
          </form>
        </>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// 注册卡片：邮箱+密码(≥8)+昵称(选填) → 验证码
// ---------------------------------------------------------------------------

type RegisterStep = 'form' | 'verify'

function RegisterCard({
  initialEmail,
  initialPassword,
  incomplete,
  onSwitchToLogin,
}: {
  initialEmail: string
  initialPassword: string
  incomplete: boolean
  onSwitchToLogin: () => void
}) {
  const { t, locale } = useI18n()
  const [step, setStep] = useState<RegisterStep>('form')
  const [email, setEmail] = useState(initialEmail)
  const [password, setPassword] = useState(initialPassword)
  // 预填「形容词+动物」建议昵称（情绪触点前置，见 lib/cloud/nickname.ts）；
  // 用户可改可清空，清空后不自动重填，仅提交前静默兜底（见 doRegister）。
  const [nickname, setNickname] = useState(() => suggest(locale))
  const [code, setCode] = useState('')
  const [ttl, setTtl] = useState(0)
  const [sentAt, setSentAt] = useState(0)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)

  async function doRegister() {
    setBusy(true)
    setError('')
    // 提交前若昵称被清空，静默重新建议一个，保证非空且不打断用户操作。
    const finalNickname = nickname.trim() || suggest(locale)
    if (finalNickname !== nickname) setNickname(finalNickname)
    try {
      const res = await cloudApi.register(email.trim(), password, finalNickname)
      setTtl(res.ttlSeconds)
      setSentAt(Date.now())
      setStep('verify')
    } catch (err) {
      setError(cloudErrorText(t, err))
    } finally {
      setBusy(false)
    }
  }

  function submitForm(e: FormEvent) {
    e.preventDefault()
    if (!email.trim() || password.length < 8) {
      setError(t('cloud.err.validation'))
      return
    }
    void doRegister()
  }

  async function submitVerify() {
    if (!code.trim()) return
    setBusy(true)
    setError('')
    try {
      const auth = await cloudApi.registerVerify(email.trim(), code.trim())
      applyCloudSession(auth)
    } catch (err) {
      setError(cloudErrorText(t, err))
    } finally {
      setBusy(false)
    }
  }

  const headTitle = step === 'verify' ? t('cloud.registerVerifyTitle') : t('cloud.registerTitle')
  const headSubtitle = step === 'verify' ? t('cloud.registerVerifySubtitle', { email: email.trim() }) : t('cloud.registerSubtitle')

  return (
    <div className="cloud-card">
      <div className="cloud-card-head">
        <span className="cloud-card-icon">
          <Cloud size={20} />
        </span>
        <h3>{headTitle}</h3>
        <p>{headSubtitle}</p>
      </div>

      {step === 'verify' ? (
        <VerificationCodeStep
          code={code}
          onCodeChange={setCode}
          ttlSeconds={ttl}
          sentAt={sentAt}
          busy={busy}
          error={error}
          onResend={() => void doRegister()}
          onSubmit={() => void submitVerify()}
          onBack={() => {
            setStep('form')
            setError('')
            setCode('')
          }}
        />
      ) : (
        <form className="flex flex-col" onSubmit={submitForm}>
          {incomplete ? <p className="mb-4 text-[12px] leading-relaxed text-text3">{t('cloud.registerIncompleteNotice')}</p> : null}
          <label className="field-label" style={{ marginTop: 0 }}>
            {t('cloud.emailPlaceholder')}
          </label>
          <input
            className="text-input"
            type="email"
            required
            spellCheck={false}
            placeholder={t('cloud.emailPlaceholder')}
            value={email}
            disabled={busy}
            onChange={(e) => setEmail(e.target.value)}
          />
          <label className="field-label">{t('cloud.passwordPlaceholder')}</label>
          <input
            className="text-input"
            type="password"
            required
            placeholder={t('cloud.passwordPlaceholder')}
            value={password}
            disabled={busy}
            onChange={(e) => setPassword(e.target.value)}
          />
          <p className="mt-1 text-[11px] text-text3">{t('cloud.passwordHint')}</p>
          <label className="field-label">{t('cloud.nicknamePlaceholder')}</label>
          <div className="flex items-center gap-2">
            <input
              className="text-input flex-1"
              type="text"
              placeholder={t('cloud.nicknamePlaceholder')}
              value={nickname}
              disabled={busy}
              onChange={(e) => setNickname(e.target.value)}
            />
            <button
              type="button"
              className="icon-btn flex-shrink-0"
              disabled={busy}
              title={t('cloud.nicknameReroll')}
              aria-label={t('cloud.nicknameReroll')}
              onClick={() => setNickname(suggest(locale))}
            >
              <span aria-hidden="true" className="text-[15px] leading-none">
                🎲
              </span>
            </button>
          </div>
          {error ? <p className="mt-2 text-[12px] text-danger">{error}</p> : null}
          <button type="submit" className="btn primary block mt-5" disabled={busy}>
            {busy ? t('common.loading') : t('cloud.register')}
          </button>
          <p className="mt-4 text-center text-[11.5px] text-text3">
            {t('cloud.alreadyHaveAccount')}{' '}
            <button type="button" className="link-btn" onClick={onSwitchToLogin}>
              {t('cloud.login')}
            </button>
          </p>
        </form>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// 已登录：资料卡 + 设备列表
// ---------------------------------------------------------------------------

function LoggedInPanel({ user }: { user: { nickname: string; email: string; plan: string; originId: number | null } }) {
  const { t } = useI18n()
  const [loggingOut, setLoggingOut] = useState(false)
  const displayName = user.nickname || user.email.split('@')[0]

  async function logout() {
    setLoggingOut(true)
    const rt = getCloudRefreshToken()
    if (rt) {
      try {
        await cloudApi.logout(rt)
      } catch {
        // 尽力通知服务端吊销 refreshToken，失败也不阻塞本地登出。
      }
    }
    clearCloudSession()
  }

  return (
    <>
      <div className="set-group">
        <div className="flex items-center gap-3 p-4">
          <div className="flex min-w-0 flex-1 flex-wrap items-center gap-2">
            <b className="text-[14px] font-semibold">{displayName}</b>
            {user.plan ? (
              <span className="rounded-full bg-accent-weak px-2 py-0.5 text-[10.5px] font-semibold text-accent">{user.plan}</span>
            ) : null}
            <OriginIdBadge originId={user.originId} />
          </div>
          <button type="button" className="btn ghost sm flex-shrink-0" disabled={loggingOut} onClick={() => void logout()}>
            {t('common.logout')}
          </button>
        </div>
      </div>
      <p className="mb-1 mt-6 text-[12.5px] font-semibold text-text2">{t('cloud.securityTitle')}</p>
      <p className="set-desc" style={{ marginBottom: 10 }}>
        {t('cloud.securityDesc')}
      </p>
      <div className="set-group">
        <SetRow title={t('cloud.emailLabel')}>
          <span className="min-w-0 flex-shrink truncate text-[12.5px] text-text2">{user.email}</span>
        </SetRow>
      </div>
      <DeviceListSection />
    </>
  )
}

/** Origin ID 徽标：胶囊 pill（accent 弱底、圆角、tabular-nums），点击复制纯数字
 *  （clipboard API 不可用/被拒时静默降级，不阻断其它交互）。null（pending 用户）兜底
 *  显示 #— 且不可点。颜色走 design.css .origin-badge（unlayered，显式声明背景/颜色
 *  才压得过下方全局 button 重置，Tailwind 工具类在这个元素上不生效，同 .link-btn 注释）。 */
function OriginIdBadge({ originId }: { originId: number | null }) {
  const { t } = useI18n()
  const [copied, setCopied] = useState(false)
  const timer = useRef<number | undefined>(undefined)
  useEffect(() => () => window.clearTimeout(timer.current), [])

  async function copy() {
    if (originId == null) return
    try {
      await navigator.clipboard.writeText(String(originId))
      setCopied(true)
      window.clearTimeout(timer.current)
      timer.current = window.setTimeout(() => setCopied(false), 1500)
    } catch {
      // 剪贴板 API 不可用（非安全上下文 / 权限被拒等），静默降级，不阻断其它交互
    }
  }

  return (
    <button type="button" className="origin-badge flex-shrink-0" disabled={originId == null} onClick={() => void copy()} title={t('cloud.originId')}>
      <span>#{originId == null ? '—' : originId}</span>
      {originId != null ? copied ? <Check size={11} /> : <Copy size={11} /> : null}
    </button>
  )
}

/** 内联展示的设备行数上限；超出后「查看全部」展开为限高滚动容器 + 过滤输入框，
 *  避免几十台设备把设置页撑爆（见需求：可扩展设备列表）。 */
const DEVICE_INLINE_LIMIT = 5

function DeviceListSection() {
  const { t } = useI18n()
  const showDeviceSync = useShowDeviceSync()
  const currentId = cloudDeviceId()
  const { data, isLoading, isError, refetch } = useQuery({
    queryKey: DEVICES_QUERY_KEY,
    queryFn: () => cloudApi.devices().then((r) => r.devices),
    staleTime: 10_000,
  })
  const [showAll, setShowAll] = useState(false)
  const [filter, setFilter] = useState('')
  const [openId, setOpenId] = useState<string | null>(null)

  // 服务端已按 lastSeenAt 降序返回，这里只把当前设备置顶（稳定排序，不打乱其余顺序）。
  const sorted = useMemo(() => {
    if (!data) return []
    return [...data].sort((a, b) => (a.deviceId === currentId ? -1 : b.deviceId === currentId ? 1 : 0))
  }, [data, currentId])

  const filtered = useMemo(() => {
    const q = filter.trim().toLowerCase()
    if (!q) return sorted
    return sorted.filter((d) => (d.name || '').toLowerCase().includes(q) || (d.platform || '').toLowerCase().includes(q))
  }, [sorted, filter])

  const hasMore = sorted.length > DEVICE_INLINE_LIMIT
  const visible = showAll ? filtered : sorted.slice(0, DEVICE_INLINE_LIMIT)

  function toggleShowAll() {
    setShowAll((v) => !v)
    setFilter('')
  }

  return (
    <>
      <p className="mb-1 mt-6 text-[12.5px] font-semibold text-text2">{t('cloud.devicesTitle')}</p>
      <p className="set-desc" style={{ marginBottom: 10 }}>
        {t('cloud.devicesDesc')}
      </p>
      <div className="set-group">
        <SetRow title="在侧边栏显示设备同步" desc="开启后即使仅本机也在侧边栏展示设备区，便于提前熟悉多设备协同入口">
          <SetSwitch checked={showDeviceSync} onCheckedChange={setShowDeviceSync} />
        </SetRow>
        {isLoading ? (
          <p className="p-4 text-[12px] text-text3">{t('common.loading')}</p>
        ) : isError ? (
          <div className="flex items-center justify-between p-4">
            <p className="text-[12px] text-danger">{t('cloud.devicesLoadFailed')}</p>
            <button type="button" className="btn ghost sm" onClick={() => void refetch()}>
              {t('common.retry')}
            </button>
          </div>
        ) : !data || data.length === 0 ? (
          <p className="p-4 text-[12px] text-text3">{t('cloud.devicesEmpty')}</p>
        ) : (
          <>
            {showAll ? (
              <div className="device-filter">
                <div className="relative">
                  <Search size={13} className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-text3" />
                  <input
                    className="text-input"
                    style={{ paddingLeft: 30 }}
                    placeholder={t('cloud.deviceFilterPlaceholder')}
                    value={filter}
                    onChange={(e) => setFilter(e.target.value)}
                    autoFocus
                  />
                </div>
              </div>
            ) : null}
            <div className={cn(showAll && 'device-scroll')}>
              {visible.length === 0 ? (
                <p className="device-list-empty">{t('cloud.devicesFilterEmpty')}</p>
              ) : (
                visible.map((d) => (
                  <DeviceItem
                    key={d.id}
                    device={d}
                    isCurrent={d.deviceId === currentId}
                    open={openId === d.id}
                    onToggle={() => setOpenId((cur) => (cur === d.id ? null : d.id))}
                  />
                ))
              )}
            </div>
            {hasMore ? (
              <button type="button" className="device-list-toggle" onClick={toggleShowAll}>
                {showAll ? t('cloud.devicesCollapse') : t('cloud.devicesShowAll', { n: sorted.length })}
              </button>
            ) : null}
          </>
        )}
      </div>
    </>
  )
}

/** 设备行：主体点击展开/收起详情（最近登录 IP、首次信任时间、最近活跃、平台、
 *  App 版本、设备 ID），对标 Telegram/Google 设备管理的信息量，不做额外采集。 */
function DeviceItem({
  device,
  isCurrent,
  open,
  onToggle,
}: {
  device: CloudDevice
  isCurrent: boolean
  open: boolean
  onToggle: () => void
}) {
  const { t } = useI18n()
  const qc = useQueryClient()
  const [editing, setEditing] = useState(false)
  const [name, setName] = useState(device.name)
  const [renameError, setRenameError] = useState('')

  const renameMut = useMutation({
    mutationFn: (n: string) => cloudApi.renameDevice(device.id, n),
    onSuccess: () => {
      setEditing(false)
      void qc.invalidateQueries({ queryKey: DEVICES_QUERY_KEY })
    },
    onError: (err) => setRenameError(cloudErrorText(t, err)),
  })

  const deleteMut = useMutation({
    mutationFn: () => cloudApi.deleteDevice(device.id),
    onSuccess: () => {
      if (isCurrent) clearCloudSession()
      void qc.invalidateQueries({ queryKey: DEVICES_QUERY_KEY })
    },
  })

  function commitRename() {
    const trimmed = name.trim()
    if (trimmed.length < 1 || trimmed.length > 64) {
      setRenameError(t('cloud.deviceRenameInvalid'))
      return
    }
    if (trimmed === device.name) {
      setEditing(false)
      return
    }
    renameMut.mutate(trimmed)
  }

  function cancelRename() {
    setEditing(false)
    setName(device.name)
    setRenameError('')
  }

  async function handleDelete() {
    const ok = await confirmDialog({
      title: t('cloud.deviceDeleteTitle'),
      message: isCurrent ? `${t('cloud.deviceDeleteDesc')} ${t('cloud.deviceDeleteCurrentWarning')}` : t('cloud.deviceDeleteDesc'),
      danger: true,
    })
    if (ok) deleteMut.mutate()
  }

  const PlatformIcon = device.platform === 'android' || device.platform === 'ios' ? Smartphone : device.platform === 'windows' || device.platform === 'macos' || device.platform === 'linux' ? Monitor : Cloud

  return (
    <div className="device-item">
      <div className="set-row">
        {editing ? (
          <div className="grid h-8 w-8 flex-shrink-0 place-items-center rounded-lg bg-surface2 text-text2">
            <PlatformIcon size={15} />
          </div>
        ) : (
          <button type="button" className="device-row-main" onClick={onToggle}>
            <ChevronRight size={13} className={cn('device-chevron', open && 'open')} />
            <div className="grid h-8 w-8 flex-shrink-0 place-items-center rounded-lg bg-surface2 text-text2">
              <PlatformIcon size={15} />
            </div>
            <div className="min-w-0 flex-1 text-left">
              <div className="flex items-center gap-2">
                <i className={cn('queue-dot', device.isOnline && 'on')} title={device.isOnline ? '在线' : '离线'} />
                <b className="truncate text-[13px] font-medium">{device.name || '-'}</b>
                {isCurrent ? (
                  <span className="flex-shrink-0 rounded-full bg-accent-weak px-1.5 py-0.5 text-[9.5px] font-semibold text-accent">
                    {t('cloud.deviceCurrent')}
                  </span>
                ) : null}
              </div>
              <p className="text-[11.5px] text-text3">{fmtRelativeTime(device.lastSeenAt)}</p>
            </div>
          </button>
        )}
        {editing ? (
          <div className="flex flex-1 items-center gap-1.5">
            <input
              className="text-input short"
              autoFocus
              maxLength={64}
              value={name}
              disabled={renameMut.isPending}
              onChange={(e) => {
                setName(e.target.value)
                setRenameError('')
              }}
              onKeyDown={(e) => {
                if (e.key === 'Enter') commitRename()
                if (e.key === 'Escape') cancelRename()
              }}
            />
            <button type="button" className="icon-btn sm accent" disabled={renameMut.isPending} onClick={commitRename}>
              <Check size={14} />
            </button>
            <button type="button" className="icon-btn sm" disabled={renameMut.isPending} onClick={cancelRename}>
              <X size={14} />
            </button>
          </div>
        ) : (
          <>
            <button type="button" className="icon-btn sm" title={t('cloud.deviceRename')} aria-label={t('cloud.deviceRename')} onClick={() => setEditing(true)}>
              <Pencil size={14} />
            </button>
            <button
              type="button"
              className="icon-btn sm text-text3 hover:text-danger"
              title={t('cloud.deviceDeleteTitle')}
              aria-label={t('cloud.deviceDeleteTitle')}
              disabled={deleteMut.isPending}
              onClick={() => void handleDelete()}
            >
              <Trash2 size={14} />
            </button>
          </>
        )}
        {renameError ? <p className="w-full px-0 text-[11.5px] text-danger">{renameError}</p> : null}
      </div>
      {open && !editing ? (
        <div className="device-detail">
          <div className="d-field">
            <span>{t('cloud.deviceDetailPlatform')}</span>
            <p>{platformLabel(t, device.platform)}</p>
          </div>
          <div className="d-field">
            <span>{t('cloud.deviceDetailAppVersion')}</span>
            <p>{device.appVersion || '—'}</p>
          </div>
          <div className="d-field">
            <span>{t('cloud.deviceDetailLastIp')}</span>
            <p>{device.lastIp || '—'}</p>
          </div>
          <div className="d-field">
            <span>{t('cloud.deviceDetailCreatedAt')}</span>
            <p>{fmtIsoTime(device.createdAt)}</p>
          </div>
          <div className="d-field">
            <span>{t('cloud.deviceDetailLastSeenAt')}</span>
            <p>{fmtIsoTime(device.lastSeenAt)}</p>
          </div>
          <div className="d-field mono">
            <span>{t('cloud.deviceDetailId')}</span>
            <p>{device.deviceId}</p>
          </div>
        </div>
      ) : null}
    </div>
  )
}

// ---------------------------------------------------------------------------
// 云服务器地址
// ---------------------------------------------------------------------------

function CloudServerAddressGroup() {
  const { t } = useI18n()
  const [value, setValue] = useState(getCloudBaseUrl())
  const [error, setError] = useState('')
  const [custom, setCustom] = useState(isCloudBaseUrlCustom())

  function commit(next: string) {
    const trimmed = next.trim()
    if (trimmed === '') {
      resetCloudBaseUrl()
      setValue(getCloudBaseUrl())
      setCustom(false)
      setError('')
      return
    }
    if (!/^https?:\/\/.+/i.test(trimmed)) {
      setError(t('cloud.serverAddrInvalid'))
      setValue(trimmed)
      return
    }
    setCloudBaseUrl(trimmed)
    setValue(trimmed)
    setCustom(isCloudBaseUrlCustom())
    setError('')
  }

  function reset() {
    resetCloudBaseUrl()
    setValue(getCloudBaseUrl())
    setCustom(false)
    setError('')
  }

  return (
    <>
      <p className="mb-1 mt-6 text-[12.5px] font-semibold text-text2">{t('cloud.serverAddrTitle')}</p>
      <p className="set-desc" style={{ marginBottom: 10 }}>
        {t('cloud.serverAddrDesc')}
      </p>
      <div className="set-group">
        <SetRow title={t('cloud.serverAddr')}>
          <TextInput value={value} onCommit={commit} />
          {custom ? (
            <button type="button" className="btn ghost sm" onClick={reset}>
              {t('cloud.serverAddrReset')}
            </button>
          ) : null}
        </SetRow>
      </div>
      {error ? <p className="mt-2 text-[12px] text-danger">{error}</p> : null}
    </>
  )
}
