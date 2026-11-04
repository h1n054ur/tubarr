import type { ReactNode } from 'react'
import { createRootRoute, createRoute, createRouter, type RouterHistory } from '@tanstack/react-router'
import { Layout } from './components/Layout'
import { ActivityPage } from './routes/ActivityPage'
import { DraftsPage } from './routes/DraftsPage'
import { HealthPage } from './routes/HealthPage'
import { MappingsPage } from './routes/MappingsPage'
import { SettingsPage } from './routes/SettingsPage'
import { UnmatchedPage } from './routes/UnmatchedPage'

const rootRoute = createRootRoute({ component: Layout })

const page = (path: string, component: () => ReactNode) =>
  createRoute({ getParentRoute: () => rootRoute, path, component })

const routeTree = rootRoute.addChildren([
  page('/', MappingsPage),
  page('/unmatched', UnmatchedPage),
  page('/drafts', DraftsPage),
  page('/activity', ActivityPage),
  page('/health', HealthPage),
  page('/settings', SettingsPage),
])

/** Made once the browser is signed in, so it starts from the final URL (after a login's `next`). */
export function makeRouter(history: RouterHistory) {
  return createRouter({ routeTree, history })
}

export type AppRouter = ReturnType<typeof makeRouter>

declare module '@tanstack/react-router' {
  interface Register {
    router: AppRouter
  }
}
