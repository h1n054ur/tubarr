import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from '@tanstack/react-router'
import { render } from '@testing-library/react'
import type { ReactNode } from 'react'
import { vi } from 'vitest'

export function renderPage(page: ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  render(<QueryClientProvider client={client}>{page}</QueryClientProvider>)
}

/** A page at `path` inside a router, for pages that keep search, page and size in the URL. Returns
 * the history so a test can read the URL. */
export function renderRoutedPage(component: () => ReactNode, path = '/') {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  const root = createRootRoute()
  const route = createRoute({ getParentRoute: () => root, path: '/', component })
  const history = createMemoryHistory({ initialEntries: [path] })
  const router = createRouter({ routeTree: root.addChildren([route]), history })
  render(
    <QueryClientProvider client={client}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  )
  return history
}

export interface Call {
  url: string
  method: string
  body: unknown
  headers: Record<string, string>
  credentials: RequestCredentials | undefined
}

/** A fetch stub answering by "METHOD path" (query string included) and recording each call. A route
 * may return a Response for a status other than 200, or a promise to hold the answer back. */
export function mockApi(routes: Record<string, unknown | ((call: Call) => unknown)>) {
  const calls: Call[] = []
  const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
    const call: Call = {
      url,
      method: init?.method ?? 'GET',
      body: init?.body ? JSON.parse(String(init.body)) : undefined,
      headers: (init?.headers as Record<string, string>) ?? {},
      credentials: init?.credentials,
    }
    calls.push(call)
    const key = `${call.method} ${url}`
    if (!(key in routes)) return new Response(JSON.stringify({ error: `no route ${key}` }), { status: 404 })
    const route = routes[key]
    const body = typeof route === 'function' ? await route(call) : route
    if (body instanceof Response) return body
    if (body === null) return new Response(null, { status: 204 })
    return new Response(JSON.stringify(body), { status: 200 })
  })
  vi.stubGlobal('fetch', fetchMock)
  return calls
}

/** A JSON error answer, as the server sends them. */
export function errorResponse(status: number, error: string, headers: Record<string, string> = {}) {
  return () => new Response(JSON.stringify({ error }), { status, headers })
}
