// 代理：服务器出站代理（config 表）+ 连通性测试（/api/v1/proxy/test）。
import { useState } from 'react'
import { api } from '../../lib/api'
import { translateBackendMessage, useI18n } from '../../lib/i18n'
import type { ConfigMap } from '../../lib/types'
import { SetRow, SetSelect, TextFieldRow } from './controls'

const PROXY_TYPE_OPTIONS = [
  { value: 'http', label: 'HTTP' },
  { value: 'https', label: 'HTTPS' },
  { value: 'socks4', label: 'SOCKS4' },
  { value: 'socks5', label: 'SOCKS5' },
]

type TestState = { status: 'idle' | 'pending' | 'ok' | 'err'; detail?: string }

export function ProxySettings({
  config,
  mutate,
}: {
  config: ConfigMap
  mutate: (entries: ConfigMap) => void
}) {
  const { t, locale } = useI18n()
  const mode = config.proxy_mode ?? 'none'
  const type = config.proxy_type ?? 'socks5'
  const host = config.proxy_host ?? ''
  const port = config.proxy_port ?? ''
  const username = config.proxy_username ?? ''
  const password = config.proxy_password ?? ''
  const noList = config.proxy_no_list ?? ''

  const [testState, setTestState] = useState<TestState>({ status: 'idle' })

  const PROXY_MODE_OPTIONS = [
    { value: 'none', label: t('set.proxy.none') },
    { value: 'system', label: t('set.proxy.system') },
    { value: 'manual', label: t('set.proxy.manual') },
  ]

  async function runTest() {
    setTestState({ status: 'pending' })
    try {
      const res = await api.proxyTest({
        proxyType: type,
        host,
        port,
        username: username || undefined,
        password: password || undefined,
      })
      setTestState({ status: 'ok', detail: t('set.proxy.testOk', { ms: res.latencyMs }) })
    } catch (err) {
      setTestState({
        status: 'err',
        detail: err instanceof Error ? translateBackendMessage(err.message) : t('set.proxy.testFailed'),
      })
    }
  }

  return (
    <>
      <h2 className="set-title">{t('set.proxy')}</h2>
      <p className="set-desc">{t('set.proxy.desc')}</p>
      <p className="set-note">
        {locale === 'zh' ? (
          <>
            <b>{t('set.proxy.webNoteTitle')}</b>：{t('set.proxy.webNote')}
          </>
        ) : (
          t('set.proxy.webNote')
        )}
      </p>
      <div className="set-group">
        <SetRow title={t('set.proxy.mode')}>
          <SetSelect value={mode} onValueChange={(v) => mutate({ proxy_mode: v })} options={PROXY_MODE_OPTIONS} />
        </SetRow>
        {mode === 'manual' ? (
          <>
            <SetRow title={t('set.proxy.type')} desc="HTTP / HTTPS / SOCKS4 / SOCKS5">
              <SetSelect value={type} onValueChange={(v) => mutate({ proxy_type: v })} options={PROXY_TYPE_OPTIONS} />
            </SetRow>
            <TextFieldRow title={t('set.proxy.host')} value={host} placeholder="127.0.0.1" onCommit={(v) => mutate({ proxy_host: v })} />
            <TextFieldRow title={t('set.proxy.port')} value={port} placeholder="1080" onCommit={(v) => mutate({ proxy_port: v })} />
            <TextFieldRow
              title={t('set.proxy.username')}
              desc={t('common.optional')}
              value={username}
              onCommit={(v) => mutate({ proxy_username: v })}
            />
            <TextFieldRow
              title={t('set.proxy.password')}
              desc={t('common.optional')}
              value={password}
              password
              onCommit={(v) => mutate({ proxy_password: v })}
            />
            <TextFieldRow
              title={t('set.proxy.noList')}
              desc={t('set.proxy.noListDesc')}
              value={noList}
              placeholder="localhost, *.lan"
              onCommit={(v) => mutate({ proxy_no_list: v })}
            />
            <SetRow title={t('set.proxy.test')}>
              <div className="flex items-center gap-3">
                <button
                  type="button"
                  className="btn ghost sm"
                  disabled={testState.status === 'pending'}
                  onClick={runTest}
                >
                  {testState.status === 'pending' ? t('set.proxy.testing') : t('set.proxy.testRun')}
                </button>
                {testState.status === 'ok' ? <span className="text-[12px] text-success">{testState.detail}</span> : null}
                {testState.status === 'err' ? <span className="text-[12px] text-danger">{testState.detail}</span> : null}
              </div>
            </SetRow>
          </>
        ) : null}
      </div>
    </>
  )
}
