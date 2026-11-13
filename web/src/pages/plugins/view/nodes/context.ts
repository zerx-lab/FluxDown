import { createContext, useContext } from 'react'
import type { ViewEventDto } from '../../../../lib/rpc'

export interface ViewRenderContext {
  /** 向 daemon 投递视图事件。 */
  send: (node: string, event: ViewEventDto) => void
}

export const ViewRenderContext = createContext<ViewRenderContext>({ send: () => undefined })

export const useViewRender = (): ViewRenderContext => useContext(ViewRenderContext)
