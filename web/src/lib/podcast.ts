import type { Settings } from './api'

/** The podcast preset's rules, as the server sets them (crates/tubarr/src/matcher/preset.rs). */
export const podcastDefaults = { backfillLimit: 10, minDurationMins: 20, excludes: ['Clip', 'Best of'] }

/** The time zones offered first. Any other IANA name may be typed in. */
export const timeZones = [
  'America/Chicago',
  'America/Los_Angeles',
  'America/New_York',
  'Asia/Karachi',
  'Asia/Kolkata',
  'Australia/Sydney',
  'Europe/London',
]

/** A listed channel's zone: its time zone, else its fixed offset in minutes. */
export type ChannelZone = { timezone: string } | { offset: number }

/** The zone `[channels]` gives the first listed channel, or null when none is listed. */
export function channelZone(channels: Settings['channels'], ids: string[]): ChannelZone | null {
  for (const id of ids) {
    const listed = channels?.[id]
    if (listed) return listed.timezone ? { timezone: listed.timezone } : { offset: listed.utc_offset_mins }
  }
  return null
}

export function zoneLabel(zone: ChannelZone): string {
  return 'timezone' in zone ? zone.timezone : `UTC offset ${zone.offset} minutes`
}

export function zoneHelp(configured: ChannelZone | null): string {
  return configured === null
    ? 'This channel is not in [channels], so pick its time zone, like America/Chicago for a US show. Pakistan is not assumed.'
    : `From [channels]: ${zoneLabel(configured)}. Pick one only to use another.`
}
