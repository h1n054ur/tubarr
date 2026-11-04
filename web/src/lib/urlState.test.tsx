import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from '@tanstack/react-router'
import { act, render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import {
  oneOf,
  readFilters,
  readListParams,
  useFilters,
  useListParams,
  writeFilters,
  writeListParams,
  type FilterParam,
} from './urlState'

describe('readListParams', () => {
  it('falls back to the defaults for missing or bad values', () => {
    expect(readListParams({})).toEqual({ q: '', page: 1, size: 25 })
    expect(readListParams({ page: 0, size: 30 })).toEqual({ q: '', page: 1, size: 25 })
    expect(readListParams({ page: 'x', size: 'big' })).toEqual({ q: '', page: 1, size: 25 })
    expect(readListParams({ page: 2.5 })).toEqual({ q: '', page: 1, size: 25 })
  })

  it('reads a number the router parsed as text again', () => {
    expect(readListParams({ q: 452001, page: 3, size: 100 })).toEqual({ q: '452001', page: 3, size: 100 })
  })

  it('reads a second list under its prefix only', () => {
    const search = { page: 4, events_page: 2, events_size: 50 }
    expect(readListParams(search, 'events_')).toEqual({ q: '', page: 2, size: 50 })
    expect(readListParams(search)).toEqual({ q: '', page: 4, size: 25 })
  })
})

describe('writeListParams', () => {
  it('drops defaults and keeps other parameters', () => {
    expect(writeListParams({ kind: 'show', page: 3 }, { q: '', page: 1, size: 25 })).toEqual({ kind: 'show' })
    expect(writeListParams({ kind: 'show' }, { q: 'rogan', size: 50 })).toEqual({ kind: 'show', q: 'rogan', size: 50 })
  })

  it('writes a second list under its prefix and leaves the first alone', () => {
    expect(writeListParams({ page: 3, events_page: 2 }, { page: 1 }, 'events_')).toEqual({ page: 3 })
    expect(writeListParams({ page: 3 }, { page: 2, size: 50 }, 'events_')).toEqual({
      page: 3,
      events_page: 2,
      events_size: 50,
    })
  })
})

function start(path: string) {
  let params: ReturnType<typeof useListParams> | undefined
  function Probe() {
    params = useListParams()
    return <p>{`${params.q}|${params.page}|${params.size}`}</p>
  }
  const root = createRootRoute()
  const list = createRoute({ getParentRoute: () => root, path: '/', component: Probe })
  const history = createMemoryHistory({ initialEntries: [path] })
  const router = createRouter({ routeTree: root.addChildren([list]), history })
  render(<RouterProvider router={router} />)
  return { history, get: () => params as ReturnType<typeof useListParams> }
}

describe('useListParams', () => {
  it('reads q, page and size from the URL', async () => {
    start('/?q=rogan&page=2&size=50&kind=show')
    expect(await screen.findByText('rogan|2|50')).toBeInTheDocument()
  })

  it('replaces the entry while typing and resets the page, keeping other parameters', async () => {
    const { history, get } = start('/?kind=show&page=3')
    await screen.findByText('|3|25')
    const before = history.length
    await act(() => get().setQ('rogan'))
    expect(await screen.findByText('rogan|1|25')).toBeInTheDocument()
    expect(history.location.search).toBe('?kind=show&q=rogan')
    expect(history.length).toBe(before)
  })

  it('adds an entry for a page change and goes back to page 1 on a new size', async () => {
    const { history, get } = start('/')
    await screen.findByText('|1|25')
    const before = history.length
    await act(() => get().setPage(4))
    expect(await screen.findByText('|4|25')).toBeInTheDocument()
    expect(history.length).toBe(before + 1)
    await act(() => get().setSize(100))
    expect(await screen.findByText('|1|100')).toBeInTheDocument()
    expect(history.location.search).toBe('?size=100')
  })
})

const spec = {
  kind: oneOf(['show', 'podcast', 'film']),
  show: oneOf(['open', 'all'], 'open'),
  mapping: { values: (v: string) => /^[1-9]\d*$/.test(v), default: '' } as FilterParam,
  events: oneOf(['play', 'search'], '', { resetsPage: false }),
}

describe('readFilters', () => {
  it('reads allowed values and falls back to the defaults for missing or bad ones', () => {
    expect(readFilters({}, spec)).toEqual({ kind: '', show: 'open', mapping: '', events: '' })
    expect(readFilters({ kind: 'podcast', show: 'all', mapping: 12, events: 'play' }, spec)).toEqual({
      kind: 'podcast',
      show: 'all',
      mapping: '12',
      events: 'play',
    })
    expect(readFilters({ kind: 'movie', show: true, mapping: 0, events: 'x' }, spec)).toEqual({
      kind: '',
      show: 'open',
      mapping: '',
      events: '',
    })
    expect(readFilters({ mapping: 'abc', kind: null }, spec)).toEqual({ kind: '', show: 'open', mapping: '', events: '' })
  })
})

describe('writeFilters', () => {
  it('drops defaults and bad values, writes ids as numbers and resets the page', () => {
    expect(writeFilters({ q: 'rogan', page: 3 }, spec, { kind: 'podcast' })).toEqual({ q: 'rogan', kind: 'podcast' })
    expect(writeFilters({ kind: 'film', show: 'all', page: 2 }, spec, { kind: '', show: 'open' })).toEqual({})
    expect(writeFilters({}, spec, { mapping: '12' })).toEqual({ mapping: 12 })
    expect(writeFilters({ kind: 'show' }, spec, { kind: 'bogus' as never })).toEqual({})
  })

  it('keeps the page for a filter that does not narrow the paged list', () => {
    expect(writeFilters({ page: 3 }, spec, { events: 'play' })).toEqual({ page: 3, events: 'play' })
  })

  it('resets the page parameter a filter names instead', () => {
    const second = { kind: oneOf(['play'], '', { resetsPage: 'events_page' }) }
    expect(writeFilters({ page: 3, events_page: 2 }, second, { kind: 'play' })).toEqual({ page: 3, kind: 'play' })
  })
})

describe('useFilters', () => {
  function startFilters(path: string) {
    let state: ReturnType<typeof useFilters<typeof spec>> | undefined
    function Probe() {
      state = useFilters(spec)
      return <p>{`${state.kind}|${state.show}|${state.mapping}|${state.events}`}</p>
    }
    const root = createRootRoute()
    const route = createRoute({ getParentRoute: () => root, path: '/', component: Probe })
    const history = createMemoryHistory({ initialEntries: [path] })
    const router = createRouter({ routeTree: root.addChildren([route]), history })
    render(<RouterProvider router={router} />)
    return { history, get: () => state as NonNullable<typeof state> }
  }

  it('reads the filters from the URL and ignores values it does not know', async () => {
    startFilters('/?kind=podcast&show=all&mapping=7&events=nope')
    expect(await screen.findByText('podcast|all|7|')).toBeInTheDocument()
  })

  it('writes a change to the URL, replacing the entry and going back to page 1', async () => {
    const { history, get } = startFilters('/?q=rogan&page=3')
    await screen.findByText('|open||')
    const before = history.length
    await act(() => get().set('mapping', '12'))
    expect(await screen.findByText('|open|12|')).toBeInTheDocument()
    expect(history.location.search).toBe('?q=rogan&mapping=12')
    expect(history.length).toBe(before)
    await act(() => get().set('show', 'all'))
    await act(() => get().set('mapping', ''))
    expect(await screen.findByText('|all||')).toBeInTheDocument()
    expect(history.location.search).toBe('?q=rogan&show=all')
  })
})
