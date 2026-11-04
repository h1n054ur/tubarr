import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { health } from '../test-data'
import { mockApi, renderPage } from '../test-util'
import { HealthPage } from './HealthPage'

afterEach(() => vi.unstubAllGlobals())

describe('HealthPage', () => {
  it('renders the report and runs check and rollback', async () => {
    const calls = mockApi({
      'GET /api/health': health,
      'POST /admin/api/binaries/check': [{ tool: 'yt-dlp', ok: true, result: 'updated from 2026.09.30 to 2026.10.10' }],
      'POST /admin/api/binaries/yt-dlp/rollback': { tool: 'yt-dlp', rolled_back: true },
    })
    renderPage(<HealthPage />)
    expect(await screen.findByText('degraded')).toBeInTheDocument()
    expect(screen.getByText('v0.1.0 (e7d7114a1b2c)')).toBeInTheDocument()
    // Whole tokens only: 2.5 available is 2 a lookup can use.
    expect(screen.getByText('2')).toBeInTheDocument()
    expect(screen.getByText('Degraded: the canary failed its last check.')).toBeInTheDocument()
    // deno has no previous version kept, so it has no roll back button.
    expect(screen.queryByRole('button', { name: 'Roll back deno' })).not.toBeInTheDocument()
    expect(screen.getByText('2026.09.30')).toBeInTheDocument()
    expect(screen.getByText('PO token for 30 min more')).toBeInTheDocument()
    expect(screen.getByText('Plain: 9 of 10 worked')).toBeInTheDocument()
    expect(screen.getByText('UC4JCksJF76g_MdzPVBJoC3Q')).toBeInTheDocument()
    expect(screen.getByText('12')).toBeInTheDocument()
    // Capped back episodes stay out of Waiting (2 pending + 3 unmatched) and show on their own.
    expect(screen.getByText('5')).toBeInTheDocument()
    expect(screen.getByText('Capped by backfill limit').nextElementSibling).toHaveTextContent('40')
    // The backfills: a failed one waiting for its next run, a finished one.
    const failed = screen.getByText('Dar E Nijaat').closest('tr')!
    expect(failed).toHaveTextContent('yt-dlp did not finish within 60s')
    expect(failed).toHaveTextContent('none yet')
    const finished = screen.getByText('Kabhi Main Kabhi Tum').closest('tr')!
    expect(finished).toHaveTextContent('done')
    expect(finished).toHaveTextContent('42')
    // The Jellyfin merge queue and its last error.
    expect(screen.getByText('Queue').nextElementSibling).toHaveTextContent('3')
    expect(screen.getByText(/Jellyfin answered 401 Unauthorized/)).toBeInTheDocument()
    // The last matcher run and the store sizes.
    expect(screen.getByText(/1 mappings matched, 7 unchanged/)).toBeInTheDocument()
    expect(screen.getByText('Unmatched notes').nextElementSibling).toHaveTextContent('6123')
    expect(screen.getByText('Watched videos').nextElementSibling).toHaveTextContent('18450')
    expect(screen.getByText('Download history').nextElementSibling).toHaveTextContent('212 grabs')
    expect(screen.getByText('Activity events').nextElementSibling).toHaveTextContent('3410 searches, pushes and plays')
    expect(screen.getByText('Sample tables').nextElementSibling).toHaveTextContent('840 files, 110.0 MiB')
    expect(screen.getByText('First plays').nextElementSibling).toHaveTextContent('1 of 2 reading, 2 waiting')
    expect(screen.getByText('Streams').nextElementSibling).toHaveTextContent('3 of 8')

    await userEvent.click(screen.getByRole('button', { name: 'Check now' }))
    expect(await screen.findByText('yt-dlp: updated from 2026.09.30 to 2026.10.10')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Roll back yt-dlp' }))
    expect(await screen.findByText('yt-dlp is back on the previous version.')).toBeInTheDocument()
    await waitFor(() => expect(calls.filter((c) => c.method === 'POST')).toHaveLength(2))
  })

  it('hides roll back when nothing is installed or kept', async () => {
    mockApi({
      'GET /api/health': {
        ...health,
        status: 'failing',
        reasons: ['yt-dlp is not installed or does not run'],
        binaries: health.binaries.map((b) => ({ ...b, installed: null, can_roll_back: false })),
        jellyfin: null,
      },
    })
    renderPage(<HealthPage />)
    expect(await screen.findByText('Failing: yt-dlp is not installed or does not run.')).toBeInTheDocument()
    expect(screen.getByText('failing').parentElement).toHaveAttribute('title', 'yt-dlp is not installed or does not run')
    expect(screen.queryByRole('button', { name: /Roll back/ })).not.toBeInTheDocument()
    expect(screen.queryByText('Jellyfin versions')).not.toBeInTheDocument()
  })

  it('says when no show needs a backfill and when one is due or over', async () => {
    const [due] = health.backfills
    mockApi({
      'GET /api/health': {
        ...health,
        backfills: [
          { ...due, next_run_at: 1000 },
          { ...due, mapping_id: 9, title: 'Over', next_run_at: null },
        ],
      },
    })
    renderPage(<HealthPage />)
    expect((await screen.findByText('Dar E Nijaat')).closest('tr')).toHaveTextContent('Due now')
    expect(screen.getByText('Over').closest('tr')).toHaveTextContent('Ended')
  })

  it('shows an empty backfill list', async () => {
    mockApi({ 'GET /api/health': { ...health, backfills: [] } })
    renderPage(<HealthPage />)
    expect(await screen.findByText('No show needs one.')).toBeInTheDocument()
  })

  it('marks channels whose failing feed is stood in for by short listings', async () => {
    const [ok] = health.watcher.channels
    mockApi({
      'GET /api/health': {
        ...health,
        watcher: {
          active: true,
          channels: [
            ok,
            {
              ...ok,
              channel_id: 'UCaaaaaaaaaaaaaaaaaaaaaa',
              last_feed_error: 'feed answered HTTP 404',
              feed_failures: 3,
              feed_failing_since: 1760000000,
              feed_fallback: true,
              last_fallback_at: 1760001800,
            },
          ],
        },
      },
    })
    renderPage(<HealthPage />)
    const failing = (await screen.findByText('UCaaaaaaaaaaaaaaaaaaaaaa')).closest('tr')!
    expect(failing).toHaveTextContent('feed failing, using listing')
    expect(failing).toHaveTextContent('3 in a row, failing since')
    expect(failing).toHaveTextContent('Last short listing')
    expect(screen.getByText('UC4JCksJF76g_MdzPVBJoC3Q').closest('tr')).not.toHaveTextContent('using listing')
    expect(
      screen.getByText("1 channel's feed is failing, so its newest uploads are listed instead."),
    ).toBeInTheDocument()
  })

  it('repeats feed, listing and backfill runs under the first cell for phones', async () => {
    mockApi({ 'GET /api/health': health })
    renderPage(<HealthPage />)
    const summary = (row: HTMLElement) => {
      const first = within(within(row).getAllByRole('cell')[0])
      return [first.getAllByRole('term').map((t) => t.textContent), first.getAllByRole('definition')]
    }
    const [channelTerms, channel] = summary((await screen.findByText('UC4JCksJF76g_MdzPVBJoC3Q')).closest('tr')!)
    expect(channelTerms).toEqual(['Feed', 'Listing'])
    expect(channel[0]).toHaveTextContent(/^Feed /)
    expect(channel[1]).toHaveTextContent(/^Listing /)
    const [backfillTerms, failed] = summary(screen.getByText('Dar E Nijaat').closest('tr')!)
    expect(backfillTerms).toEqual(['Last run', 'Next run'])
    expect(failed[0]).toHaveTextContent('yt-dlp did not finish within 60s')
    expect(failed[1]).toHaveTextContent(/^Next run /)
    const [, finished] = summary(screen.getByText('Kabhi Main Kabhi Tum').closest('tr')!)
    expect(finished[1]).toHaveTextContent(/^done$/)
    for (const name of ['Feed', 'Listing', 'Last run', 'Next run'])
      expect(screen.getByRole('columnheader', { name })).toHaveClass('hidden', 'sm:table-cell')
    expect(screen.getByRole('columnheader', { name: 'Videos' })).not.toHaveClass('hidden')
    expect(screen.getByRole('columnheader', { name: 'Found' })).not.toHaveClass('hidden')
  })

  it('shows a refused key', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify({ error: 'missing or wrong API key' }), { status: 401 })),
    )
    renderPage(<HealthPage />)
    expect(await screen.findByRole('alert')).toHaveTextContent('missing or wrong API key')
  })
})
