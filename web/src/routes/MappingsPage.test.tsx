import { act, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { drama, theri } from '../test-data'
import type { Mapping } from '../lib/api'
import { mockApi, renderRoutedPage, type Call } from '../test-util'
import { MappingsPage } from './MappingsPage'

const page = (path = '/') => renderRoutedPage(MappingsPage, path)

const named = (title: string, id: number, extra: Partial<Mapping> = {}): Mapping => ({
  ...drama,
  id,
  title,
  year: null,
  tvdb_id: 500000 + id,
  ...extra,
})

const rogan = named('The Joe Rogan Experience', 101, { language: 'English', channel_ids: ['UCzQUP1qoWDoEbmsQxvdjxgQ'] })
const theo = named('This Past Weekend w/Theo Von', 102, { language: 'English' })

/** 60 shows, "Show 01" to "Show 60", in the server's order. */
const many = Array.from({ length: 60 }, (_, i) => named(`Show ${String(i + 1).padStart(2, '0')}`, 200 + i))

const titles = () =>
  screen
    .getAllByRole('row')
    .slice(1)
    .map((r) => within(r).getAllByRole('cell')[0].querySelector('div')?.textContent)

async function typeSearch(text: string) {
  const box = screen.getByLabelText('Search')
  await userEvent.clear(box)
  if (text) await userEvent.type(box, text)
}

afterEach(() => vi.unstubAllGlobals())

describe('MappingsPage', () => {
  it('lists mappings with the session cookie and filters by kind', async () => {
    const fetchMock = vi.fn(async (url: string) => {
      if (url.endsWith('/settings')) return new Response(JSON.stringify({ arr: { tag: 'youtube' } }), { status: 200 })
      const body = url.includes('kind=film') ? [theri] : [drama, theri]
      return new Response(JSON.stringify(body), { status: 200 })
    })
    vi.stubGlobal('fetch', fetchMock)
    page()
    expect(await screen.findByText('Kabhi Main Kabhi Tum (2024)')).toBeInTheDocument()
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit]
    expect(url).toBe('/admin/api/mappings')
    expect((init.headers as Record<string, string>)['X-Requested-With']).toBe('tubarr')
    expect(init.credentials).toBe('same-origin')

    await userEvent.selectOptions(screen.getByLabelText('Kind'), 'film')
    await waitFor(() => expect(screen.queryByText('Kabhi Main Kabhi Tum (2024)')).not.toBeInTheDocument())
    expect(fetchMock.mock.calls.at(-1)?.[0]).toBe('/admin/api/mappings?kind=film')

    await userEvent.selectOptions(screen.getByLabelText('Kind'), 'podcast')
    await waitFor(() => expect(fetchMock.mock.calls.at(-1)?.[0]).toBe('/admin/api/mappings?kind=podcast'))
  })

  it('shows the empty state', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response('[]', { status: 200 })))
    page()
    expect(await screen.findByText('No mappings')).toBeInTheDocument()
  })

  it('names the configured tag, opens the drawer from a row and saves rules and aliases', async () => {
    const calls = mockApi({
      'GET /admin/api/mappings': [drama, theri],
      'GET /admin/api/settings': { arr: { tag: 'youtube', sync_interval_secs: 900 } },
      'GET /admin/api/mappings/1/aliases': [
        { alias: 'KMKT', source: 'arr' },
        { alias: 'Kabhi Main', source: 'user' },
      ],
      'PUT /admin/api/mappings/1/aliases': (call: Call) =>
        (call.body as { aliases: string[] }).aliases.map((alias) => ({ alias, source: 'user' })),
      'PATCH /admin/api/mappings/1': (call: Call) => ({ ...drama, ...(call.body as object) }),
    })
    page()
    expect(await screen.findByText('youtube')).toBeInTheDocument()
    expect(screen.queryByText(/tubarr tag/i)).not.toBeInTheDocument()

    await userEvent.click(screen.getByText('Kabhi Main Kabhi Tum (2024)'))
    const dialog = await screen.findByRole('dialog')
    const language = within(dialog).getByLabelText('Language')
    await userEvent.clear(language)
    await userEvent.type(language, 'Punjabi')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }))
    const aliases = within(dialog).getByLabelText('Aliases')
    await waitFor(() => expect(aliases).toHaveValue('Kabhi Main'))
    await userEvent.type(aliases, '{enter}Kabhi Main Kabhi Tum OST')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Save' }))
    await waitFor(() =>
      expect(calls.find((c) => c.method === 'PATCH')?.body).toMatchObject({ language: 'Punjabi', root_folder: null }),
    )
    // The arr's alias goes back too, since the endpoint replaces the whole list.
    await waitFor(() =>
      expect(calls.find((c) => c.method === 'PUT')?.body).toEqual({
        aliases: ['KMKT', 'Kabhi Main', 'Kabhi Main Kabhi Tum OST'],
      }),
    )
  })

  it('narrows the list as you search, through typos and in any word order', async () => {
    mockApi({ 'GET /admin/api/mappings': [drama, theri, rogan, theo], 'GET /admin/api/settings': {} })
    page()
    expect(await screen.findByText('Kabhi Main Kabhi Tum (2024)')).toBeInTheDocument()
    expect(titles()).toHaveLength(4)

    await typeSearch('rogan')
    await waitFor(() => expect(titles()).toEqual(['The Joe Rogan Experience']))

    await typeSearch('joe rogn')
    await waitFor(() => expect(titles()).toEqual(['The Joe Rogan Experience']))

    await typeSearch('rogan joe')
    await waitFor(() => expect(titles()).toEqual(['The Joe Rogan Experience']))

    // An id matches exactly: a near id (theri, rogan) does not come along.
    await typeSearch('452001')
    await waitFor(() => expect(titles()).toEqual(['Kabhi Main Kabhi Tum (2024)']))

    await typeSearch('UCzQUP1qoWDoEbmsQxvdjxgQ')
    await waitFor(() => expect(titles()).toEqual(['The Joe Rogan Experience']))
  })

  it('shows a no match state with a way to clear the search', async () => {
    mockApi({ 'GET /admin/api/mappings': [drama, rogan], 'GET /admin/api/settings': {} })
    page('/?q=xyzzy')
    expect(await screen.findByText('No shows match "xyzzy".')).toBeInTheDocument()
    expect(screen.queryByRole('table')).not.toBeInTheDocument()
    const empty = screen.getByText('No matches').parentElement as HTMLElement
    // The X in the box keeps its own label, so the two buttons read differently.
    expect(screen.getByRole('button', { name: 'Clear search' })).toBeInTheDocument()
    expect(within(empty).queryByRole('button', { name: 'Clear search' })).not.toBeInTheDocument()
    await userEvent.click(within(empty).getByRole('button', { name: 'Show all' }))
    await waitFor(() => expect(titles()).toHaveLength(2))
    expect(screen.getByLabelText('Search')).toHaveValue('')
  })

  it('pages the list and changes the page size', async () => {
    mockApi({ 'GET /admin/api/mappings': many, 'GET /admin/api/settings': {} })
    const history = page()
    expect(await screen.findByText('Show 01')).toBeInTheDocument()
    expect(titles()).toHaveLength(25)
    expect(screen.getByText(/Showing/).textContent).toBe('Showing 1 to 25 of 60')

    await userEvent.click(screen.getByRole('button', { name: 'Page 3' }))
    await waitFor(() => expect(titles()).toEqual(many.slice(50).map((m) => m.title)))
    expect(history.location.search).toBe('?page=3')

    await userEvent.selectOptions(screen.getByLabelText('Per page'), '50')
    await waitFor(() => expect(titles()).toHaveLength(50))
    expect(titles()[0]).toBe('Show 01')
    expect(screen.getByText(/Showing/).textContent).toBe('Showing 1 to 50 of 60')
    expect(history.location.search).toBe('?size=50')
  })

  it('goes back to page 1 when a filter or the search changes', async () => {
    mockApi({
      'GET /admin/api/mappings': many,
      'GET /admin/api/mappings?active=true': many,
      'GET /admin/api/settings': {},
    })
    const history = page('/?page=2')
    expect(await screen.findByText('Show 26')).toBeInTheDocument()

    const before = history.length
    await userEvent.selectOptions(screen.getByLabelText('Status'), 'active')
    await waitFor(() => expect(history.location.search).toBe('?status=active'))
    expect(history.length).toBe(before)
    expect(await screen.findByText('Show 01')).toBeInTheDocument()

    await userEvent.click(screen.getByRole('button', { name: 'Page 2' }))
    await waitFor(() => expect(history.location.search).toBe('?status=active&page=2'))
    await typeSearch('show')
    await waitFor(() => expect(history.location.search).toBe('?status=active&q=show'))
  })

  it('keeps kind and status in the URL and restores them from a link and on back', async () => {
    const calls = mockApi({
      'GET /admin/api/mappings': [drama, theri, rogan, theo],
      'GET /admin/api/mappings?kind=podcast&active=true': [rogan, theo],
      'GET /admin/api/mappings?kind=film': [theri],
      'GET /admin/api/settings': {},
    })
    const history = page('/?kind=podcast&status=active&q=rogan')
    expect(await screen.findByText('The Joe Rogan Experience')).toBeInTheDocument()
    expect(titles()).toEqual(['The Joe Rogan Experience'])
    expect(screen.getByLabelText('Kind')).toHaveValue('podcast')
    expect(screen.getByLabelText('Status')).toHaveValue('active')
    expect(screen.getByLabelText('Search')).toHaveValue('rogan')
    expect(calls.some((c) => c.url === '/admin/api/mappings?kind=podcast&active=true')).toBe(true)

    await userEvent.selectOptions(screen.getByLabelText('Kind'), 'film')
    await userEvent.selectOptions(screen.getByLabelText('Status'), '')
    await waitFor(() => expect(history.location.search).toBe('?kind=film&q=rogan'))

    act(() => history.push('/?kind=film'))
    await waitFor(() => expect(titles()).toEqual([expect.stringContaining('Theri')]))
    expect(screen.getByLabelText('Search')).toHaveValue('')
    act(() => history.back())
    await waitFor(() => expect(screen.getByLabelText('Kind')).toHaveValue('film'))
    expect(screen.getByLabelText('Search')).toHaveValue('rogan')
  })

  it('ignores a kind or status it does not know', async () => {
    const calls = mockApi({ 'GET /admin/api/mappings': [drama, theri], 'GET /admin/api/settings': {} })
    page('/?kind=cartoon&status=maybe')
    expect(await screen.findByText('Kabhi Main Kabhi Tum (2024)')).toBeInTheDocument()
    expect(screen.getByLabelText('Kind')).toHaveValue('')
    expect(screen.getByLabelText('Status')).toHaveValue('')
    expect(calls.filter((c) => c.url.startsWith('/admin/api/mappings')).map((c) => c.url)).toEqual(['/admin/api/mappings'])
  })

  it('keeps the search in the URL both ways', async () => {
    mockApi({ 'GET /admin/api/mappings': [drama, theri, rogan, theo], 'GET /admin/api/settings': {} })
    const history = page('/?q=theo')
    expect(await screen.findByText('This Past Weekend w/Theo Von')).toBeInTheDocument()
    expect(screen.getByLabelText('Search')).toHaveValue('theo')
    expect(titles()[0]).toBe('This Past Weekend w/Theo Von')

    await typeSearch('rogan')
    await waitFor(() => expect(history.location.search).toBe('?q=rogan'))

    act(() => history.push('/?q=kabhi'))
    await waitFor(() => expect(screen.getByLabelText('Search')).toHaveValue('kabhi'))
    expect(titles()).toEqual(['Kabhi Main Kabhi Tum (2024)'])
  })
})
