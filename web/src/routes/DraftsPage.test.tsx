import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { DraftList, DraftSummary, StoredDraft } from '../lib/api'
import { mockApi, renderRoutedPage } from '../test-util'
import { DraftsPage, keysHint } from './DraftsPage'

afterEach(() => vi.unstubAllGlobals())

const ready = { tvdb: { ready: true }, tmdb: { ready: true } }

const show: DraftSummary = {
  id: 1,
  kind: 'show',
  title: 'Dil Ka Kya Karein',
  verdict: 'probably_missing',
  episodes: 8,
  created_at: 1_761_000_000,
}
const film: DraftSummary = {
  id: 2,
  kind: 'film',
  title: 'Tich Button',
  verdict: 'possible',
  episodes: null,
  created_at: 1_761_000_100,
}

const page = (items: DraftSummary[], total = items.length, keys = ready): DraftList => ({ items, total, keys })

const many = (from: number, n: number): DraftSummary[] =>
  Array.from({ length: n }, (_, i) => ({ ...film, id: from + i, title: `Film ${from + i}` }))

const stored: StoredDraft = {
  id: 2,
  kind: 'film',
  title: 'Tich Button',
  request: { video_id: 'fIlm0000001' },
  draft: {
    title: 'Tich Button',
    original_title: null,
    year: 2022,
    year_from_upload: false,
    runtime_mins: 128,
    language: 'Urdu',
    country: 'Pakistan',
    network: 'ARY Digital',
    youtube_url: 'https://www.youtube.com/watch?v=fIlm0000001',
    tv_film: false,
    part: null,
    check: { kind: 'film', title: 'Tich Button', verdict: 'possible', best: null, sources: [] },
  },
  text: 'Maybe on TMDB, check first\nTitle: Tich Button',
  created_at: 1_761_000_100,
}

describe('DraftsPage', () => {
  it('lists drafts with their check, searches and filters by kind on the server', async () => {
    const calls = mockApi({
      'GET /admin/api/drafts?limit=25&offset=0': page([show, film]),
      'GET /admin/api/drafts?q=tich&limit=25&offset=0': page([film]),
      'GET /admin/api/drafts?kind=show&q=tich&limit=25&offset=0': page([]),
    })
    const history = renderRoutedPage(DraftsPage)
    expect(await screen.findByText('Dil Ka Kya Karein')).toBeInTheDocument()
    // In its column and again in the phone summary.
    expect(screen.getAllByText('probably missing')).toHaveLength(2)
    expect(screen.getAllByText('maybe there')).toHaveLength(2)
    expect(screen.getByText('8 episodes')).toBeInTheDocument()
    expect(screen.queryByText('Checks need keys')).not.toBeInTheDocument()

    await userEvent.type(screen.getByLabelText('Search'), 'tich')
    await waitFor(() => expect(calls.at(-1)?.url).toBe('/admin/api/drafts?q=tich&limit=25&offset=0'))
    expect(await screen.findByText('Tich Button')).toBeInTheDocument()
    expect(screen.queryByText('Dil Ka Kya Karein')).not.toBeInTheDocument()

    await userEvent.selectOptions(screen.getByLabelText('Kind'), 'show')
    expect(await screen.findByText('No drafts match "tich".')).toBeInTheDocument()
    expect(history.location.search).toBe('?q=tich&kind=show')
    await userEvent.click(screen.getByRole('button', { name: 'Show all' }))
    expect(await screen.findByText('Dil Ka Kya Karein')).toBeInTheDocument()
  })

  it('pages on the server', async () => {
    const calls = mockApi({
      'GET /admin/api/drafts?limit=25&offset=0': page(many(1, 25), 30),
      'GET /admin/api/drafts?limit=25&offset=25': page(many(26, 5), 30),
    })
    const history = renderRoutedPage(DraftsPage)
    expect(await screen.findByText('Film 1')).toBeInTheDocument()
    expect(screen.getByText(/Showing/).textContent).toBe('Showing 1 to 25 of 30')
    await userEvent.click(screen.getByRole('button', { name: 'Page 2' }))
    expect(await screen.findByText('Film 26')).toBeInTheDocument()
    expect(calls.at(-1)?.url).toBe('/admin/api/drafts?limit=25&offset=25')
    expect(history.location.search).toBe('?page=2')
  })

  it('keeps the description full width and the filters and buttons under it', async () => {
    mockApi({ 'GET /admin/api/drafts?limit=25&offset=0': page(many(1, 1), 1) })
    renderRoutedPage(DraftsPage)
    await screen.findByText('Film 1')
    const text = screen.getByText(/Sonarr only adds shows/)
    const filters = screen.getByLabelText('Search').closest('div.mt-4') as HTMLElement
    expect(filters.parentElement).toBe(text.parentElement)
    expect(text.parentElement?.className ?? '').not.toMatch(/flex/)
    expect(filters.className).not.toMatch(/shrink-0|nowrap/)
    expect(within(filters).getByRole('button', { name: 'New draft' })).toBeInTheDocument()
  })

  it('explains drafts and the keys when there are none', async () => {
    mockApi({
      'GET /admin/api/drafts?limit=25&offset=0': page([], 0, {
        tvdb: { ready: false, reason: 'no TVDB key configured' },
        tmdb: { ready: false, reason: 'no TMDB key configured' },
      } as DraftList['keys']),
    })
    renderRoutedPage(DraftsPage)
    expect(await screen.findByText('No drafts yet')).toBeInTheDocument()
    expect(screen.getByText(/\[drafts\] section of the config/)).toBeInTheDocument()
    expect(screen.getByText(keysHint)).toBeInTheDocument()
    expect(screen.queryByRole('link')).not.toBeInTheDocument()
  })

  it('says which key is missing above the list', async () => {
    mockApi({
      'GET /admin/api/drafts?limit=25&offset=0': page([show], 1, {
        tvdb: { ready: true },
        tmdb: { ready: false, reason: 'no TMDB key configured' },
      } as DraftList['keys']),
    })
    renderRoutedPage(DraftsPage)
    const note = await screen.findByRole('status')
    expect(note).toHaveTextContent('No TMDB key is set')
    expect(note).toHaveTextContent(keysHint)
    expect(within(note).queryByRole('link')).not.toBeInTheDocument()
  })

  it('checks a title without storing anything', async () => {
    const calls = mockApi({
      'GET /admin/api/drafts?limit=25&offset=0': page([show]),
      'POST /admin/api/drafts/check': {
        kind: 'film',
        title: 'Uunchai',
        verdict: 'not_checked',
        best: null,
        sources: [
          { source: 'radarr', state: 'not_checked', reason: 'Radarr is not connected', candidates: [] },
          { source: 'tmdb_movie', state: 'not_checked', reason: 'no TMDB key configured', candidates: [] },
        ],
        summary: 'Not checked against TMDB: Radarr is not connected and no TMDB key is set',
      },
    })
    renderRoutedPage(DraftsPage)
    await screen.findByText('Dil Ka Kya Karein')
    await userEvent.click(screen.getByRole('button', { name: 'Check' }))
    const dialog = await screen.findByRole('dialog')
    await userEvent.selectOptions(within(dialog).getByLabelText('Kind'), 'film')
    await userEvent.type(within(dialog).getByLabelText('Title'), 'Uunchai')
    await userEvent.type(within(dialog).getByLabelText('Year'), '2022')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Check' }))
    expect(await within(dialog).findByText(/Not checked against TMDB/)).toBeInTheDocument()
    expect(within(dialog).getByText('no TMDB key configured')).toBeInTheDocument()
    expect(calls.find((c) => c.method === 'POST')?.body).toEqual({ kind: 'film', title: 'Uunchai', year: 2022 })
  })

  it('drafts a show and opens it', async () => {
    const madeShow: StoredDraft = {
      ...stored,
      id: 9,
      kind: 'show',
      title: 'Ae Ishq-e-Junoon',
      request: { channel_id: 'UC4JCksJF76g_MdzPVBJoC3Q', title: 'Ae Ishq-e-Junoon' },
      draft: {
        series_name: 'Ae Ishq-e-Junoon',
        network: 'ARY Digital',
        episodes: [{}, {}, {}],
        skipped: [],
        check: { ...stored.draft.check, kind: 'show', verdict: 'probably_missing' },
      },
    }
    const calls = mockApi({
      'GET /admin/api/drafts?limit=25&offset=0': page([show]),
      'POST /admin/api/drafts/show': madeShow,
      'GET /admin/api/drafts/9': madeShow,
    })
    const history = renderRoutedPage(DraftsPage)
    await screen.findByText('Dil Ka Kya Karein')
    await userEvent.click(screen.getByRole('button', { name: 'New draft' }))
    const dialog = await screen.findByRole('dialog')
    await userEvent.type(within(dialog).getByLabelText('Channel id'), 'UC4JCksJF76g_MdzPVBJoC3Q')
    await userEvent.type(within(dialog).getByLabelText('Series name'), 'Ae Ishq-e-Junoon')
    await userEvent.click(within(dialog).getByRole('button', { name: 'Draft' }))
    expect(await screen.findByRole('heading', { name: 'Ae Ishq-e-Junoon' })).toBeInTheDocument()
    expect(calls.find((c) => c.method === 'POST')?.body).toEqual({
      channel_id: 'UC4JCksJF76g_MdzPVBJoC3Q',
      title: 'Ae Ishq-e-Junoon',
    })
    expect(history.location.search).toBe('?draft=9')
    expect(screen.getByText('3')).toBeInTheDocument()
  })

  it('opens a draft, drafts it again in its place and deletes it', async () => {
    const fresh = { ...stored, id: 7 }
    const calls = mockApi({
      'GET /admin/api/drafts?limit=25&offset=0': page([film]),
      'GET /admin/api/drafts/2': stored,
      'GET /admin/api/drafts/7': fresh,
      'POST /admin/api/drafts/film': fresh,
      'DELETE /admin/api/drafts/2': null,
      'DELETE /admin/api/drafts/7': null,
    })
    const history = renderRoutedPage(DraftsPage)
    await userEvent.click(await screen.findByRole('button', { name: /Open/ }))
    expect(await screen.findByRole('heading', { name: 'Tich Button' })).toBeInTheDocument()
    expect(screen.getByText('128 min')).toBeInTheDocument()
    expect(screen.getByText('ARY Digital')).toBeInTheDocument()
    expect(screen.getByText(/Title: Tich Button/)).toBeInTheDocument()

    await userEvent.click(screen.getByRole('button', { name: 'Draft again' }))
    await waitFor(() => expect(history.location.search).toBe('?draft=7'))
    const methods = calls.filter((c) => c.method !== 'GET').map((c) => `${c.method} ${c.url}`)
    expect(methods).toEqual(['POST /admin/api/drafts/film', 'DELETE /admin/api/drafts/2'])
    expect(calls.find((c) => c.method === 'POST')?.body).toEqual({ video_id: 'fIlm0000001' })

    await userEvent.click(await screen.findByRole('button', { name: 'Delete' }))
    await userEvent.click(screen.getByRole('button', { name: 'Yes, delete this draft' }))
    await waitFor(() => expect(history.location.search).toBe(''))
    expect(calls.some((c) => c.method === 'DELETE' && c.url === '/admin/api/drafts/7')).toBe(true)
    expect(await screen.findByText('Tich Button')).toBeInTheDocument()
  })

  it('copies the text block', async () => {
    const user = userEvent.setup()
    mockApi({ 'GET /admin/api/drafts/2': stored })
    renderRoutedPage(DraftsPage, '/?draft=2')
    await user.click(await screen.findByRole('button', { name: 'Copy' }))
    expect(await navigator.clipboard.readText()).toBe(stored.text)
    expect(screen.getByRole('button', { name: 'Copied' })).toBeInTheDocument()
  })
})
