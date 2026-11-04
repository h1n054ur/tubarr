import { screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { ArrConnections, Settings } from '../lib/api'
import { admin, drama } from '../test-data'
import { mockApi, renderPage } from '../test-util'
import { SettingsPage } from './SettingsPage'

afterEach(() => vi.unstubAllGlobals())

const settings: Settings = {
  public_url: 'http://tubarr:8480/',
  api_key: 'set',
  sonarr: { url: 'http://sonarr:8989/', api_key: 'set' },
  radarr: null,
  arr: { tag: 'youtube', sync_interval_secs: 900 },
  films: { channels: ['UCgoldmines'], min_duration_secs: 4800, similarity: 0.9 },
  notify: null,
}

const connections: ArrConnections = {
  sonarr: { source: 'config', url: 'http://sonarr:8989/', api_key: 'set' },
  radarr: { source: null, url: null, api_key: 'not set' },
}

describe('SettingsPage', () => {
  it('shows the config, tests Sonarr and adds a mapping', async () => {
    const calls = mockApi({
      'GET /admin/api/settings': settings,
      'GET /admin/api/arr/connections': connections,
      'GET /admin/api/users': [admin],
      'POST /admin/api/arr/sonarr/test': { ok: true, tag: 'youtube', tag_id: 7 },
      'POST /admin/api/mappings': drama,
    })
    renderPage(<SettingsPage />)
    expect(await screen.findByText('http://tubarr:8480/')).toBeInTheDocument()
    expect(await screen.findByText('Radarr is not connected.')).toBeInTheDocument()
    expect(screen.getByText(/Set in the config file, so it is read only here/)).toBeInTheDocument()
    expect(screen.queryByLabelText('Sonarr URL')).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Test Radarr' })).toBeDisabled()
    expect(screen.getByText('UCgoldmines')).toBeInTheDocument()

    await userEvent.click(screen.getByRole('button', { name: 'Test Sonarr' }))
    expect(await screen.findByText('Sonarr answered: tag "youtube" has id 7.')).toBeInTheDocument()

    await userEvent.type(screen.getByLabelText('Title'), 'Kabhi Main Kabhi Tum')
    await userEvent.type(screen.getByLabelText('TVDB id'), '452001')
    await userEvent.type(screen.getByLabelText('Language'), 'Urdu')
    await userEvent.type(screen.getByLabelText('Channels'), 'UC4JCksJF76g_MdzPVBJoC3Q')
    await userEvent.click(screen.getByRole('button', { name: 'Add mapping' }))
    expect(await screen.findByText('Added Kabhi Main Kabhi Tum.')).toBeInTheDocument()
    expect(calls.find((c) => c.url === '/admin/api/mappings' && c.method === 'POST')?.body).toEqual({
      kind: 'show',
      title: 'Kabhi Main Kabhi Tum',
      tvdb_id: 452001,
      language: 'Urdu',
      channel_ids: ['UC4JCksJF76g_MdzPVBJoC3Q'],
    })
  })

  it('shows nested config as text, never [object Object]', async () => {
    mockApi({
      'GET /admin/api/settings': {
        ...settings,
        channels: {
          UCary: { network: 'ARY Digital', country: 'Pakistan', language: 'Urdu', utc_offset_mins: 300, timezone: 'America/New_York' },
        },
        root_folders: { shows: { Urdu: '/data/media/pk-dramas', Hindi: '/data/media/in-dramas' }, films: {} },
      },
      'GET /admin/api/arr/connections': connections,
      'GET /admin/api/users': [admin],
    })
    renderPage(<SettingsPage />)
    expect(await screen.findByText('Network ARY Digital · Pakistan · Urdu · America/New_York')).toBeInTheDocument()
    expect(screen.getByText(/Hindi: \/data\/media\/in-dramas/)).toBeInTheDocument()
    expect(document.body.textContent).not.toContain('[object Object]')
  })

  it('adds a podcast with its defaults and asks for the time zone of an unlisted channel', async () => {
    const calls = mockApi({
      'GET /admin/api/settings': { ...settings, channels: { UClisted00000000000000000: { utc_offset_mins: -360, timezone: 'America/Chicago' } } },
      'GET /admin/api/arr/connections': connections,
      'GET /admin/api/users': [admin],
      'POST /admin/api/mappings': { ...drama, title: 'The Joe Rogan Experience' },
    })
    renderPage(<SettingsPage />)
    await screen.findByText('http://tubarr:8480/')
    await userEvent.selectOptions(screen.getByLabelText('Starting rules'), 'podcast')
    await userEvent.type(screen.getByLabelText('Title'), 'The Joe Rogan Experience')
    await userEvent.type(screen.getByLabelText('TVDB id'), '1')
    await userEvent.type(screen.getByLabelText('Language'), 'English')
    await userEvent.type(screen.getByLabelText('Channels'), 'UCzQUP1qoWDoEbmsQxvdjxgQ')
    const zone = screen.getByLabelText('Channel time zone')
    expect(zone).toBeRequired()
    expect(screen.getByText(/Pakistan is not assumed/)).toBeInTheDocument()
    await userEvent.type(zone, 'America/Chicago')
    await userEvent.click(screen.getByRole('button', { name: 'Add mapping' }))
    expect(await screen.findByText('Added The Joe Rogan Experience.')).toBeInTheDocument()
    expect(calls.find((c) => c.url === '/admin/api/mappings' && c.method === 'POST')?.body).toEqual({
      kind: 'show',
      title: 'The Joe Rogan Experience',
      tvdb_id: 1,
      language: 'English',
      channel_ids: ['UCzQUP1qoWDoEbmsQxvdjxgQ'],
      preset: 'podcast',
      timezone: 'America/Chicago',
    })

    // A listed channel brings its own zone.
    await userEvent.selectOptions(screen.getByLabelText('Starting rules'), 'podcast')
    await userEvent.type(screen.getByLabelText('Channels'), 'UClisted00000000000000000')
    expect(screen.getByLabelText('Channel time zone')).not.toBeRequired()
    expect(screen.getByText('From [channels]: America/Chicago. Pick one only to use another.')).toBeInTheDocument()
  })

  it('lists every section', async () => {
    mockApi({
      'GET /admin/api/settings': settings,
      'GET /admin/api/arr/connections': connections,
      'GET /admin/api/users': [admin],
    })
    renderPage(<SettingsPage />)
    // The top-level key and Sonarr's own key, each shown only as "set".
    expect(await screen.findAllByText('API key')).toHaveLength(2)
    expect(screen.getAllByText('not set')).toHaveLength(2)
    expect(screen.getByRole('heading', { name: 'Sonarr and Radarr sync' })).toBeInTheDocument()
    expect(screen.getByText('15 min')).toBeInTheDocument()
    expect(screen.getByText('80 min')).toBeInTheDocument()
    expect(screen.getByText(/looks up the youtube tag/)).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Users' })).toBeInTheDocument()
    expect(await screen.findByText('hani')).toBeInTheDocument()
  })

  it('connects Radarr: tests and saves, never shows the key, then checks its setup', async () => {
    let current = connections
    const calls = mockApi({
      'GET /admin/api/settings': settings,
      'GET /admin/api/arr/connections': () => current,
      'GET /admin/api/users': [admin],
      'PUT /admin/api/arr/radarr/connection': () => {
        current = {
          ...connections,
          radarr: { source: 'saved', url: 'http://radarr:7878/', api_key: 'set' },
        }
        return { ...current.radarr, version: '6.4.4' }
      },
      'DELETE /admin/api/arr/radarr/connection': () => {
        current = connections
        return connections.radarr
      },
      'POST /admin/api/arr/radarr/wiring': {
        ok: true,
        items: [
          { key: 'tag', label: 'Tag "youtube"', state: 'ok', detail: 'Found. Put it on every films Tubarr should serve.' },
          {
            key: 'indexer',
            label: 'Indexer "Tubarr"',
            state: 'missing',
            detail: 'Add a Newznab indexer named "Tubarr" in Radarr under Settings > Indexers.',
          },
        ],
      },
    })
    renderPage(<SettingsPage />)
    await userEvent.type(await screen.findByLabelText('Radarr URL'), 'http://radarr:7878')
    await userEvent.type(screen.getByLabelText('Radarr API key'), 'radarr-key-123')
    await userEvent.click(screen.getByRole('button', { name: 'Test and save' }))
    expect(
      await screen.findByText('Connected. Radarr 6.4.4 answered. Syncing will start in a moment.'),
    ).toBeInTheDocument()
    expect(calls.find((c) => c.method === 'PUT')?.body).toEqual({ url: 'http://radarr:7878', api_key: 'radarr-key-123' })
    expect(screen.queryByText(/Restart Tubarr/)).not.toBeInTheDocument()
    expect(screen.getByLabelText('Radarr API key')).toHaveValue('')
    expect(screen.getByLabelText('Radarr API key')).toHaveAttribute('placeholder', 'Saved. Leave empty to keep it.')
    expect(screen.getByRole('button', { name: 'Remove' })).toBeInTheDocument()

    await userEvent.click(screen.getByRole('button', { name: 'Check Radarr setup' }))
    const list = await screen.findByRole('list', { name: 'Radarr setup' })
    expect(list).toHaveTextContent('In place')
    expect(list).toHaveTextContent('Missing')
    expect(list).toHaveTextContent('Add a Newznab indexer named "Tubarr" in Radarr under Settings > Indexers.')
    expect(document.body.textContent).not.toContain('radarr-key-123')

    await userEvent.click(screen.getByRole('button', { name: 'Remove' }))
    expect(await screen.findByText('Removed. Tubarr no longer uses this Radarr.')).toBeInTheDocument()
  })

  it('shows why a save was refused', async () => {
    mockApi({
      'GET /admin/api/settings': settings,
      'GET /admin/api/arr/connections': connections,
      'GET /admin/api/users': [admin],
      'PUT /admin/api/arr/radarr/connection': new Response(
        JSON.stringify({ error: 'Not saved: Radarr refused the API key (HTTP 401 Unauthorized): copy it again from Radarr, Settings > General' }),
        { status: 502 },
      ),
    })
    renderPage(<SettingsPage />)
    await userEvent.type(await screen.findByLabelText('Radarr URL'), 'http://radarr:7878')
    await userEvent.type(screen.getByLabelText('Radarr API key'), 'wrong')
    await userEvent.click(screen.getByRole('button', { name: 'Test and save' }))
    expect(await screen.findByText(/Not saved: Radarr refused the API key/)).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Remove' })).not.toBeInTheDocument()
  })
})

describe('mapping export and import', () => {
  const file = { format: 1, exported_at: 0, mappings: [{ kind: 'show', tvdb_id: 452001, title: 'Kabhi Main Kabhi Tum' }] }
  const report = (dry_run: boolean) => ({
    dry_run,
    creates: [{ title: 'Kabhi Main Kabhi Tum', kind: 'show', tvdb_id: 452001, tmdb_id: null }],
    updates: [],
    unchanged: [],
    conflicts: [{ title: 'Theri', tvdb_id: null, tmdb_id: 352173, reason: 'TMDB id 352173 is in the file twice' }],
  })

  it('links the export and applies an import only after the dry run', async () => {
    const calls = mockApi({
      'GET /admin/api/settings': settings,
      'GET /admin/api/users': [admin],
      'POST /admin/api/mappings/import?dry_run=true': report(true),
      'POST /admin/api/mappings/import': report(false),
    })
    renderPage(<SettingsPage />)
    await screen.findByText('http://tubarr:8480/')
    expect(screen.getByRole('link', { name: 'Download export' })).toHaveAttribute('href', '/admin/api/mappings/export')
    const apply = screen.getByRole('button', { name: 'Apply import' })
    expect(apply).toBeDisabled()

    const upload = new File([JSON.stringify(file)], 'tubarr-mappings.json', { type: 'application/json' })
    await userEvent.upload(screen.getByLabelText('Import file'), upload)
    expect(
      await screen.findByText('Would add 1 mapping, update 0, leave 0 as they are and skip 1 with a conflict.'),
    ).toBeInTheDocument()
    expect(screen.getByText(/TMDB id 352173 is in the file twice/)).toBeInTheDocument()
    expect(calls.filter((c) => c.url === '/admin/api/mappings/import')).toHaveLength(0)

    await userEvent.click(apply)
    expect(
      await screen.findByText('Added 1 mapping, updated 0, left 0 as they were and skipped 1 with a conflict.'),
    ).toBeInTheDocument()
    expect(calls.find((c) => c.url === '/admin/api/mappings/import')?.body).toEqual(file)
  })

  it('says so when the file is not JSON', async () => {
    mockApi({ 'GET /admin/api/settings': settings, 'GET /admin/api/users': [admin] })
    renderPage(<SettingsPage />)
    await screen.findByText('http://tubarr:8480/')
    await userEvent.upload(screen.getByLabelText('Import file'), new File(['not json'], 'notes.json', { type: 'application/json' }))
    expect(await screen.findByText('notes.json is not a Tubarr mapping export.')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Apply import' })).toBeDisabled()
  })
})
