import { act, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { drama, theri, unsureNote } from '../test-data'
import type { UnmatchedNote } from '../lib/api'
import { mockApi, renderRoutedPage } from '../test-util'
import { UnmatchedPage } from './UnmatchedPage'

afterEach(() => vi.unstubAllGlobals())

/** `n` notes from id `from` on, as one server page. */
const notes = (from: number, n: number): UnmatchedNote[] =>
  Array.from({ length: n }, (_, i) => ({
    ...unsureNote,
    id: from + i,
    video_id: `vid${from + i}`,
    video_title: `Video ${from + i}`,
  }))

describe('UnmatchedPage', () => {
  it('lists notes, filters by reason and mapping, and ignores one', async () => {
    const calls = mockApi({
      'GET /admin/api/unmatched?limit=25&offset=0': { items: [unsureNote], total: 1 },
      'GET /admin/api/unmatched?state=pending&limit=25&offset=0': { items: [], total: 0 },
      'GET /admin/api/unmatched?state=pending&mapping_id=2&limit=25&offset=0': { items: [], total: 0 },
      'GET /admin/api/mappings': [drama, theri],
      'POST /admin/api/unmatched/7/ignore': null,
    })
    renderRoutedPage(UnmatchedPage)
    expect(await screen.findByText('Kabhi Main Kabhi Tum Episode 3')).toBeInTheDocument()
    // Shown in its column and again in the phone summary under the title.
    expect(screen.getAllByText('two uploads say episode 3')).toHaveLength(2)
    expect(screen.getAllByText('35 min')).toHaveLength(2)
    expect(screen.getByRole('link', { name: /YouTube/ })).toHaveAttribute(
      'href',
      'https://www.youtube.com/watch?v=0eD-GQCutAU',
    )

    await userEvent.click(screen.getByRole('button', { name: /Ignore/ }))
    await waitFor(() => expect(calls.some((c) => c.url === '/admin/api/unmatched/7/ignore')).toBe(true))
    expect(calls.find((c) => c.method === 'POST')?.body).toEqual({ ignored: true })

    await userEvent.selectOptions(screen.getByLabelText('Reason'), 'pending')
    expect(await screen.findByText('Nothing unmatched')).toBeInTheDocument()
    await userEvent.selectOptions(screen.getByLabelText('Mapping'), '2')
    await waitFor(() => expect(calls.at(-1)?.url).toBe('/admin/api/unmatched?state=pending&mapping_id=2&limit=25&offset=0'))
  })

  it('repeats mapping, length, upload date and reason under the title for phones', async () => {
    mockApi({
      'GET /admin/api/unmatched?limit=25&offset=0': { items: [unsureNote], total: 1 },
      'GET /admin/api/mappings': [drama],
    })
    renderRoutedPage(UnmatchedPage)
    await screen.findByText('Kabhi Main Kabhi Tum Episode 3')
    const row = screen.getAllByRole('row')[1]
    const cells = within(row).getAllByRole('cell')
    const first = within(cells[0])
    expect(first.getAllByRole('term').map((t) => t.textContent)).toEqual(['Mapping', 'Length', 'Uploaded', 'Reason'])
    expect(first.getAllByRole('definition').map((d) => d.textContent)).toEqual([
      cells[1].textContent,
      '35 min',
      cells[3].textContent,
      'unsuretwo uploads say episode 3',
    ])
    // Each copy hides from the breakpoint where its own column shows.
    expect(first.getByText('Mapping').parentElement).toHaveClass('md:hidden')
    expect(first.getByText('Reason').parentElement).toHaveClass('sm:hidden')
    // Length and Uploaded join the row only at xl: at lg the sidebar leaves too little room for them and the actions.
    expect(screen.getByRole('columnheader', { name: 'Length' })).toHaveClass('hidden', 'xl:table-cell')
    expect(screen.getByRole('columnheader', { name: 'Uploaded' })).toHaveClass('hidden', 'xl:table-cell')
    expect(first.getByText('Length').parentElement?.parentElement).toHaveClass('xl:hidden')
    // A long mapping title wraps in its column instead of pushing the actions out of the card.
    expect(cells[1]).not.toHaveClass('whitespace-nowrap')
    expect(screen.getByRole('columnheader', { name: 'Actions' })).not.toHaveClass('hidden')
    expect(within(cells[5]).getByRole('link', { name: /YouTube/ })).toBeInTheDocument()
  })

  it('keeps the description full width and the filters under it, with Mapping capped', async () => {
    mockApi({
      'GET /admin/api/unmatched?limit=25&offset=0': { items: [unsureNote], total: 1 },
      'GET /admin/api/mappings': [{ ...drama, title: 'A very long mapping title '.repeat(8) }],
    })
    renderRoutedPage(UnmatchedPage)
    await screen.findByText('Kabhi Main Kabhi Tum Episode 3')
    const text = screen.getByText(/Videos the matcher saw/)
    const filters = screen.getByLabelText('Search').closest('div.mt-4') as HTMLElement
    // Text and filters are stacked siblings, never a flex row where the filters squeeze the text.
    expect(filters.parentElement).toBe(text.parentElement)
    expect(text.parentElement?.className ?? '').not.toMatch(/flex/)
    expect(filters.className).not.toMatch(/shrink-0|nowrap/)
    expect(filters).toHaveClass('sm:flex', 'sm:flex-wrap')
    // Mapping sits in a fixed-width box from sm up, so its longest option cannot widen the row.
    const mappingBox = screen.getByLabelText('Mapping').closest('div.col-span-2') as HTMLElement
    expect(mappingBox).toHaveClass('sm:w-64', 'min-w-0')
    // Phones keep the compact grid: Search and Mapping full width, Mapping last.
    expect(filters).toHaveClass('grid', 'grid-cols-2')
    expect(mappingBox).toHaveClass('order-last', 'sm:order-none')
  })

  it('pins a note to its episode', async () => {
    const calls = mockApi({
      'GET /admin/api/unmatched?limit=25&offset=0': { items: [unsureNote], total: 1 },
      'GET /admin/api/mappings': [drama],
      'POST /admin/api/pins': { id: 1, mapping_id: 1, video_id: '0eD-GQCutAU', season: 1, episode: 4, air_date: null, created_at: 0 },
    })
    renderRoutedPage(UnmatchedPage)
    await userEvent.click(await screen.findByRole('button', { name: /^Pin/ }))
    const episode = await screen.findByLabelText('Episode')
    expect(screen.getByLabelText('Season')).toHaveValue(1)
    await userEvent.clear(episode)
    await userEvent.type(episode, '4')
    await userEvent.click(screen.getByRole('button', { name: 'Pin' }))
    await waitFor(() => expect(calls.some((c) => c.url === '/admin/api/pins')).toBe(true))
    expect(calls.find((c) => c.url === '/admin/api/pins')?.body).toEqual({
      mapping_id: 1,
      video_id: '0eD-GQCutAU',
      season: 1,
      episode: 4,
    })
  })

  it('sends the search to the server and offers to show all when nothing matches', async () => {
    const calls = mockApi({
      'GET /admin/api/unmatched?limit=25&offset=0': { items: [unsureNote], total: 1 },
      'GET /admin/api/unmatched?q=kabhi&limit=25&offset=0': { items: [unsureNote], total: 1 },
      'GET /admin/api/unmatched?q=xyzzy&limit=25&offset=0': { items: [], total: 0 },
      'GET /admin/api/mappings': [drama],
    })
    const history = renderRoutedPage(UnmatchedPage)
    expect(await screen.findByText('Kabhi Main Kabhi Tum Episode 3')).toBeInTheDocument()

    await userEvent.type(screen.getByLabelText('Search'), 'kabhi')
    await waitFor(() => expect(calls.at(-1)?.url).toBe('/admin/api/unmatched?q=kabhi&limit=25&offset=0'))
    expect(history.location.search).toBe('?q=kabhi')

    await userEvent.clear(screen.getByLabelText('Search'))
    await userEvent.type(screen.getByLabelText('Search'), 'xyzzy')
    expect(await screen.findByText('No unmatched videos match "xyzzy".')).toBeInTheDocument()
    expect(screen.queryByText('Nothing unmatched')).not.toBeInTheDocument()
    const empty = screen.getByText('No matches').parentElement as HTMLElement
    expect(within(empty).queryByRole('button', { name: 'Clear search' })).not.toBeInTheDocument()
    await userEvent.click(within(empty).getByRole('button', { name: 'Show all' }))
    expect(await screen.findByText('Kabhi Main Kabhi Tum Episode 3')).toBeInTheDocument()
    expect(screen.getByLabelText('Search')).toHaveValue('')
  })

  it('pages on the server, shows the total and keeps the rows while the next page loads', async () => {
    let release: () => void = () => {}
    const second = new Promise<void>((resolve) => (release = resolve))
    const calls = mockApi({
      'GET /admin/api/unmatched?limit=25&offset=0': { items: notes(1, 25), total: 60 },
      'GET /admin/api/unmatched?limit=25&offset=25': async () => {
        await second
        return { items: notes(26, 25), total: 60 }
      },
      'GET /admin/api/unmatched?limit=50&offset=0': { items: notes(1, 50), total: 60 },
      'GET /admin/api/mappings': [],
    })
    const history = renderRoutedPage(UnmatchedPage)
    expect(await screen.findByText('Video 1')).toBeInTheDocument()
    expect(screen.getByText(/Showing/).textContent).toBe('Showing 1 to 25 of 60')

    await userEvent.click(screen.getByRole('button', { name: 'Page 2' }))
    await waitFor(() => expect(calls.at(-1)?.url).toBe('/admin/api/unmatched?limit=25&offset=25'))
    expect(screen.getByText('Video 1')).toBeInTheDocument()
    release()
    expect(await screen.findByText('Video 26')).toBeInTheDocument()
    expect(screen.getByText(/Showing/).textContent).toBe('Showing 26 to 50 of 60')
    expect(history.location.search).toBe('?page=2')

    await userEvent.selectOptions(screen.getByLabelText('Per page'), '50')
    await waitFor(() => expect(calls.at(-1)?.url).toBe('/admin/api/unmatched?limit=50&offset=0'))
    expect(await screen.findByText('Video 50')).toBeInTheDocument()
  })

  it('goes back to page 1 when a filter changes', async () => {
    const calls = mockApi({
      'GET /admin/api/unmatched?limit=25&offset=25': { items: notes(26, 25), total: 60 },
      'GET /admin/api/unmatched?ignored=true&limit=25&offset=0': { items: notes(1, 25), total: 70 },
      'GET /admin/api/mappings': [],
    })
    const history = renderRoutedPage(UnmatchedPage, '/?page=2')
    expect(await screen.findByText('Video 26')).toBeInTheDocument()

    await userEvent.selectOptions(screen.getByLabelText('Show'), 'all')
    await waitFor(() => expect(calls.at(-1)?.url).toBe('/admin/api/unmatched?ignored=true&limit=25&offset=0'))
    expect(history.location.search).toBe('?show=all')
    expect(await screen.findByText('Video 1')).toBeInTheDocument()
    expect(screen.getByText(/Showing/).textContent).toBe('Showing 1 to 25 of 70')
  })

  it('keeps reason, mapping and show in the URL and restores them from a link and on back', async () => {
    const calls = mockApi({
      'GET /admin/api/unmatched?limit=25&offset=0': { items: notes(1, 3), total: 3 },
      'GET /admin/api/unmatched?state=unsure&mapping_id=2&ignored=true&limit=25&offset=25': {
        items: notes(26, 2),
        total: 27,
      },
      'GET /admin/api/unmatched?state=unsure&mapping_id=2&ignored=true&limit=25&offset=0': {
        items: notes(1, 25),
        total: 27,
      },
      'GET /admin/api/unmatched?state=partial&mapping_id=2&ignored=true&limit=25&offset=0': { items: [], total: 0 },
      'GET /admin/api/mappings': [drama, theri],
    })
    const history = renderRoutedPage(UnmatchedPage, '/?reason=unsure&mapping=2&show=all&page=2')
    expect(await screen.findByText('Video 26')).toBeInTheDocument()
    expect(screen.getByLabelText('Reason')).toHaveValue('unsure')
    expect(screen.getByLabelText('Show')).toHaveValue('all')
    await waitFor(() => expect(screen.getByLabelText('Mapping')).toHaveValue('2'))

    const before = history.length
    await userEvent.selectOptions(screen.getByLabelText('Reason'), 'partial')
    await waitFor(() =>
      expect(calls.at(-1)?.url).toBe('/admin/api/unmatched?state=partial&mapping_id=2&ignored=true&limit=25&offset=0'),
    )
    expect(history.location.search).toBe('?reason=partial&mapping=2&show=all')
    expect(history.length).toBe(before)

    await userEvent.selectOptions(screen.getByLabelText('Mapping'), '')
    await userEvent.selectOptions(screen.getByLabelText('Reason'), '')
    await userEvent.selectOptions(screen.getByLabelText('Show'), 'open')
    await waitFor(() => expect(history.location.search).toBe(''))
    expect(await screen.findByText('Video 3')).toBeInTheDocument()

    act(() => history.push('/?reason=unsure&mapping=2&show=all'))
    expect(await screen.findByText('Video 25')).toBeInTheDocument()
    act(() => history.back())
    await waitFor(() => expect(screen.getByLabelText('Reason')).toHaveValue(''))
    expect(await screen.findByText('Video 3')).toBeInTheDocument()
  })

  it('ignores filter values it does not know', async () => {
    const calls = mockApi({
      'GET /admin/api/unmatched?limit=25&offset=0': { items: [unsureNote], total: 1 },
      'GET /admin/api/mappings': [drama, theri],
    })
    renderRoutedPage(UnmatchedPage, '/?reason=lost&mapping=abc&show=hidden')
    expect(await screen.findByText('Kabhi Main Kabhi Tum Episode 3')).toBeInTheDocument()
    expect(screen.getByLabelText('Reason')).toHaveValue('')
    expect(screen.getByLabelText('Show')).toHaveValue('open')
    expect(calls.filter((c) => c.url.startsWith('/admin/api/unmatched')).map((c) => c.url)).toEqual([
      '/admin/api/unmatched?limit=25&offset=0',
    ])
  })
})
