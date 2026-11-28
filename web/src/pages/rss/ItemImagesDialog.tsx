import { useEffect, useState } from 'react'
import { useT } from '../../i18n'
import { fetchRssImage, RssImageProxyUnverifiableError } from '../../lib/rpc/http'
import type { RssItemDto } from '../../lib/rpc'
import { Dialog, Spinner } from '../../ui'

const MAX_IMAGES = 4

type ImageState =
  | { status: 'loading' }
  | { status: 'failed'; message: 'rssImageLoadFailed' | 'rssImageProxyUnverifiable' }
  | { status: 'ready'; url: string }

function ItemImage({ sourceId, guid, index }: { sourceId: string; guid: string; index: number }) {
  const t = useT()
  const [state, setState] = useState<ImageState>({ status: 'loading' })

  useEffect(() => {
    const controller = new AbortController()
    let objectUrl: string | undefined
    void fetchRssImage(sourceId, guid, index, controller.signal)
      .then((blob) => {
        if (controller.signal.aborted) return
        objectUrl = URL.createObjectURL(blob)
        setState({ status: 'ready', url: objectUrl })
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted) {
          const message = error instanceof RssImageProxyUnverifiableError
            ? 'rssImageProxyUnverifiable'
            : 'rssImageLoadFailed'
          setState({ status: 'failed', message })
        }
      })
    return () => {
      controller.abort()
      if (objectUrl) URL.revokeObjectURL(objectUrl)
    }
  }, [sourceId, guid, index])

  if (state.status === 'ready') {
    return (
      <img
        className="mx-auto max-h-[65dvh] max-w-full rounded-md object-contain"
        src={state.url}
        alt=""
        onError={() => setState({ status: 'failed', message: 'rssImageLoadFailed' })}
      />
    )
  }
  return (
    <div role="status" className="flex min-h-16 items-center justify-center gap-2 rounded-md bg-muted px-3 py-2 text-sm text-muted-foreground">
      {state.status === 'loading' ? <Spinner size="md" /> : null}
      {t(state.status === 'loading' ? 'rssImageLoading' : state.message)}
    </div>
  )
}

export function ItemImagesDialog({ item, onClose }: { item: RssItemDto; onClose: () => void }) {
  const t = useT()
  const imageCount = Math.min(item.imageCount ?? 0, MAX_IMAGES)
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
      title={t('rssViewImages')}
      description={<span dir="auto" className="block truncate whitespace-pre" title={item.title}>{item.title}</span>}
      size="lg"
    >
      <div className="flex flex-col gap-3 pb-safe">
        {Array.from({ length: imageCount }, (_, index) => (
          <ItemImage key={index} sourceId={item.sourceId} guid={item.guid} index={index} />
        ))}
      </div>
    </Dialog>
  )
}
