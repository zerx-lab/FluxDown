import { useEffect, useState, type CSSProperties } from 'react'
import { cn } from '../../../../lib/cn'
import { fetchPluginImage } from '../../../../lib/rpc/http'
import type { ViewNode } from '../../../../lib/rpc'
import { ProgressBar, Spinner } from '../../../../ui'
import { splitMarkdown } from '../markdownLite'
import { toneBackground, toneColor } from '../tones'
import { useViewRender } from './context'
import { bool, num, optStr, safeHref, str } from './props'

const imageResources = new Map<string, { refs: number; bytes: number; controller: AbortController; promise: Promise<string> }>()
const MAX_ACTIVE_IMAGES = 128
const MAX_ACTIVE_IMAGE_BYTES = 16 * 1024 * 1024
let activeImageBytes = 0

export function acquireImage(session: string, id: string): { promise: Promise<string>; release: () => void } {
  const key = `${session}/${id}`
  let entry = imageResources.get(key)
  if (!entry && imageResources.size >= MAX_ACTIVE_IMAGES) {
    return { promise: Promise.reject(new Error('image resource limit reached')), release: () => undefined }
  }
  if (!entry) {
    const controller = new AbortController()
    const created = { refs: 0, bytes: 0, controller, promise: Promise.resolve('') }
    created.promise = fetchPluginImage(session, id, controller.signal).then((blob) => {
      if (activeImageBytes + blob.size > MAX_ACTIVE_IMAGE_BYTES) throw new Error('plugin image cache limit reached')
      created.bytes = blob.size
      activeImageBytes += blob.size
      return URL.createObjectURL(blob)
    })
    entry = created
    imageResources.set(key, entry)
    void entry.promise.catch(() => {
      if (imageResources.get(key) === entry) imageResources.delete(key)
    })
  }
  entry.refs += 1
  const acquired = entry
  return {
    promise: acquired.promise,
    release: () => {
      acquired.refs -= 1
      if (acquired.refs === 0 && imageResources.get(key) === acquired) {
        imageResources.delete(key)
        acquired.controller.abort()
        void acquired.promise.then((url) => {
          URL.revokeObjectURL(url)
          activeImageBytes -= acquired.bytes
          acquired.bytes = 0
        }, () => undefined)
      }
    },
  }
}

const SIZE: Record<string, string> = { xs: 'text-xs', sm: 'text-sm', md: 'text-md', lg: 'text-lg', xl: 'text-xl' }
const WEIGHT: Record<string, string> = { normal: 'font-normal', medium: 'font-medium', semibold: 'font-semibold' }

export function TextNode({ node }: { node: ViewNode }) {
  const p = node.p
  const style: CSSProperties = { color: toneColor(str(p, 'tone', 'neutral')) }
  return (
    <span
      className={cn(
        SIZE[str(p, 'size', 'sm')] ?? 'text-sm',
        WEIGHT[str(p, 'weight', 'normal')] ?? 'font-normal',
        bool(p, 'mono') && 'font-mono',
        bool(p, 'truncate') ? 'block min-w-0 truncate' : 'whitespace-pre-wrap break-words',
      )}
      style={style}
    >
      {str(p, 'text')}
    </span>
  )
}

export function HeadingNode({ node }: { node: ViewNode }) {
  const level = num(node.p, 'level') ?? 2
  const text = str(node.p, 'text')
  if (level === 1) return <h1 className="text-xl font-semibold text-foreground">{text}</h1>
  if (level === 3) return <h3 className="text-md font-semibold text-foreground">{text}</h3>
  return <h2 className="text-lg font-semibold text-foreground">{text}</h2>
}

function CodeBlock({ text }: { text: string }) {
  return <pre className="overflow-auto rounded-md bg-muted px-3 py-2 font-mono text-xs text-foreground">{text}</pre>
}

export function MarkdownNode({ node }: { node: ViewNode }) {
  const blocks = splitMarkdown(str(node.p, 'text'))
  return (
    <div className="flex flex-col gap-2 text-sm text-foreground">
      {blocks.map((block, index) =>
        block.type === 'code' ? (
          <CodeBlock key={index} text={block.text} />
        ) : (
          <p key={index} className="whitespace-pre-wrap break-words">
            {block.text}
          </p>
        ),
      )}
    </div>
  )
}

export function CodeNode({ node }: { node: ViewNode }) {
  return <CodeBlock text={str(node.p, 'text')} />
}

export function LinkNode({ node }: { node: ViewNode }) {
  const href = safeHref(str(node.p, 'href'))
  const text = str(node.p, 'text')
  if (href === null) return <span className="text-sm text-foreground">{text}</span>
  return (
    <a href={href} target="_blank" rel="noopener noreferrer" className="text-sm text-accent-text underline-offset-2 hover:underline">
      {text}
    </a>
  )
}

function RemoteImageNode({ session, id, alt, width, height }: { session: string; id: string; alt: string; width?: number; height?: number }) {
  const [resource, setResource] = useState<{ key: string; url: string | null }>({ key: '', url: null })
  useEffect(() => {
    const key = `${session}/${id}`
    let current = true
    const acquired = acquireImage(session, id)
    setResource({ key, url: null })
    void acquired.promise.then(
      (url) => { if (current) setResource({ key, url }) },
      () => { if (current) setResource({ key, url: null }) },
    )
    return () => {
      current = false
      acquired.release()
    }
  }, [session, id])
  const key = `${session}/${id}`
  const url = resource.key === key ? resource.url : null
  const style: CSSProperties = { objectFit: 'contain', maxWidth: '100%' }
  return url
    ? <img src={url} alt={alt} width={width} height={height} style={style} />
    : <span role="img" aria-label={alt}>{alt}</span>
}

export function ImageNode({ node }: { node: ViewNode }) {
  const src = str(node.p, 'src')
  const alt = str(node.p, 'alt')
  const width = num(node.p, 'width')
  const height = num(node.p, 'height')
  const { sessionId } = useViewRender()
  if (/^data:image\/(png|jpeg|webp);base64,/.test(src)) {
    return <img src={src} alt={alt} width={width} height={height} className="max-w-full object-contain" />
  }
  const resource = /^flux-image:([0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12})$/.exec(src)
  if (!resource || !sessionId) return null
  return <RemoteImageNode session={sessionId} id={resource[1]} alt={alt} width={width} height={height} />
}

export function ToneBadge({ text, tone }: { text: string; tone: unknown }) {
  return (
    <span
      className="inline-flex items-center rounded-full px-2 py-0.5 text-caption font-medium tabular"
      style={{ color: toneColor(tone), backgroundColor: toneBackground(tone) }}
    >
      {text}
    </span>
  )
}

export function BadgeNode({ node }: { node: ViewNode }) {
  return <ToneBadge text={str(node.p, 'text')} tone={str(node.p, 'tone', 'neutral')} />
}

export function ProgressNode({ node }: { node: ViewNode }) {
  const raw = node.p.value
  const value = typeof raw === 'number' ? raw : null
  const label = optStr(node.p, 'label')
  return (
    <div className="flex flex-col gap-1">
      {label ? <div className="text-xs text-muted-foreground">{label}</div> : null}
      <ProgressBar value={value} />
    </div>
  )
}

export function SpinnerNode({ node }: { node: ViewNode }) {
  const label = optStr(node.p, 'label')
  return (
    <div className="flex items-center gap-2 text-sm text-muted-foreground">
      <Spinner size="md" />
      {label ? <span>{label}</span> : null}
    </div>
  )
}
