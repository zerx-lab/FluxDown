import { expect, test } from 'bun:test'
import { closeAddWebSeed, getDialogsState, openAddWebSeed } from './store'

test('a late success cannot close a reopened dialog for the same torrent', async () => {
  const taskId = 'torrent-stale-close'
  openAddWebSeed(taskId)
  try {
    const original = getDialogsState().addWebSeed
    if (!original) throw new Error('Expected the original dialog to open')
    const oldSuccess = Promise.resolve().then(() => closeAddWebSeed(original.session))

    closeAddWebSeed(original.session)
    expect(getDialogsState().addWebSeed).toBeNull()
    openAddWebSeed(taskId)
    const reopened = getDialogsState().addWebSeed
    if (!reopened) throw new Error('Expected the replacement dialog to open')

    await oldSuccess
    expect(getDialogsState().addWebSeed).toEqual(reopened)

    closeAddWebSeed(reopened.session)
    expect(getDialogsState().addWebSeed).toBeNull()
  } finally {
    const active = getDialogsState().addWebSeed
    if (active?.taskId === taskId) closeAddWebSeed(active.session)
  }
})
