import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { RouterProvider } from '@tanstack/react-router'
import './index.css'
import { router } from './router'
import { ConfirmDialog } from './components/dialogs/confirm-dialog'
import { ThemeProvider } from './lib/theme'
import { connectWs } from './lib/ws'
import { isAuthenticated, saveCredentials } from './lib/auth'

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 10_000,
      retry: 1,
      refetchOnWindowFocus: false,
    },
  },
})

// URL 携带 ?token=（可选 ?base=）时自动登录——用于演示站分享链接，
// 保存凭证后立即从地址栏抹除令牌，避免泄露到历史记录/截图。
const params = new URLSearchParams(window.location.search)
const urlToken = params.get('token')
if (urlToken) {
  saveCredentials(params.get('base') ?? '', urlToken, true)
  params.delete('token')
  params.delete('base')
  const qs = params.toString()
  window.history.replaceState(
    null,
    '',
    window.location.pathname + (qs ? `?${qs}` : '') + window.location.hash,
  )
}

// 已登录会话（刷新页面）直接建立 WS。
if (isAuthenticated()) connectWs(queryClient)

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <ThemeProvider>
        <RouterProvider router={router} />
        <ConfirmDialog />
      </ThemeProvider>
    </QueryClientProvider>
  </StrictMode>,
)
