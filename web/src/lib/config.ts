// Friendly names for the effective config from /admin/api/settings. Keys not listed here still show,
// with a name made from the key, so a new config key never disappears from the page.
import { duration } from './format'

const sectionTitles: Record<string, string> = {
  sonarr: 'Sonarr',
  radarr: 'Radarr',
  arr: 'Sonarr and Radarr sync',
  films: 'Films',
  matcher: 'Matcher',
  newznab: 'Newznab answers',
  watcher: 'Channel watcher',
  rate_limit: 'YouTube rate limit',
  fallback: 'Fallback steps',
  canary: 'Canary',
  binaries: 'Binaries',
  subtitles: 'Subtitles',
  notify: 'Notifications',
  channels: 'Channels',
  root_folders: 'Root folders by language',
}

const labels: Record<string, string> = {
  listen: 'Listen address',
  public_url: 'Public URL',
  data_dir: 'Data folder',
  complete_dir: 'Completed downloads folder',
  cache_ttl_secs: 'Lookups cached for',
  lookup_timeout_secs: 'Lookup timeout',
  api_key: 'API key',
  url: 'Address',
  tag: 'Tag',
  sync_interval_secs: 'Sync every',
  channels: 'Official film channels',
  min_duration_secs: 'Shortest film',
  similarity: 'Title similarity needed',
  lookups_per_run: 'Lookups per run',
  assumed_height: 'Height assumed until looked up',
  report_size: 'Send release sizes',
  rss_interval_secs: 'Feed check every',
  listing_interval_secs: 'Full listing every',
  listing_size: 'Videos per listing',
  fallback_listing_size: 'Videos per fallback listing',
  fallback_per_hour: 'Fallback listings per hour',
  backfill_days: 'Search back episodes for (days)',
  backfill_interval_secs: 'Search back episodes every',
  lookups: 'Lookups per interval',
  burst: 'Burst',
  jitter_secs: 'Jitter',
  throttle_pause_secs: 'Pause after YouTube pushes back',
  po_token_url: 'PO token provider',
  plugin_dir: 'Plugin folder',
  cookies_file: 'Cookies file',
  sticky_for_secs: 'Stay on a fallback step for',
  video_id: 'Video',
  bin_dir: 'Binaries folder',
  yt_dlp: 'yt-dlp path',
  deno: 'deno path',
  yt_dlp_channel: 'yt-dlp release channel',
  check_interval_secs: 'Update check every',
  languages: 'Languages',
  automatic: 'Automatic captions',
  kind: 'Kind',
  webhook: 'Webhook',
  token: 'Token',
}

/** `interval_secs` means a different thing in each section. */
const intervalLabels: Record<string, string> = {
  rate_limit: 'Interval',
  canary: 'Check every',
  matcher: 'Runs every',
}

const order = Object.keys(sectionTitles)

function fromKey(key: string): string {
  const words = key.replace(/_secs$/, '').replace(/_/g, ' ')
  return words.charAt(0).toUpperCase() + words.slice(1)
}

export function label(key: string, section?: string): string {
  if (key === 'interval_secs' && section) return intervalLabels[section] ?? 'Interval'
  return labels[key] ?? fromKey(key)
}

export function value(key: string, v: unknown): string {
  if (v === null || v === undefined) return 'not set'
  if (Array.isArray(v)) return v.length === 0 ? 'none' : v.map((x) => value('', x)).join(', ')
  if (typeof v === 'boolean') return v ? 'yes' : 'no'
  if (typeof v === 'number' && key.endsWith('_secs')) return duration(v)
  // A nested table, like a [root_folders] kind: one "key: value" line each.
  if (isSection(v)) {
    const lines = Object.entries(v).map(([k, x]) => `${label(k)}: ${value(k, x)}`)
    return lines.length === 0 ? 'none' : lines.join('\n')
  }
  return String(v)
}

/** "UTC+05:00" from an offset in minutes. */
function utcOffset(mins: number): string {
  const abs = Math.abs(mins)
  const pad = (n: number) => String(n).padStart(2, '0')
  return `UTC${mins < 0 ? '-' : '+'}${pad(Math.floor(abs / 60))}:${pad(abs % 60)}`
}

/** One `[channels]` entry: who and where on the first line, its overrides on the next. */
export function channelSummary(v: unknown): string {
  if (!isSection(v)) return value('', v)
  const text = (k: string) => (typeof v[k] === 'string' && v[k] !== '' ? (v[k] as string) : null)
  const num = (k: string) => (typeof v[k] === 'number' ? (v[k] as number) : null)
  const offset = num('utc_offset_mins')
  const zone = text('timezone') ?? (offset === null ? null : utcOffset(offset))
  const network = text('network')
  const about = [network && `Network ${network}`, text('country'), text('language'), zone].filter(Boolean)
  const height = num('assumed_height')
  const overrides = [
    text('root_folder') && `Root folder ${text('root_folder')}`,
    text('show_root_folder') && `Shows in ${text('show_root_folder')}`,
    text('film_root_folder') && `Films in ${text('film_root_folder')}`,
    num('listing_size') !== null && `Lists the ${num('listing_size')} newest uploads`,
    height !== null && (height === 0 ? 'Waits for a lookup for the height' : `Named ${height}p until looked up`),
  ].filter(Boolean)
  return [about.join(' · '), overrides.join(' · ')].filter((line) => line !== '').join('\n')
}

export interface ConfigSection {
  title: string
  items: { term: string; detail: string }[]
}

function isSection(v: unknown): v is Record<string, unknown> {
  return v !== null && typeof v === 'object' && !Array.isArray(v)
}

function tableItems(section: string, table: Record<string, unknown>): ConfigSection['items'] {
  const items = Object.entries(table).map(([key, x]) =>
    // A channel's id is its name; everything else gets a friendly label.
    section === 'channels' ? { term: key, detail: channelSummary(x) } : { term: label(key, section), detail: value(key, x) },
  )
  return items.length === 0 ? [{ term: 'Status', detail: 'none' }] : items
}

/** Top-level values under "General", then one section per config table. A table that is off shows "not set". */
export function configSections(settings: Record<string, unknown>): ConfigSection[] {
  // Sign-in has its own block in the Users section.
  const entries = Object.entries(settings).filter(([k]) => k !== 'auth')
  const general = entries.filter(([k, v]) => !isSection(v) && !(k in sectionTitles))
  const tables = entries
    .filter(([k, v]) => isSection(v) || k in sectionTitles)
    .sort(([a], [b]) => (order.indexOf(a) + 1 || 99) - (order.indexOf(b) + 1 || 99))

  return [
    { title: 'General', items: general.map(([k, v]) => ({ term: label(k), detail: value(k, v) })) },
    ...tables.map(([k, v]) => ({
      title: sectionTitles[k] ?? fromKey(k),
      items: isSection(v)
        ? tableItems(k, v)
        : [{ term: 'Status', detail: 'not set' }],
    })),
  ]
}
