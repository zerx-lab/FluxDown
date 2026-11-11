// 本机做种：把守护进程所在机器上已有的文件 / 目录登记为已完成 BT 任务并开始做种。
// Web 无服务器文件选择器（服务端文件系统 ≠ 浏览器文件系统），路径手动输入。
// 慢请求（哈希大数据可达数分钟）：提交期间忙碌态、不可关闭；创建类提交不自动重试。
// 「种子选项」默认折叠，只在现场生成种子（未填现成 .torrent）时出现；填了现成 .torrent 时改为「跳过校验」。

import { ChevronDown, ChevronRight } from 'lucide-react'
import { useState } from 'react'
import { useT } from '../../../i18n'
import { rpc } from '../../../lib/rpc'
import type { SeedTorrentVersion } from '../../../lib/rpc'
import { rpcErrorText } from '../../../lib/rpcErrorText'
import { ConfirmFooter, Dialog, FieldError, FieldHint, Form, FormField, FormRow, Icon, Input, OptionGroup, OptionRow, Select, Spinner, Switch, Textarea, toast } from '../../../ui'
import { buildSeedLocalParams, pieceLengthLabel, SEED_LOCAL_FORM_DEFAULTS, SEED_PIECE_LENGTHS, SEED_TORRENT_VERSIONS, seedLocalFeedback } from './seedLocal'
import type { SeedLocalForm } from './seedLocal'
import { closeSeedLocal } from './store'

const VERSION_KEY: Record<SeedTorrentVersion, string> = {
  v1: 'btCreateVersionV1',
  v2: 'btCreateVersionV2',
  hybrid: 'btCreateVersionHybrid',
}

export function SeedLocalDialog() {
  const t = useT()
  const [form, setForm] = useState<SeedLocalForm>(SEED_LOCAL_FORM_DEFAULTS)
  const [optionsOpen, setOptionsOpen] = useState(false)
  const [error, setError] = useState('')
  const [pending, setPending] = useState(false)
  const set = <K extends keyof SeedLocalForm>(key: K, value: SeedLocalForm[K]) => setForm((prev) => ({ ...prev, [key]: value }))
  const generating = form.torrentPath.trim() === ''

  const submit = async () => {
    if (pending) return
    const built = buildSeedLocalParams(form)
    if (!built.ok) {
      if (built.error === 'outputRequired') setOptionsOpen(true)
      setError(t(built.error === 'pathRequired' ? 'seedLocalErrPathRequired' : 'btCreateOutputRequired'))
      return
    }
    setPending(true)
    setError('')
    try {
      const result = await rpc.daemon.task.seedLocal(built.params)
      const fb = seedLocalFeedback(result, built.params)
      if ('params' in fb) toast.key(fb.key, fb.level, fb.params)
      else toast.key(fb.key, fb.level)
      closeSeedLocal()
    } catch (err) {
      setError(rpcErrorText(err, t))
      setPending(false)
    }
  }

  const switchRow = (key: 'private' | 'startSeeding' | 'superSeeding' | 'skipCheck', titleKey: string, descKey?: string, disabled = false) => (
    <OptionRow
      title={t(titleKey)}
      description={descKey === undefined ? undefined : t(descKey)}
      control={<Switch checked={form[key]} disabled={pending || disabled} onCheckedChange={(next) => set(key, next)} aria-label={t(titleKey)} />}
    />
  )

  return (
    <Dialog
      open
      onOpenChange={(open) => !open && !pending && closeSeedLocal()}
      modalLocked={pending}
      size="md"
      title={t('seedLocalTitle')}
      description={t('seedLocalDesc')}
      footer={<ConfirmFooter okLabel={t('seedLocalSubmit')} loading={pending} okDisabled={pending} onCancel={closeSeedLocal} onOk={() => void submit()} />}
    >
      <Form onSubmit={() => void submit()}>
        <FormField label={t('seedLocalPath')} htmlFor="seed-local-path">
          <Input
            id="seed-local-path"
            value={form.path}
            placeholder={t('seedLocalPathPlaceholder')}
            spellCheck={false}
            autoFocus
            autoComplete="off"
            disabled={pending}
            onChange={(event) => set('path', event.target.value)}
            className="font-mono text-xs"
          />
        </FormField>
        <FormField label={t('seedLocalTorrent')} htmlFor="seed-local-torrent" hint={t('seedLocalTorrentHint')}>
          <Input
            id="seed-local-torrent"
            value={form.torrentPath}
            spellCheck={false}
            autoComplete="off"
            disabled={pending}
            onChange={(event) => set('torrentPath', event.target.value)}
            className="font-mono text-xs"
          />
        </FormField>
        {generating ? (
          <FormField label={t('seedLocalTrackers')} htmlFor="seed-local-trackers" hint={t('seedLocalTrackersHint')}>
            <Textarea
              id="seed-local-trackers"
              value={form.trackers}
              spellCheck={false}
              disabled={pending}
              onChange={(event) => set('trackers', event.target.value)}
              className="font-mono text-xs"
            />
          </FormField>
        ) : null}
        <OptionGroup>
          <OptionRow
            title={t('seedLocalUnlimited')}
            description={t('seedLocalUnlimitedDesc')}
            control={<Switch checked={form.unlimited} disabled={pending} onCheckedChange={(next) => set('unlimited', next)} aria-label={t('seedLocalUnlimited')} />}
          />
          {/* 与 GPUI 同序：不限速 → 超级做种 → 跳过校验；只生成种子文件（不做种）时超级做种无意义，禁用。 */}
          {switchRow('superSeeding', 'btSuperSeeding', 'btSuperSeedingDesc', generating && !form.startSeeding)}
          {generating ? null : switchRow('skipCheck', 'btSeedSkipCheck', 'btSeedSkipCheckDesc')}
        </OptionGroup>
        {generating ? (
          <div className="flex w-full flex-col gap-4">
            <button
              type="button"
              className="flex min-h-control w-full items-center gap-1 text-left coarse:min-h-touch"
              aria-expanded={optionsOpen}
              onClick={() => setOptionsOpen((open) => !open)}
            >
              <Icon icon={optionsOpen ? ChevronDown : ChevronRight} size="md" className="text-text-tertiary" />
              <span className="text-sm font-medium">{t('btCreateOptions')}</span>
              <span className="ml-2 h-px flex-1 bg-hairline" />
            </button>
            {optionsOpen ? (
              <>
                <FormRow>
                  <FormField label={t('btCreateVersion')}>
                    <Select
                      value={form.version}
                      disabled={pending}
                      aria-label={t('btCreateVersion')}
                      options={SEED_TORRENT_VERSIONS.map((value) => ({ value, label: t(VERSION_KEY[value]) }))}
                      onValueChange={(next) => set('version', next)}
                    />
                  </FormField>
                  <FormField label={t('btCreatePieceLength')}>
                    <Select
                      value={String(form.pieceLength)}
                      disabled={pending}
                      aria-label={t('btCreatePieceLength')}
                      options={SEED_PIECE_LENGTHS.map((bytes) => ({ value: String(bytes), label: bytes === 0 ? t('btCreatePieceLengthAuto') : pieceLengthLabel(bytes) }))}
                      onValueChange={(next) => set('pieceLength', Number(next))}
                    />
                  </FormField>
                </FormRow>
                <FormField label={t('btCreateWebSeeds')} htmlFor="seed-local-webseeds" hint={t('btCreateWebSeedsHint')}>
                  <Textarea
                    id="seed-local-webseeds"
                    value={form.webSeeds}
                    spellCheck={false}
                    disabled={pending}
                    onChange={(event) => set('webSeeds', event.target.value)}
                    className="font-mono text-xs"
                  />
                </FormField>
                <FormField label={t('btCreateComment')} htmlFor="seed-local-comment">
                  <Input id="seed-local-comment" value={form.comment} autoComplete="off" disabled={pending} onChange={(event) => set('comment', event.target.value)} />
                </FormField>
                <FormField label={t('btCreateSource')} htmlFor="seed-local-source" hint={t('btCreateSourceHint')}>
                  <Input id="seed-local-source" value={form.source} spellCheck={false} autoComplete="off" disabled={pending} onChange={(event) => set('source', event.target.value)} />
                </FormField>
                <FormField label={t('btCreateTorrentOutput')} htmlFor="seed-local-output">
                  <Input
                    id="seed-local-output"
                    value={form.torrentOutput}
                    placeholder={t('btCreateTorrentOutputPlaceholder')}
                    spellCheck={false}
                    autoComplete="off"
                    disabled={pending}
                    onChange={(event) => set('torrentOutput', event.target.value)}
                    className="font-mono text-xs"
                  />
                </FormField>
                <OptionGroup>
                  {switchRow('private', 'btCreatePrivate', 'btCreatePrivateDesc')}
                  {switchRow('startSeeding', 'btCreateStartSeeding')}
                </OptionGroup>
              </>
            ) : null}
          </div>
        ) : null}
        {pending ? (
          <FieldHint className="flex items-center gap-2">
            <Spinner />
            {t('seedLocalWorking')}
          </FieldHint>
        ) : null}
        {error ? <FieldError className="break-all">{error}</FieldError> : null}
        <button type="submit" className="hidden" tabIndex={-1} aria-hidden />
      </Form>
    </Dialog>
  )
}
