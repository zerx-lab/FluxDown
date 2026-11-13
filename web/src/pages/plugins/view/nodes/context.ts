import { createContext, useContext } from 'react'
import type { ViewEventDto } from '../../../../lib/rpc'

export interface ViewRenderContext {
  /** Current daemon-owned view session; images are scoped to this exact session. */
  sessionId: string | null
  /** 向 daemon 投递视图事件。 */
  send: (node: string, event: ViewEventDto) => void
}

export const ViewRenderContext = createContext<ViewRenderContext>({ sessionId: null, send: () => undefined })
export const useViewRender = (): ViewRenderContext => useContext(ViewRenderContext)
