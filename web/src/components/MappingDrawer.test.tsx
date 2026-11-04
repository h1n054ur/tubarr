import { cleanup, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { drama, run, theri } from '../test-data'
import type { Alias } from '../lib/api'
import { MappingDrawer, patchFrom, formFrom } from './MappingDrawer'

const aliases: Alias[] = [
  { alias: 'KMKT', source: 'arr' },
  { alias: 'Kabhi Main', source: 'user' },
]

function drawer(overrides: Partial<Parameters<typeof MappingDrawer>[0]> = {}) {
  const props = {
    mapping: drama,
    aliases,
    onClose: vi.fn(),
    onSave: vi.fn(),
    onTest: vi.fn(),
    saving: false,
    testing: false,
    error: null,
    run: null,
    ...overrides,
  }
  render(<MappingDrawer {...props} />)
  return props
}

describe('MappingDrawer', () => {
  it('fills the form from the mapping', () => {
    drawer()
    expect(screen.getByRole('dialog')).toHaveTextContent('Kabhi Main Kabhi Tum')
    expect(screen.getByLabelText('Official channels')).toHaveValue('UC4JCksJF76g_MdzPVBJoC3Q')
    expect(screen.getByLabelText('Excludes')).toHaveValue('Recap')
    expect(screen.getByLabelText('Minimum length (minutes)')).toHaveValue(25)
    expect(screen.getByLabelText('Episode regex')).toHaveValue('')
  })

  it('sends handles as typed and shows the resolved ids and names after the save', async () => {
    const props = {
      mapping: drama,
      aliases,
      onClose: vi.fn(),
      onSave: vi.fn(),
      onTest: vi.fn(),
      saving: false,
      testing: false,
      error: null,
      run: null,
    }
    const { rerender } = render(<MappingDrawer {...props} />)
    expect(screen.queryByRole('list', { name: 'Channel names' })).not.toBeInTheDocument()
    const field = screen.getByLabelText('Official channels')
    await userEvent.clear(field)
    await userEvent.type(field, '@arydigital{enter}https://www.youtube.com/@HumTV')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    expect(props.onSave.mock.calls[0][0].channel_ids).toEqual(['@arydigital', 'https://www.youtube.com/@HumTV'])

    const saved = {
      ...drama,
      channel_ids: ['UC4JCksJF76g_MdzPVBJoC3Q', 'UCnHumHumHumHumHumHumHum'],
      channel_titles: { UC4JCksJF76g_MdzPVBJoC3Q: 'ARY Digital' },
    }
    rerender(<MappingDrawer {...props} mapping={saved} />)
    expect(screen.getByLabelText('Official channels')).toHaveValue('UC4JCksJF76g_MdzPVBJoC3Q\nUCnHumHumHumHumHumHumHum')
    const names = screen.getByRole('list', { name: 'Channel names' })
    expect(names).toHaveTextContent('UC4JCksJF76g_MdzPVBJoC3Q ARY Digital')
    expect(names).not.toHaveTextContent('UCnHum')
    cleanup()
  })

  it('saves the edited rules as a patch', async () => {
    const props = drawer()
    const excludes = screen.getByLabelText('Excludes')
    await userEvent.clear(excludes)
    await userEvent.type(excludes, 'Recap{enter}Shorts')
    const regex = screen.getByLabelText('Episode regex')
    await userEvent.type(regex, 'Ep (?P<ep>[[0-9]+)')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    expect(props.onSave).toHaveBeenCalledWith(
      {
        language: 'Urdu',
        channel_ids: ['UC4JCksJF76g_MdzPVBJoC3Q'],
        excludes: ['Recap', 'Shorts'],
        min_duration_secs: 1500,
        episode_regex: 'Ep (?P<ep>[0-9]+)',
        playlist_id: null,
        playlist_season: 1,
        playlist_offset: 1,
        playlist_mode: 'source',
        title_number: false,
        backfill_limit: null,
        audio_version: false,
        episode_thumbnails: false,
        root_folder: null,
      },
      // Aliases left alone are not sent.
      null,
    )
  })

  it('uses a playlist to find videos by default and shows season and first episode only for order', async () => {
    const props = drawer({ mapping: { ...drama, playlist_id: 'PLdarENijaat' } })
    const mode = screen.getByLabelText('Use playlist for')
    expect(mode).toHaveValue('source')
    expect(screen.queryByLabelText('First episode')).not.toBeInTheDocument()

    await userEvent.selectOptions(mode, 'order')
    expect(screen.getByLabelText('Its season')).toHaveValue(1)
    const first = screen.getByLabelText('First episode')
    await userEvent.clear(first)
    await userEvent.type(first, '2')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    expect(props.onSave).toHaveBeenCalledWith(
      expect.objectContaining({ playlist_id: 'PLdarENijaat', playlist_mode: 'order', playlist_offset: 2 }),
      null,
    )
  })

  it('shows arr aliases as read-only chips and saves user aliases with them', async () => {
    const props = drawer()
    const fromSonarr = screen.getByRole('list', { name: 'Aliases from Sonarr' })
    expect(fromSonarr).toHaveTextContent('KMKT')
    expect(fromSonarr).toHaveTextContent('from Sonarr')
    expect(fromSonarr.querySelector('input, textarea')).toBeNull()
    const field = screen.getByLabelText('Aliases')
    expect(field).toHaveValue('Kabhi Main')
    await userEvent.clear(field)
    await userEvent.type(field, 'Kabhi Main, Kabhi Tum{enter}{enter}KMKT Drama')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    expect(props.onSave).toHaveBeenCalledWith(expect.any(Object), ['KMKT', 'Kabhi Main, Kabhi Tum', 'KMKT Drama'])
  })

  it('turns on matching by the number in the title for a show only', async () => {
    const props = drawer()
    const toggle = screen.getByRole('checkbox', { name: 'Match by number in the title' })
    expect(toggle).not.toBeChecked()
    expect(toggle).toHaveAccessibleDescription(
      'For podcasts that number episodes like #2569: matched against TheTVDB episode titles',
    )
    await userEvent.click(toggle)
    expect(toggle).toBeChecked()
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    expect(props.onSave).toHaveBeenCalledWith(expect.objectContaining({ title_number: true }), null)
  })

  it('fills the podcast defaults and asks for the time zone of a channel not in the config', async () => {
    const props = drawer()
    await userEvent.click(screen.getByRole('button', { name: 'Use podcast defaults' }))
    expect(screen.getByRole('checkbox', { name: 'Match by number in the title' })).toBeChecked()
    const zone = screen.getByLabelText('Channel time zone')
    expect(zone).toBeRequired()
    expect(zone).toHaveValue('')
    expect(screen.getByLabelText('Fixed UTC offset (minutes)')).toHaveValue(null)
    expect(screen.getByText(/not in \[channels\].*Pakistan is not assumed/)).toBeInTheDocument()
    await userEvent.type(zone, 'America/Chicago')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    expect(props.onSave).toHaveBeenCalledWith(
      expect.objectContaining({
        title_number: true,
        episode_thumbnails: true,
        backfill_limit: 10,
        min_duration_secs: 1200,
        excludes: ['Recap', 'Clip', 'Best of'],
        timezone: 'America/Chicago',
        utc_offset_mins: null,
      }),
      null,
    )
  })

  it('takes a podcast zone from the channel config, its time zone over its offset', async () => {
    drawer({ channels: { UC4JCksJF76g_MdzPVBJoC3Q: { utc_offset_mins: -360 } } })
    await userEvent.click(screen.getByRole('button', { name: 'Use podcast defaults' }))
    expect(screen.getByLabelText('Fixed UTC offset (minutes)')).toHaveValue(-360)
    expect(screen.getByLabelText('Channel time zone')).not.toBeRequired()
    expect(screen.getByText('From [channels]: UTC offset -360 minutes. Pick one only to use another.')).toBeInTheDocument()
  })

  it('fills the time zone a listed channel sets', async () => {
    drawer({ channels: { UC4JCksJF76g_MdzPVBJoC3Q: { utc_offset_mins: 300, timezone: 'America/New_York' } } })
    await userEvent.click(screen.getByRole('button', { name: 'Use podcast defaults' }))
    expect(screen.getByLabelText('Channel time zone')).toHaveValue('America/New_York')
    expect(screen.getByLabelText('Fixed UTC offset (minutes)')).toHaveValue(null)
  })

  it('suggests the alias when no title names the show', async () => {
    const props = drawer({
      run: {
        ...run,
        name_hint: {
          message: 'None of the 3 videos names the show "The Joe Rogan Experience", so nothing can match.',
          alias: 'Joe Rogan Experience',
        },
      },
    })
    expect(screen.getByText('No titles match the show name')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Add "Joe Rogan Experience" as an alias' }))
    expect(screen.getByLabelText('Aliases')).toHaveValue('Kabhi Main\nJoe Rogan Experience')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    expect(props.onSave).toHaveBeenCalledWith(expect.any(Object), ['KMKT', 'Kabhi Main', 'Joe Rogan Experience'])
  })

  it('shows the number toggle on when the mapping has it', () => {
    drawer({ mapping: { ...drama, title_number: true } })
    expect(screen.getByRole('checkbox', { name: 'Match by number in the title' })).toBeChecked()
  })

  it('leaves the number toggle out for a daily show', () => {
    drawer({ mapping: { ...drama, kind: 'daily' } })
    expect(screen.queryByRole('checkbox', { name: 'Match by number in the title' })).not.toBeInTheDocument()
  })

  it('turns on the audio only version for shows and daily shows, never films', async () => {
    const props = drawer({ mapping: { ...drama, kind: 'daily' } })
    const toggle = screen.getByRole('checkbox', { name: 'Audio only version' })
    expect(toggle).not.toBeChecked()
    expect(toggle).toHaveAccessibleDescription('Adds an Audio only choice next to each episode in Jellyfin')
    await userEvent.click(toggle)
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    expect(props.onSave).toHaveBeenCalledWith(expect.objectContaining({ audio_version: true }), null)
    cleanup()

    drawer({ mapping: { ...drama, audio_version: true } })
    expect(screen.getByRole('checkbox', { name: 'Audio only version' })).toBeChecked()
    cleanup()

    drawer({ mapping: theri })
    expect(screen.queryByRole('checkbox', { name: 'Audio only version' })).not.toBeInTheDocument()
    expect(patchFrom(formFrom(theri), 'film')).not.toHaveProperty('audio_version')
  })

  it('turns on episode images for shows and daily shows, never films', async () => {
    const props = drawer({ mapping: { ...drama, kind: 'daily' } })
    const toggle = screen.getByRole('checkbox', { name: 'Episode images from YouTube' })
    expect(toggle).not.toBeChecked()
    await userEvent.click(toggle)
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    expect(props.onSave).toHaveBeenCalledWith(expect.objectContaining({ episode_thumbnails: true }), null)
    cleanup()

    drawer({ mapping: { ...drama, episode_thumbnails: true } })
    expect(screen.getByRole('checkbox', { name: 'Episode images from YouTube' })).toBeChecked()
    cleanup()

    drawer({ mapping: theri })
    expect(screen.queryByRole('checkbox', { name: 'Episode images from YouTube' })).not.toBeInTheDocument()
    expect(patchFrom(formFrom(theri), 'film')).not.toHaveProperty('episode_thumbnails')
  })

  it('names Radarr for a film and waits for the aliases to load', () => {
    drawer({ mapping: theri, aliases: undefined })
    expect(screen.getByLabelText('Aliases')).toBeDisabled()
    expect(screen.getByText(/Names from Radarr/)).toBeInTheDocument()
  })

  it('explains the built-in excludes and length floor of a show, a daily show and a film', () => {
    for (const kind of ['show', 'daily'] as const) {
      drawer({ mapping: { ...drama, kind } })
      expect(screen.getByText(/Always excluded: Teaser, Promo, Highlights/)).toBeInTheDocument()
      expect(screen.getByText('Empty means 25.')).toBeInTheDocument()
      cleanup()
    }
    drawer({ mapping: theri })
    expect(screen.getByText(/unless the word is in the film's name or aliases: Trailer, Teaser, Promo, Song/)).toBeInTheDocument()
    expect(screen.queryByText(/Highlights/)).not.toBeInTheDocument()
    expect(screen.getByText('Empty means [films] min_duration in the config, 80 by default.')).toBeInTheDocument()
  })

  it('shows where the language came from and marks an edit as yours', async () => {
    drawer({ mapping: { ...drama, language_source: 'channel' } })
    expect(screen.getByText('from channel')).toBeInTheDocument()
    const language = screen.getByLabelText('Language')
    await userEvent.clear(language)
    await userEvent.type(language, 'Punjabi')
    expect(screen.getByText('set by you')).toBeInTheDocument()
    expect(screen.queryByText('from channel')).not.toBeInTheDocument()
  })

  it('labels a language from Sonarr', () => {
    drawer({ aliases: [] })
    expect(screen.getByText('from Sonarr')).toBeInTheDocument()
  })

  it('sends the library folder override', async () => {
    const props = drawer({ mapping: { ...drama, root_folder: '/tv/old' } })
    expect(screen.getByLabelText('Move to')).toHaveValue('custom')
    expect(screen.getByLabelText('Custom folder')).toHaveValue('/tv/old')
    const save = () => userEvent.click(screen.getByRole('button', { name: 'Save' }))
    const sent = () => vi.mocked(props.onSave).mock.calls.at(-1)?.[0].root_folder

    await userEvent.selectOptions(screen.getByLabelText('Move to'), 'none')
    await save()
    expect(sent()).toBe('none')

    await userEvent.selectOptions(screen.getByLabelText('Move to'), 'follow')
    await save()
    expect(sent()).toBeNull()

    await userEvent.selectOptions(screen.getByLabelText('Move to'), 'custom')
    const path = screen.getByLabelText('Custom folder')
    await userEvent.clear(path)
    await userEvent.type(path, '/tv/punjabi')
    await save()
    expect(sent()).toBe('/tv/punjabi')
  })

  it('shows the folders and hints when the wanted root folder is missing', () => {
    drawer({
      mapping: {
        ...drama,
        wanted_root_folder: '/tv/punjabi',
        current_root_folder: '/tv/urdu',
        root_folder_result: 'missing',
        root_folder_result_at: 1760000000,
      },
    })
    expect(screen.getByText('/tv/urdu')).toBeInTheDocument()
    expect(screen.getByText('missing')).toBeInTheDocument()
    expect(screen.getByRole('status')).toHaveTextContent('Add this root folder in Sonarr')
  })

  it('gives no hint when the folder is already right', () => {
    drawer({ mapping: { ...drama, root_folder_result: 'moved', root_folder_result_at: 1760000000 } })
    expect(screen.getByText('moved')).toBeInTheDocument()
    expect(screen.queryByRole('status')).not.toBeInTheDocument()
  })

  it('tests the unsaved draft and shows the verdicts', async () => {
    const props = drawer({ run })
    await userEvent.click(screen.getByRole('button', { name: 'Test against channel' }))
    expect(props.onTest).toHaveBeenCalledWith(expect.objectContaining({ excludes: ['Recap'] }))
    expect(screen.getByText('1 matched, 1 unsure, 1 excluded. Nothing was saved.')).toBeInTheDocument()
    expect(screen.getByText('matched')).toBeInTheDocument()
    expect(screen.getByText('unsure')).toBeInTheDocument()
    expect(screen.getByText('excluded')).toBeInTheDocument()
    expect(screen.getByText(/under the 25 min floor/)).toBeInTheDocument()
  })

  it('shows an error from the API', () => {
    drawer({ error: 'episode regex needs a named group `ep` (or `episode`)' })
    expect(screen.getByRole('alert')).toHaveTextContent('named group')
  })

  it('sends only the fields a kind uses', () => {
    const daily = { ...drama, kind: 'daily' as const, utc_offset_mins: 330 }
    const patch = patchFrom(formFrom(daily), 'daily')
    expect(patch.utc_offset_mins).toBe(330)
    expect(patch.timezone).toBeNull()
    expect(patchFrom(formFrom({ ...daily, timezone: 'Asia/Kolkata' }), 'daily').timezone).toBe('Asia/Kolkata')
    expect(patch).not.toHaveProperty('episode_regex')
    const cleared = patchFrom({ ...formFrom(drama), minDurationMins: '' }, 'film')
    expect(cleared.min_duration_secs).toBeNull()
    expect(cleared).not.toHaveProperty('playlist_id')
    expect(cleared).not.toHaveProperty('backfill_limit')
    expect(cleared).not.toHaveProperty('title_number')
    expect(patch).not.toHaveProperty('title_number')
  })

  it('sends the backfill limit as a number, empty as no limit', () => {
    expect(patchFrom({ ...formFrom(drama), backfillLimit: '5' }, 'show').backfill_limit).toBe(5)
    expect(patchFrom(formFrom(drama), 'daily').backfill_limit).toBeNull()
    expect(formFrom({ ...drama, backfill_limit: 0 }).backfillLimit).toBe('0')
  })
})
