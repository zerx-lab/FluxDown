import { useEffect, useState } from 'react'
import { useT } from '../../../i18n'
import { rpc, CUSTOM_CATEGORIES_PREF_KEY, useAgent, type EditTaskParams, type TaskEditSnapshot } from '../../../lib/rpc'
import { rpcErrorText } from '../../../lib/rpcErrorText'
import { copyText } from '../../../lib/copy'
import { Button, ConfirmFooter, Dialog, FieldError, Form, FormField, Input, Textarea } from '../../../ui'
import { closeTaskEdit } from './store'
import { hasTaskEditChanges, taskEditPatch, taskEditValues, type TaskEditValues } from '../model/taskEditPatch'
import { categoriesFromPreference } from '../model/categories'
import { categoryLabel } from '../state'
import { parseTaskSegments } from '../model/taskEdit'
import { FsPickerDialog } from './FsPickerDialog'

const localDateTime = (unix: number) => unix > 0 ? (() => { const date = new Date(unix * 1000); return `${date.getFullYear()}-${String(date.getMonth()+1).padStart(2,'0')}-${String(date.getDate()).padStart(2,'0')}T${String(date.getHours()).padStart(2,'0')}:${String(date.getMinutes()).padStart(2,'0')}:${String(date.getSeconds()).padStart(2,'0')}` })() : ''
const unixDateTime = (value: string) => value ? Math.floor(new Date(value).getTime() / 1000) : 0

export function TaskPropertiesDialog({ taskId }: { taskId: string }) {
  const t = useT()
  const categoriesPref = useAgent((state) => state.preferences.values[CUSTOM_CATEGORIES_PREF_KEY], undefined)
  const categories = categoriesFromPreference(categoriesPref).filter((category) => category.builtinType !== 'all' && category.builtinType !== 'other')
  const [snapshot, setSnapshot] = useState<TaskEditSnapshot | null>(null)
  const [values, setValues] = useState<TaskEditValues | null>(null)
  const [error, setError] = useState('')
  const [pending, setPending] = useState(false)
  const [headersText, setHeadersText] = useState('')
  const [headersInvalid, setHeadersInvalid] = useState(false)
  const [pickerOpen, setPickerOpen] = useState(false)
  const [tagsText, setTagsText] = useState('')
  const [scheduleText, setScheduleText] = useState('')
  useEffect(() => { let active = true; void rpc.daemon.task.editInfo({ taskId }).then((result) => { if (active) { setSnapshot(result); setValues(taskEditValues(result)); setHeadersText(JSON.stringify(result.headers, null, 2)); setTagsText(result.properties.tags.join(', ')); setScheduleText(localDateTime(result.properties.scheduledAt)) } }).catch((err) => { if (active) setError(rpcErrorText(err, t)) }); return () => { active = false } }, [taskId, t])
  const update = (patch: Partial<TaskEditValues>) => setValues((current) => current ? { ...current, ...patch } : current)
  const close = () => closeTaskEdit()
  const save = async () => {
    if (!snapshot || !values || pending) return
    if (headersInvalid) { setError(t('taskEditHeaders')); return }
    const scheduledAt = unixDateTime(scheduleText)
    if (!Number.isFinite(scheduledAt)) { setError(t('taskEditScheduledAt')); return }
    const patch = taskEditPatch(snapshot, { ...values, scheduledAt, tags: tagsText.split(/[\n,]/).map((tag) => tag.trim()).filter(Boolean) })
    if (!hasTaskEditChanges(patch)) { close(); return }
    setPending(true); setError('')
    try { await rpc.daemon.task.edit(patch as EditTaskParams); close() } catch (err) { setError(rpcErrorText(err, t)); setPending(false) }
  }
  const field = (key: string, label: string, value: string, onChange: (value: string) => void, disabled = false, type: 'text' | 'datetime-local' = 'text') => <FormField key={key} label={label} htmlFor={key}><Input id={key} type={type} step={type === 'datetime-local' ? 1 : undefined} value={value} disabled={disabled || pending} onChange={(event) => onChange(event.target.value)} /></FormField>
  return <><Dialog open onOpenChange={(open) => !open && !pending && close()} size="lg" title={t('taskEditTitle')} footer={<ConfirmFooter okLabel={t('taskEditSave')} loading={pending} okDisabled={!snapshot || !values} onCancel={close} onOk={() => void save()} />}>
    {!snapshot || !values ? <div className="p-4">{error || t('loading')}</div> : <Form onSubmit={(event) => { event.preventDefault(); void save() }}>
      <FormField label={t('taskEditUrl')} htmlFor="edit-url"><div className="flex gap-2"><Input id="edit-url" value={snapshot.url} disabled className="min-w-0 flex-1" /><Button type="button" onClick={() => void copyText(snapshot.url)}>{t('copyUrl')}</Button></div></FormField>
      {field('edit-save-dir', t('saveDir'), values.saveDir, (saveDir) => update({ saveDir }), !snapshot.capabilities.storage)}
      <Button disabled={!snapshot.capabilities.storage || pending} onClick={() => setPickerOpen(true)}>{t('taskEditSelectDirectory')}</Button>
      {field('edit-file-name', t('colFileName'), values.fileName, (fileName) => update({ fileName }), !snapshot.capabilities.rename)}
      {field('edit-proxy', t('taskProxy'), values.proxyUrl, (proxyUrl) => update({ proxyUrl }), !snapshot.capabilities.proxy)}
      {field('edit-limit', t('taskEditDownloadLimit'), String(values.downloadLimitKib), (value) => { const amount = Number(value); if (/^\d*$/.test(value) && Number.isSafeInteger(amount) && amount <= Math.floor(Number.MAX_SAFE_INTEGER / 1024)) update({ downloadLimitKib: amount }) })}
      {field('edit-segments', t('configuredThreads'), String(values.segments), (value) => { const amount = parseTaskSegments(value); if (amount !== null) update({ segments: amount }) }, !snapshot.capabilities.segments)}
      {field('edit-agent', t('taskEditUserAgent'), values.userAgent, (userAgent) => update({ userAgent }), !snapshot.capabilities.request)}
      <FormField label={t('taskEditHeaders')} htmlFor="edit-headers"><Textarea id="edit-headers" disabled={!snapshot.capabilities.request || pending} value={headersText} onChange={(event) => { const text = event.target.value; setHeadersText(text); try { const parsed: unknown = JSON.parse(text); if (parsed && typeof parsed === 'object' && !Array.isArray(parsed) && Object.values(parsed).every((value) => typeof value === 'string')) { update({ headers: parsed as Record<string, string> }); setHeadersInvalid(false) } else setHeadersInvalid(true) } catch { setHeadersInvalid(true) } }} /></FormField>
      <FormField label={t('taskEditCookies')} htmlFor="edit-cookies"><div className="flex gap-2"><Input id="edit-cookies" disabled={!snapshot.capabilities.request || pending} type="password" value={values.cookies} autoComplete="new-password" onChange={(event) => update({ cookies: event.target.value, cookiesEdited: true })} placeholder={snapshot.cookiesConfigured && !values.cookiesEdited ? '••••••••' : ''} />{snapshot.cookiesConfigured && snapshot.capabilities.request ? <Button type="button" disabled={pending || (values.cookiesEdited && !values.cookies)} onClick={() => update({ cookies: '', cookiesEdited: true })}>{t('taskEditClearCookies')}</Button> : null}</div></FormField>
      {field('edit-auth', t('taskEditAuthRef'), values.authRef, (authRef) => update({ authRef }), !snapshot.capabilities.request)}
      <FormField label={t('taskEditCategory')} htmlFor="edit-category"><select id="edit-category" className="w-full rounded-md border border-input bg-background px-3 py-2" value={values.category} onChange={(event) => { const category = categories.find((item) => item.id === event.target.value); update({ category: event.target.value, ...(snapshot.capabilities.storage && category?.saveDir ? { saveDir: category.saveDir } : {}) }) }}><option value="">{t('saveDirCategoryAuto')}</option>{categories.map((category) => <option key={category.id} value={category.id}>{categoryLabel(t, category)}</option>)}</select></FormField>
      {field('edit-tags', t('taskEditTags'), tagsText, setTagsText)}
      {field('edit-schedule', t('taskEditScheduledAt'), scheduleText, setScheduleText, !snapshot.capabilities.schedule, 'datetime-local')}
      <p className="text-sm text-muted-foreground">{t('taskEditConnectionWarning')}</p>
      {!snapshot.capabilities.request || !snapshot.capabilities.proxy || !snapshot.capabilities.storage || !snapshot.capabilities.rename || !snapshot.capabilities.segments || !snapshot.capabilities.schedule ? <p className="text-sm text-muted-foreground">{t('taskEditUnsupported')}</p> : null}

      {error ? <FieldError className="break-all">{error}</FieldError> : null}
    </Form>}
  </Dialog><FsPickerDialog open={pickerOpen} onOpenChange={setPickerOpen} initialPath={values?.saveDir ?? ''} onSelect={(saveDir) => update({ saveDir })} title={t('taskEditSelectDirectory')} /></>
}
