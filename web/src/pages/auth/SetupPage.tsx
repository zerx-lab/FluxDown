// 首次运行向导：为服务设置访问密钥（`/api/v1/setup`）。策略与服务端 `validate_access_key`
// 逐条一致（lib/token-policy.ts）；提交成功后直接以该密钥登录。
// 密钥仍空时必须另贴 data_dir 里的一次性引导码，不能匿名落盘。

import { useNavigate } from '@tanstack/react-router'
import { Copy, Dices, Eye, EyeOff } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useT } from '../../i18n'
import { SetupError, fetchSetupStatus, saveToken, submitSetup } from '../../lib/access'
import { copyText } from '../../lib/copy'
import { errorMessage } from '../../lib/rpc/error'
import { ACCESS_KEY_MAX_LEN, ACCESS_KEY_MIN_LEN, randomAccessKey, validateAccessKey } from '../../lib/token-policy'
import type { AccessKeyIssue } from '../../lib/token-policy'
import { Button, FieldError, FieldHint, Form, FormField, Icon, Input, InputWithAction, toast } from '../../ui'
import { AuthCard } from './AuthCard'

const ISSUE_KEY: Record<AccessKeyIssue, string> = {
  badChars: 'webKeyBadChars',
  tooShort: 'webKeyTooShort',
  tooLong: 'webKeyTooLong',
  needsMix: 'webKeyNeedsMix',
}

// 401 = 引导码错误，429 = 错误尝试过多被限流；两者都留在表单上让用户重试。
const REJECTION_KEY: Record<number, string | undefined> = {
  401: 'webSetupBootstrapWrong',
  429: 'webSetupTooManyAttempts',
}

export function SetupPage() {
  const t = useT()
  const navigate = useNavigate()
  const [bootstrap, setBootstrap] = useState('')
  const [needsBootstrap, setNeedsBootstrap] = useState(true)
  const [key, setKey] = useState('')
  const [confirm, setConfirm] = useState('')
  const [reveal, setReveal] = useState(false)
  const [minLength, setMinLength] = useState(ACCESS_KEY_MIN_LEN)
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const controller = new AbortController()
    fetchSetupStatus(controller.signal)
      .then((status) => {
        if (!status.setupRequired) void navigate({ to: '/login', replace: true })
        else {
          setMinLength(status.minLength)
          setNeedsBootstrap(status.needsBootstrap)
        }
      })
      .catch((cause: unknown) => {
        if (!controller.signal.aborted) setError(t('webSetupFailed', { error: errorMessage(cause) }))
      })
    return () => controller.abort()
  }, [navigate, t])

  const issue = key === '' ? null : validateAccessKey(key)
  const issueText = issue ? t(ISSUE_KEY[issue], { min: Math.max(minLength, ACCESS_KEY_MIN_LEN), max: ACCESS_KEY_MAX_LEN }) : null
  const mismatch = confirm !== '' && confirm !== key
  const bootstrapMissing = needsBootstrap && bootstrap.trim() === ''
  const canSubmit = key !== '' && issue === null && confirm === key && !bootstrapMissing && !submitting

  const generate = () => {
    const next = randomAccessKey()
    setKey(next)
    setConfirm(next)
    setReveal(true)
  }

  const copy = () => {
    copyText(key)
    toast.success(t('webCopied'))
  }

  const submit = async () => {
    if (!canSubmit) return
    setSubmitting(true)
    setError(null)
    try {
      await submitSetup(key, bootstrap.trim())
      saveToken(key, true)
      await navigate({ to: '/', replace: true })
    } catch (cause) {
      if (cause instanceof SetupError && cause.status === 409) {
        toast.info(t('webSetupAlreadyDone'))
        await navigate({ to: '/login', replace: true })
        return
      }
      const rejection = cause instanceof SetupError ? REJECTION_KEY[cause.status] : undefined
      setError(rejection ? t(rejection) : t('webSetupFailed', { error: errorMessage(cause) }))
      setSubmitting(false)
    }
  }

  return (
    <AuthCard title={t('webSetupTitle')} subtitle={t('webSetupSubtitle')}>
      <Form onSubmit={() => void submit()}>
        {needsBootstrap ? (
          <FormField label={t('webSetupBootstrapLabel')} htmlFor="setup-bootstrap" hint={t('webSetupBootstrapHint')}>
            <Input
              id="setup-bootstrap"
              name="setup-bootstrap"
              autoComplete="off"
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              autoFocus
              value={bootstrap}
              onChange={(event) => setBootstrap(event.target.value)}
            />
          </FormField>
        ) : null}
        <FormField label={t('webSetupKeyLabel')} htmlFor="setup-key" error={issueText}>
          <InputWithAction
            input={
              <Input
                id="setup-key"
                type={reveal ? 'text' : 'password'}
                autoComplete="new-password"
                autoCapitalize="off"
                autoCorrect="off"
                spellCheck={false}
                autoFocus={!needsBootstrap}
                value={key}
                invalid={issueText !== null}
                onChange={(event) => setKey(event.target.value)}
                trailing={
                  <Button
                    variant="ghost"
                    iconOnly
                    className="text-muted-foreground hover:text-foreground"
                    aria-label={reveal ? t('webHideKey') : t('webShowKey')}
                    onClick={() => setReveal((value) => !value)}
                  >
                    <Icon icon={reveal ? EyeOff : Eye} size="md" />
                  </Button>
                }
              />
            }
            action={<Button icon={Dices} onClick={generate}>{t('webSetupGenerate')}</Button>}
          />
        </FormField>
        <FormField label={t('webSetupConfirmLabel')} htmlFor="setup-confirm" error={mismatch ? t('webKeyMismatch') : null}>
          <Input
            id="setup-confirm"
            type={reveal ? 'text' : 'password'}
            autoComplete="new-password"
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            value={confirm}
            invalid={mismatch}
            onChange={(event) => setConfirm(event.target.value)}
          />
        </FormField>
        <FieldHint>{t('webSetupHint')}</FieldHint>
        {error ? <FieldError>{error}</FieldError> : null}
        <div className="flex gap-2">
          {reveal && key !== '' ? (
            <Button icon={Copy} onClick={copy}>
              {t('webCopy')}
            </Button>
          ) : null}
          <Button type="submit" variant="primary" loading={submitting} disabled={!canSubmit} className="flex-1">
            {t('webSetupSubmit')}
          </Button>
        </div>
      </Form>
    </AuthCard>
  )
}
