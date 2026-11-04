import { act, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { Activity, ActivityEvent, ActivityEvents, Grab } from '../lib/api'
import { mockApi, renderRoutedPage } from '../test-util'
import { ActivityPage } from './ActivityPage'

afterEach(() => vi.unstubAllGlobals())

// An event's request and result show twice: in their columns and under the kind badge for phones.
const event = (text: string) => screen.getAllByText(text)[0]
const findEvent = async (text: string) => (await screen.findAllByText(text))[0]
const cells = (row: HTMLElement) => within(row).getAllByRole('cell')

const activity: Activity = {
  grabs: [
    {
      nzo_id: 'SABnzbd_nzo_1',
      release_name: 'Kabhi.Main.Kabhi.Tum.S01E01.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR',
      video_id: '0eD-GQCutAU',
      category: 'tv',
      status: 'completed',
      storage_path: '/downloads/tubarr/tv/Kabhi',
      created_at: 1760000000,
    },
  ],
  total: 1,
}

const allEvents: ActivityEvent[] = [
  { kind: 'play', at: 1760000100, video_ids: '0eD-GQCutAU', client: 'Jellyfin-Server/10.11.11', status: 206 },
  { kind: 'search', at: 1760000000, function: 'tvsearch', query: 'tvdbid=452001 season=1 ep=1', results: 1 },
  {
    kind: 'arr_push',
    at: 1759999900,
    app: 'Sonarr',
    release: 'Dar.E.Nijaat.S01E02.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR',
    outcome: 'rejected',
    reasons: ['Not an upgrade for existing episode file(s)'],
  },
]

const page = (items: ActivityEvent[], total = items.length): ActivityEvents => ({
  items,
  total,
  retention_secs: 30 * 86_400,
})
const only = (kind: ActivityEvent['kind']) => page(allEvents.filter((e) => e.kind === kind))
const EVENTS = 'GET /admin/api/activity/events?limit=25&offset=0'

describe('ActivityPage', () => {
  it('shows grabs and events, and filters both', async () => {
    const calls = mockApi({
      'GET /admin/api/activity?limit=25&offset=0': activity,
      'GET /admin/api/activity?category=movies&limit=25&offset=0': { grabs: [], total: 0 },
      [EVENTS]: page(allEvents),
      'GET /admin/api/activity/events?kind=search&limit=25&offset=0': only('search'),
    })
    renderRoutedPage(ActivityPage)
    expect(await screen.findByText(activity.grabs[0].release_name)).toBeInTheDocument()
    expect(screen.getByText('/downloads/tubarr/tv/Kabhi')).toBeInTheDocument()
    expect(event('tvsearch: tvdbid=452001 season=1 ep=1')).toBeInTheDocument()
    expect(event('206 · Jellyfin-Server/10.11.11')).toBeInTheDocument()
    expect(screen.getByText('Kept for 30 days, across restarts.')).toBeInTheDocument()

    await userEvent.selectOptions(screen.getByLabelText('Kind'), 'search')
    await waitFor(() => expect(screen.queryByText('206 · Jellyfin-Server/10.11.11')).not.toBeInTheDocument())
    expect(event('1 results')).toBeInTheDocument()

    await userEvent.selectOptions(screen.getByLabelText('Category'), 'movies')
    expect(await screen.findByText('No grabs yet')).toBeInTheDocument()
    await waitFor(() => expect(calls.at(-1)?.url).toBe('/admin/api/activity?category=movies&limit=25&offset=0'))
  })

  it('lists pushes and queues the catch-up', async () => {
    const calls = mockApi({
      'GET /admin/api/activity?limit=25&offset=0': activity,
      [EVENTS]: page(allEvents),
      'GET /admin/api/activity/events?kind=arr_push&limit=25&offset=0': only('arr_push'),
      'POST /admin/api/arr/push-released': { queued: 12, waiting: 12 },
    })
    renderRoutedPage(ActivityPage)
    expect(
      await findEvent('pushed to Sonarr: Dar.E.Nijaat.S01E02.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR'),
    ).toBeInTheDocument()
    expect(event('rejected: Not an upgrade for existing episode file(s)')).toBeInTheDocument()

    await userEvent.selectOptions(screen.getByLabelText('Kind'), 'arr_push')
    await waitFor(() => expect(screen.queryByText('1 results')).not.toBeInTheDocument())

    await userEvent.click(screen.getByRole('button', { name: 'Push released' }))
    expect(await screen.findByText(/Queued 12 for a push, 12 waiting/)).toBeInTheDocument()
    expect(calls.some((c) => c.method === 'POST' && c.url === '/admin/api/arr/push-released')).toBe(true)
  })

  it('pages the grabs on the server and goes back to page 1 when the category changes', async () => {
    const grabs = (from: number, n: number): Grab[] =>
      Array.from({ length: n }, (_, i) => ({
        ...activity.grabs[0],
        nzo_id: `SABnzbd_nzo_${from + i}`,
        release_name: `Release.${from + i}`,
      }))
    const calls = mockApi({
      'GET /admin/api/activity?limit=25&offset=0': { grabs: grabs(1, 25), total: 40 },
      'GET /admin/api/activity?limit=25&offset=25': { grabs: grabs(26, 15), total: 40 },
      'GET /admin/api/activity?category=tv&limit=25&offset=0': { grabs: grabs(1, 25), total: 30 },
      [EVENTS]: page(allEvents),
    })
    const history = renderRoutedPage(ActivityPage)
    expect(await screen.findByText('Release.1')).toBeInTheDocument()
    expect(screen.getByText(/Showing/).textContent).toBe('Showing 1 to 25 of 40')

    await userEvent.click(screen.getByRole('button', { name: 'Page 2' }))
    expect(await screen.findByText('Release.26')).toBeInTheDocument()
    expect(calls.at(-1)?.url).toBe('/admin/api/activity?limit=25&offset=25')
    expect(screen.getByText(/Showing/).textContent).toBe('Showing 26 to 40 of 40')
    expect(history.location.search).toBe('?page=2')
    // The three events fit one page, so their list shows no pager.
    expect(event('1 results')).toBeInTheDocument()

    await userEvent.selectOptions(screen.getByLabelText('Category'), 'tv')
    await waitFor(() => expect(calls.at(-1)?.url).toBe('/admin/api/activity?category=tv&limit=25&offset=0'))
    expect(history.location.search).toBe('?category=tv')
    await waitFor(() => expect(screen.getByText(/Showing/).textContent).toBe('Showing 1 to 25 of 30'))
  })

  it('keeps category and kind in the URL, and kind leaves the grabs page alone', async () => {
    const calls = mockApi({
      'GET /admin/api/activity?category=movies&limit=25&offset=25': { ...activity, total: 40 },
      'GET /admin/api/activity?limit=25&offset=25': { ...activity, total: 40 },
      'GET /admin/api/activity/events?kind=search&limit=25&offset=0': only('search'),
      'GET /admin/api/activity/events?kind=play&limit=25&offset=0': only('play'),
    })
    const history = renderRoutedPage(ActivityPage, '/?category=movies&kind=search&page=2')
    expect(await findEvent('1 results')).toBeInTheDocument()
    expect(calls[0].url).toBe('/admin/api/activity?category=movies&limit=25&offset=25')
    expect(screen.getByLabelText('Category')).toHaveValue('movies')
    expect(screen.getByLabelText('Kind')).toHaveValue('search')
    expect(screen.queryByText('206 · Jellyfin-Server/10.11.11')).not.toBeInTheDocument()

    await userEvent.selectOptions(screen.getByLabelText('Kind'), 'play')
    await waitFor(() => expect(history.location.search).toBe('?category=movies&kind=play&page=2'))
    expect(await findEvent('206 · Jellyfin-Server/10.11.11')).toBeInTheDocument()

    await userEvent.selectOptions(screen.getByLabelText('Category'), '')
    await waitFor(() => expect(history.location.search).toBe('?kind=play'))

    act(() => history.push('/?kind=search&page=2'))
    await waitFor(() => expect(screen.getByLabelText('Kind')).toHaveValue('search'))
    act(() => history.back())
    await waitFor(() => expect(screen.getByLabelText('Kind')).toHaveValue('play'))
  })

  it('ignores a category or kind it does not know', async () => {
    const calls = mockApi({ 'GET /admin/api/activity?limit=25&offset=0': activity, [EVENTS]: page(allEvents) })
    renderRoutedPage(ActivityPage, '/?category=music&kind=nope')
    expect(await findEvent('1 results')).toBeInTheDocument()
    expect(screen.getByLabelText('Category')).toHaveValue('')
    expect(screen.getByLabelText('Kind')).toHaveValue('')
    expect(calls[0].url).toBe('/admin/api/activity?limit=25&offset=0')
    expect(calls[1].url).toBe('/admin/api/activity/events?limit=25&offset=0')
  })

  it('repeats status, category and time under the release, and request and result under the kind, for phones', async () => {
    mockApi({ 'GET /admin/api/activity?limit=25&offset=0': activity, [EVENTS]: page(allEvents) })
    renderRoutedPage(ActivityPage)
    await screen.findByText(activity.grabs[0].release_name)
    await findEvent('1 results')
    const [grabs, events] = screen.getAllByRole('table')
    const grab = within(cells(within(grabs).getAllByRole('row')[1])[0])
    expect(grab.getAllByRole('term').map((t) => t.textContent)).toEqual(['Status', 'Category', 'When'])
    const [status, category, time] = grab.getAllByRole('definition').map((d) => d.textContent)
    expect([status, category]).toEqual(['completed', 'tv'])
    expect(time).toBe(cells(within(grabs).getAllByRole('row')[1])[2].textContent)
    expect(within(grabs).getByRole('columnheader', { name: 'Status' })).toHaveClass('hidden', 'sm:table-cell')

    const push = within(cells(within(events).getAllByRole('row')[3])[0])
    expect(push.getByText('arr_push')).toBeInTheDocument()
    expect(push.getAllByRole('definition').map((d) => d.textContent)).toEqual([
      'pushed to Sonarr: Dar.E.Nijaat.S01E02.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR',
      'rejected: Not an upgrade for existing episode file(s)',
    ])
    expect(push.getByText(cells(within(events).getAllByRole('row')[3])[3].textContent ?? '')).toHaveClass('sm:hidden')
    expect(within(events).getByRole('columnheader', { name: 'Request' })).toHaveClass('hidden', 'sm:table-cell')
  })

  it('pages the events on the server under their own parameters, and kind goes back to their page 1', async () => {
    const searches = (from: number, n: number): ActivityEvent[] =>
      Array.from({ length: n }, (_, i) => ({
        kind: 'search',
        at: 1760000000 - from - i,
        function: 'tvsearch',
        query: `ep=${from + i}`,
        results: 0,
      }))
    const calls = mockApi({
      'GET /admin/api/activity?limit=25&offset=0': activity,
      [EVENTS]: page(searches(1, 25), 60),
      'GET /admin/api/activity/events?limit=25&offset=25': page(searches(26, 25), 60),
      'GET /admin/api/activity/events?kind=play&limit=25&offset=0': only('play'),
    })
    const history = renderRoutedPage(ActivityPage)
    expect(await findEvent('tvsearch: ep=1')).toBeInTheDocument()
    expect(screen.getByText(/Showing/).textContent).toBe('Showing 1 to 25 of 60')

    await userEvent.click(screen.getByRole('button', { name: 'Page 2' }))
    expect(await findEvent('tvsearch: ep=26')).toBeInTheDocument()
    expect(calls.at(-1)?.url).toBe('/admin/api/activity/events?limit=25&offset=25')
    expect(history.location.search).toBe('?events_page=2')
    expect(screen.getByLabelText('Per page')).toHaveAttribute('id', 'events-pager-size')

    await userEvent.selectOptions(screen.getByLabelText('Kind'), 'play')
    await waitFor(() => expect(history.location.search).toBe('?kind=play'))
    expect(await findEvent('206 · Jellyfin-Server/10.11.11')).toBeInTheDocument()
  })
})
