// TanStack Router（history 模式）。
//   /login /setup            —— 无外壳（AuthLayout）
//   壳路由 `shell`（需登录）  —— AppShell：`/` 下载、`/rss`、`/webhooks`、`/plugins/$pluginId/$viewId`、`/settings[/$category]`
// 未登录访问壳内路由 → /login；已登录访问 /login → /。

import { Outlet, createRootRoute, createRoute, createRouter, redirect } from '@tanstack/react-router'
import { isAuthenticated } from './lib/access'
import { LoginPage } from './pages/auth/LoginPage'
import { SetupPage } from './pages/auth/SetupPage'
import { DownloadsPage } from './pages/downloads/DownloadsPage'
import { PluginViewPage } from './pages/plugins/PluginViewPage'
import { RssPage } from './pages/rss/RssPage'
import { SettingsCategoryRoute, SettingsIndex, SettingsLayout } from './pages/settings/SettingsLayout'
import { WebhooksPage } from './pages/webhooks/WebhooksPage'
import { AppShell } from './shell'

const rootRoute = createRootRoute({ component: Outlet })

const loginRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/login',
  component: LoginPage,
  beforeLoad: () => {
    if (isAuthenticated()) throw redirect({ to: '/' })
  },
})

const setupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/setup',
  component: SetupPage,
  beforeLoad: () => {
    if (isAuthenticated()) throw redirect({ to: '/' })
  },
})

const shellRoute = createRoute({
  getParentRoute: () => rootRoute,
  id: 'shell',
  component: AppShell,
  beforeLoad: () => {
    if (!isAuthenticated()) throw redirect({ to: '/login' })
  },
})

const downloadsRoute = createRoute({ getParentRoute: () => shellRoute, path: '/', component: DownloadsPage })
const rssRoute = createRoute({ getParentRoute: () => shellRoute, path: '/rss', component: RssPage })
const webhooksRoute = createRoute({ getParentRoute: () => shellRoute, path: '/webhooks', component: WebhooksPage })

const pluginViewRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: '/plugins/$pluginId/$viewId',
  component: PluginViewPage,
})

const settingsRoute = createRoute({ getParentRoute: () => shellRoute, path: '/settings', component: SettingsLayout })
const settingsIndexRoute = createRoute({ getParentRoute: () => settingsRoute, path: '/', component: SettingsIndex })

const settingsCategoryRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: '$category',
  component: SettingsCategoryRoute,
})

const routeTree = rootRoute.addChildren([
  loginRoute,
  setupRoute,
  shellRoute.addChildren([
    downloadsRoute,
    rssRoute,
    webhooksRoute,
      pluginViewRoute,
    settingsRoute.addChildren([settingsIndexRoute, settingsCategoryRoute]),
  ]),
])

export const router = createRouter({ routeTree, defaultPreload: 'intent' })

declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router
  }
}
