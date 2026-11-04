import { describe, expect, it } from 'vitest'
import { channelSummary, configSections, label, value } from './config'
import { duration } from './format'

describe('config labels', () => {
  it('shows intervals as friendly durations', () => {
    expect(duration(900)).toBe('15 min')
    expect(duration(86400)).toBe('24 h')
    expect(duration(4800)).toBe('80 min')
    expect(duration(45)).toBe('45 s')
    expect(value('sync_interval_secs', 900)).toBe('15 min')
    expect(value('automatic', true)).toBe('yes')
    expect(value('channels', [])).toBe('none')
  })

  it('names keys, with a fallback for keys it does not know', () => {
    expect(label('min_duration_secs')).toBe('Shortest film')
    expect(label('interval_secs', 'canary')).toBe('Check every')
    expect(label('brand_new_secs')).toBe('Brand new')
  })

  it('groups top-level values under General and tables in a fixed order', () => {
    const sections = configSections({
      listen: '0.0.0.0:8480',
      api_key: 'set',
      binaries: { check_interval_secs: 86400 },
      arr: { tag: 'youtube', sync_interval_secs: 900 },
      radarr: null,
    })
    expect(sections.map((s) => s.title)).toEqual(['General', 'Radarr', 'Sonarr and Radarr sync', 'Binaries'])
    expect(sections[0].items).toEqual([
      { term: 'Listen address', detail: '0.0.0.0:8480' },
      { term: 'API key', detail: 'set' },
    ])
    expect(sections[1].items).toEqual([{ term: 'Status', detail: 'not set' }])
    expect(sections[3].items).toEqual([{ term: 'Update check every', detail: '24 h' }])
  })

  it('shows channels and root folders as readable text, never [object Object]', () => {
    const sections = configSections({
      channels: {
        UCary: {
          network: 'ARY Digital',
          country: 'Pakistan',
          language: 'Urdu',
          utc_offset_mins: 300,
          timezone: 'America/New_York',
          root_folder: null,
          show_root_folder: '/data/media/pk-dramas',
          film_root_folder: null,
          listing_size: 150,
          assumed_height: null,
        },
        UClabel: { network: null, country: 'India', language: 'Hindi', utc_offset_mins: 330, timezone: null },
      },
      root_folders: { shows: { Urdu: '/data/media/pk-dramas' }, films: {} },
    })
    expect(sections.map((s) => s.title)).toEqual(['General', 'Channels', 'Root folders by language'])
    expect(sections[1].items).toEqual([
      {
        term: 'UCary',
        detail:
          'Network ARY Digital · Pakistan · Urdu · America/New_York\nShows in /data/media/pk-dramas · Lists the 150 newest uploads',
      },
      { term: 'UClabel', detail: 'India · Hindi · UTC+05:30' },
    ])
    expect(sections[2].items).toEqual([
      { term: 'Shows', detail: 'Urdu: /data/media/pk-dramas' },
      { term: 'Films', detail: 'none' },
    ])
    expect(channelSummary({ country: 'United States', language: 'English', utc_offset_mins: -360, assumed_height: 0 })).toBe(
      'United States · English · UTC-06:00\nWaits for a lookup for the height',
    )
    expect(configSections({ channels: {} })[1].items).toEqual([{ term: 'Status', detail: 'none' }])
    for (const section of sections) {
      for (const item of section.items) expect(item.detail).not.toContain('[object Object]')
    }
  })
})
