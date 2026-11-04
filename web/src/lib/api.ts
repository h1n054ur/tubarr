// The admin API client. The browser sends the session cookie (same origin) and every request
// carries X-Requested-With: tubarr, which the server wants on state-changing calls so that another
// site cannot make them.

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
    public url: string,
    /** Seconds from a 429's Retry-After. */
    public retryAfter: number | null = null,
  ) {
    super(message)
  }
}

function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  return send<T>(method, `/admin/api${path}`, body)
}

export async function send<T>(method: string, url: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { 'X-Requested-With': 'tubarr' }
  if (body !== undefined) headers['Content-Type'] = 'application/json'
  const res = await fetch(url, {
    method,
    headers,
    credentials: 'same-origin',
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  // 204 answers have no body.
  const data = res.status === 204 ? null : await res.json().catch(() => null)
  if (!res.ok) {
    const retry = Number(res.headers.get('Retry-After'))
    throw new ApiError(
      res.status,
      (data && data.error) || `${res.status} ${res.statusText}`,
      url,
      Number.isFinite(retry) && retry > 0 ? retry : null,
    )
  }
  return data as T
}

export type Kind = 'show' | 'daily' | 'film'
/** The list's kind filter: a kind, or the podcasts among the shows (which `show` keeps too). */
export type KindFilter = Kind | 'podcast'

/** What a show's playlist is for: more videos whose titles give the number, or the episode order. */
export type PlaylistMode = 'source' | 'order'

export interface Mapping {
  id: number
  kind: Kind
  /** A show matched by its running number, as the podcast preset sets it. Its kind stays `show`. */
  is_podcast: boolean
  active: boolean
  title: string
  year: number | null
  tvdb_id: number | null
  tmdb_id: number | null
  imdb_id: string | null
  arr_id: number | null
  language: string
  language_source: LanguageSource
  channel_ids: string[]
  /** Channel names by id, for the channels Tubarr resolved from a handle or link. */
  channel_titles: Record<string, string>
  episode_regex: string | null
  excludes: string[]
  min_duration_secs: number | null
  utc_offset_mins: number | null
  /** An IANA time zone, over `utc_offset_mins`. */
  timezone: string | null
  playlist_id: string | null
  playlist_season: number
  playlist_offset: number
  playlist_mode: PlaylistMode
  /** Back episodes released: the newest this many uploaded before the mapping was added. Null releases all. */
  backfill_limit: number | null
  /** Shows: match the #N in a video title against the number in TheTVDB's episode titles. */
  title_number: boolean
  audio_version: boolean
  /** Shows and daily shows: the YouTube thumbnail as each episode's image in Jellyfin. */
  episode_thumbnails: boolean
  releases: number
  unmatched: number
  created_at: number
  updated_at: number
  /** The user's root folder override: a path, "none" (never move), or null (follow the config). */
  root_folder: string | null
  /** Where the title should live: the override, else its channel's, else its language's folder. */
  wanted_root_folder: string | null
  /** The root folder the arr reported at the last sync. */
  current_root_folder: string | null
  /** "moved", "missing" (the arr has no such root folder) or "failed: ...". */
  root_folder_result: string | null
  root_folder_result_at: number | null
}

/** Who set a mapping's language: the arr, the `[channels]` config, or the admin UI. */
export type LanguageSource = 'arr' | 'channel' | 'user'

/** An alias and whose it is: the arr sync's (read only here) or the user's. */
export interface Alias {
  alias: string
  source: 'arr' | 'user'
}

/** The Tubarr-owned fields. A missing field is left alone, null clears it. */
export interface MappingPatch {
  language?: string
  channel_ids?: string[]
  episode_regex?: string | null
  excludes?: string[]
  min_duration_secs?: number | null
  utc_offset_mins?: number | null
  timezone?: string | null
  playlist_id?: string | null
  playlist_season?: number
  playlist_offset?: number
  playlist_mode?: PlaylistMode
  backfill_limit?: number | null
  title_number?: boolean
  audio_version?: boolean
  episode_thumbnails?: boolean
  root_folder?: string | null
}

export type Verdict = 'matched' | 'unsure' | 'excluded'

export interface VideoVerdict {
  video_id: string
  channel_id: string
  title: string
  published_at: number
  duration_secs: number | null
  verdict: Verdict
  reason: string
  season: number | null
  episode: number | null
  part: number | null
}

export interface DryRun {
  mapping_id: number
  counts: Record<Verdict, number>
  videos: VideoVerdict[]
  /** Set when no title on the channel names the show, which the rules need. */
  name_hint: { message: string; alias: string | null } | null
}

export interface MappingFilter {
  kind?: KindFilter
  active?: boolean
}

export type UnmatchedState = 'pending' | 'duplicate' | 'unsure' | 'partial' | 'capped'

export interface UnmatchedNote {
  id: number
  mapping_id: number
  mapping_title: string
  mapping_kind: Kind
  video_id: string
  video_title: string | null
  channel_id: string | null
  duration_secs: number | null
  published_at: number | null
  upload_date: string | null
  state: UnmatchedState
  season: number | null
  episode: number | null
  part: number | null
  reason: string
  ignored: boolean
  updated_at: number
}

export interface UnmatchedFilter {
  state?: UnmatchedState
  mapping_id?: number
  ignored?: boolean
  /** Text the video title, show title, video id or channel id contains, any case. */
  q?: string
}

/** A server-side page: `limit` 1 to 100 (25 when left out), `offset` from 0. */
export interface PageParams {
  limit?: number
  offset?: number
}

/** One page of rows and how many match the filter in all. */
export interface Paged<T> {
  items: T[]
  total: number
}

export interface NewPin {
  mapping_id: number
  video_id: string
  season?: number
  episode?: number
  air_date?: string
}

export interface Pin {
  id: number
  mapping_id: number
  video_id: string
  season: number | null
  episode: number | null
  air_date: string | null
  created_at: number
}

export interface Grab {
  nzo_id: string
  release_name: string
  video_id: string
  category: string
  status: 'completed' | 'failed'
  storage_path: string
  created_at: number
}

export type ActivityEvent =
  | { kind: 'search'; at: number; function: string; query: string; results: number }
  | { kind: 'play'; at: number; video_ids: string; client: string | null; status: number }
  | {
      kind: 'arr_push'
      at: number
      app: string
      release: string
      outcome: 'grabbed' | 'pending' | 'rejected' | 'failed'
      reasons: string[]
    }

/** What the catch-up push queued: new targets, and everything now waiting. */
export interface PushReleased {
  queued: number
  waiting: number
}

export interface Activity {
  /** One page of grabs, newest first. */
  grabs: Grab[]
  /** How many grabs the category has in all. */
  total: number
}

/** One page of events, newest first, and how long the server keeps them (0: no limit). */
export interface ActivityEvents extends Paged<ActivityEvent> {
  retention_secs: number
}

export type Tool = 'yt-dlp' | 'deno'
export type Step = 'plain' | 'po_token' | 'cookies'

export interface MatchCounts {
  released: number
  pending: number
  /** Duplicates, unsure titles, parts. */
  unmatched: number
  /** Back episodes the backfill limit leaves out on purpose. */
  capped: number
}

export interface Health {
  /** The running Tubarr and the commit it was built from, when the build passed one in. */
  version: { version: string; git_sha: string | null }
  status: 'ok' | 'degraded' | 'failing'
  /** Why the status is not ok, one short line each. */
  reasons: string[]
  binaries: {
    tool: Tool
    installed: string | null
    latest: string | null
    last_check: number | null
    last_result: string | null
    can_roll_back: boolean
  }[]
  limiter: { available: number; queued: number; next_in_secs: number; paused_for_secs: number | null }
  canary: {
    enabled: boolean
    video_id: string | null
    interval_secs: number
    failures: number
    skipped: number
    last_forced_update: number | null
    history: { at: number; ok: boolean; step: Step | null; error: string | null }[]
  }
  fallback: { step: Step; configured: boolean; attempts: number; succeeded: number; failed: number }[]
  sticky: { step: Step; until: number; remaining_secs: number } | null
  watcher: {
    active: boolean
    channels: {
      channel_id: string
      last_feed_at: number | null
      last_feed_error: string | null
      feed_failures: number
      feed_failing_since: number | null
      /** The feed has failed for longer than one interval: short listings stand in until it works. */
      feed_fallback: boolean
      last_fallback_at: number | null
      last_fallback_error: string | null
      last_listing_at: number | null
      last_listing_error: string | null
      videos_seen: number
      videos_new: number
    }[]
  }
  /** Channel search backfills of shows without a playlist. */
  backfills: {
    mapping_id: number
    title: string
    last_run_at: number | null
    /** Null once done or past its days. In the past means due now. */
    next_run_at: number | null
    results_found: number | null
    done: boolean
    last_error: string | null
  }[]
  resolver: { cache_hits: number; lookups: number; failures: number }
  matcher: {
    last_run_at: number | null
    last_error: string | null
    counts: MatchCounts
    lookups: number
    skipped: [number, string][]
    /** Mappings the last run matched, and the ones it left alone since nothing they read changed. */
    matched: number
    unchanged: number
    films: { wanted: number; counts: MatchCounts; radarr_error: string | null }
  }
  /** Audio only versions waiting to be merged into (or split from) their episodes in Jellyfin. Null without `[jellyfin]`. */
  jellyfin: { queued: number; last_error: string | null; last_error_at: number | null } | null
  /** Rows and files in the stores that grow with the library's history. */
  storage: {
    notes: number
    channel_videos: number
    grabs: number
    activity_events: number
    play_index: { files: number; bytes: number }
  }
  /** First plays building and plays streaming right now. `max_streams` 0 means no cap. */
  play: { builds: number; builds_waiting: number; max_builds: number; streams: number; max_streams: number }
}

export interface CheckResult {
  tool: Tool
  ok: boolean
  result: string
}

/** Settings as Tubarr runs with them. Secrets come only as "set" or "not set". */
export type Settings = Record<string, unknown> & {
  sonarr: { url: string; api_key: string } | null
  radarr: { url: string; api_key: string } | null
  arr: { tag: string; sync_interval_secs: number }
  films: { channels: string[]; min_duration_secs: number; similarity: number }
  /** The `[channels]` config by channel id. */
  channels?: Record<string, { utc_offset_mins: number; timezone?: string | null }>
  auth?: AuthSummary
}

export interface User {
  id: number
  username: string
  display_name: string | null
  role: 'admin' | 'viewer'
  oidc_issuer: string | null
  oidc_subject: string | null
  created_at: number
  last_login_at: number | null
  disabled: boolean
  has_password: boolean
}

/** A local user takes a password, an SSO user the subject at the configured provider, or both. */
export interface NewUser {
  username: string
  password?: string
  oidc_subject?: string
}

/** How people sign in, as /admin/api/settings reports it. */
export interface AuthSummary {
  password: boolean
  oidc: boolean
  local_bypass: boolean
  session_ttl_secs: number
  session_max_age_secs: number
  oidc_issuer: string | null
  allowed_groups: string[] | null
  auto_create_users: boolean | null
  play_allowed_networks: string[]
  play_networks_default: boolean
  trusted_proxies: string[]
}

export type ArrTest = { ok: true; tag: string; tag_id: number } | { ok: false; tag: string; error: string }

export type ArrApp = 'sonarr' | 'radarr'

/** Where an arr connection comes from. The config file wins and is read only here. Never the key. */
export interface ArrConnection {
  source: 'config' | 'saved' | null
  url: string | null
  api_key: 'set' | 'not set'
  /** The arr's version, in the answer to a save. */
  version?: string
}

export type ArrConnections = Record<ArrApp, ArrConnection>

/** A save: an empty or missing key keeps the one already saved. */
export interface ArrConnectionInput {
  url: string
  api_key?: string
}

export interface WiringItem {
  key: string
  label: string
  /** "optional": missing, but only needed for a feature that is off. */
  state: 'ok' | 'missing' | 'optional'
  detail: string
}

export type WiringCheck = { ok: true; items: WiringItem[] } | { ok: false; error: string }

/** Starting rules for a new show. A podcast matches by #N, keeps the 10 newest back episodes. */
export type Preset = 'drama' | 'podcast'

export interface NewManualMapping {
  kind: Kind
  title: string
  tvdb_id?: number
  tmdb_id?: number
  language: string
  channel_ids: string[]
  preset?: Preset
  /** An IANA time zone. Needed with the podcast preset when the channel is not in `[channels]`. */
  timezone?: string
}

/** Show (TheTVDB) or film (TMDB). */
export type DraftKind = 'show' | 'film'

/** The answer to "is it there?" when a draft was made or a title checked. */
export type DraftVerdict = 'found' | 'possible' | 'probably_missing' | 'not_checked' | 'unknown'

export interface DraftSummary {
  id: number
  kind: DraftKind
  title: string
  verdict: DraftVerdict | null
  /** Shows only: the episodes drafted. */
  episodes: number | null
  created_at: number
}

export interface SiteKey {
  ready: boolean
  reason?: string
}

export interface DraftList extends Paged<DraftSummary> {
  /** Whether the check can ask TheTVDB and TMDB, or why not. */
  keys: { tvdb: SiteKey; tmdb: SiteKey }
}

export interface DraftFilter {
  kind?: DraftKind
  q?: string
}

export interface FoundTitle {
  source: string
  id: string
  name: string
  year: number | null
  url: string
  score: number
}

export interface SourceReport {
  source: string
  state: 'checked' | 'not_checked' | 'failed'
  reason?: string
  candidates: FoundTitle[]
}

export interface DraftCheck {
  kind: DraftKind
  title: string
  verdict: DraftVerdict
  best: FoundTitle | null
  sources: SourceReport[]
  summary: string
}

export interface CheckRequest {
  kind: DraftKind
  title: string
  original_title?: string
  year?: number
}

export interface ShowDraftRequest {
  channel_id: string
  title: string
  playlist_id?: string
  regex?: string
  season?: number
  preset?: Preset
  utc_offset_mins?: number
  timezone?: string
}

export interface FilmDraftRequest {
  video_id: string
}

/** A stored draft. `draft` holds the drafted fields, `text` the block to copy into the site's form. */
export interface StoredDraft {
  id: number
  kind: DraftKind
  title: string
  request: ShowDraftRequest | FilmDraftRequest
  draft: Record<string, unknown> & { check: Omit<DraftCheck, 'summary'> }
  text: string
  created_at: number
}

function query(params: Record<string, string | number | boolean | undefined>) {
  const q = new URLSearchParams()
  for (const [k, v] of Object.entries(params)) if (v !== undefined && v !== '') q.set(k, String(v))
  const qs = q.toString()
  return qs ? `?${qs}` : ''
}

/** The mapping export file. The UI passes it through as read, the server checks it. */
export interface MappingExport {
  format: number
  exported_at: number
  mappings: unknown[]
}

export interface ImportItem {
  title: string
  kind: string
  tvdb_id: number | null
  tmdb_id: number | null
}

/** What an import did, or would do on a dry run. Mappings in `conflicts` are left as they are. */
export interface ImportReport {
  dry_run: boolean
  creates: ImportItem[]
  updates: ImportItem[]
  unchanged: ImportItem[]
  conflicts: (Omit<ImportItem, 'kind'> & { reason: string })[]
}

/** The export download. A plain link: the browser sends the session cookie with a GET. */
export const mappingExportUrl = '/admin/api/mappings/export'

export const api = {
  mappings(filter: MappingFilter = {}) {
    const q = new URLSearchParams()
    if (filter.kind) q.set('kind', filter.kind)
    if (filter.active !== undefined) q.set('active', String(filter.active))
    const qs = q.toString()
    return request<Mapping[]>('GET', `/mappings${qs ? `?${qs}` : ''}`)
  },
  mapping(id: number) {
    return request<Mapping>('GET', `/mappings/${id}`)
  },
  patchMapping(id: number, patch: MappingPatch) {
    return request<Mapping>('PATCH', `/mappings/${id}`, patch)
  },
  testMapping(id: number, draft: MappingPatch) {
    return request<DryRun>('POST', `/mappings/${id}/test`, draft)
  },
  createMapping(mapping: NewManualMapping) {
    return request<Mapping>('POST', '/mappings', mapping)
  },
  importMappings(file: MappingExport, dryRun: boolean) {
    return request<ImportReport>('POST', `/mappings/import${dryRun ? '?dry_run=true' : ''}`, file)
  },
  unmatched(filter: UnmatchedFilter & PageParams = {}) {
    return request<Paged<UnmatchedNote>>('GET', `/unmatched${query({ ...filter })}`)
  },
  setIgnored(id: number, ignored: boolean) {
    return request<null>('POST', `/unmatched/${id}/ignore`, { ignored })
  },
  pins(mappingId: number) {
    return request<Pin[]>('GET', `/pins${query({ mapping_id: mappingId })}`)
  },
  pin(pin: NewPin) {
    return request<Pin>('POST', '/pins', pin)
  },
  unpin(id: number) {
    return request<null>('DELETE', `/pins/${id}`)
  },
  activity(category?: string, page: PageParams = {}) {
    return request<Activity>('GET', `/activity${query({ category, ...page })}`)
  },
  activityEvents(kind?: ActivityEvent['kind'], page: PageParams = {}) {
    return request<ActivityEvents>('GET', `/activity/events${query({ kind, ...page })}`)
  },
  health() {
    return send<Health>('GET', '/api/health')
  },
  checkBinaries() {
    return request<CheckResult[]>('POST', '/binaries/check')
  },
  rollback(tool: Tool) {
    return request<{ tool: Tool; rolled_back: boolean }>('POST', `/binaries/${tool}/rollback`)
  },
  settings() {
    return request<Settings>('GET', '/settings')
  },
  /** Catch-up: queues a push to the arr for every episode and film Tubarr has a release for. */
  pushReleased() {
    return request<PushReleased>('POST', '/arr/push-released')
  },
  testArr(app: ArrApp) {
    return request<ArrTest>('POST', `/arr/${app}/test`)
  },
  arrConnections() {
    return request<ArrConnections>('GET', '/arr/connections')
  },
  /** Tests the URL and key against the arr and saves them only when it answers. */
  saveArr(app: ArrApp, input: ArrConnectionInput) {
    return request<ArrConnection>('PUT', `/arr/${app}/connection`, input)
  },
  removeArr(app: ArrApp) {
    return request<ArrConnection>('DELETE', `/arr/${app}/connection`)
  },
  /** Read only: what Tubarr needs in the arr and whether each is there. */
  arrWiring(app: ArrApp) {
    return request<WiringCheck>('POST', `/arr/${app}/wiring`)
  },
  aliases(mappingId: number) {
    return request<Alias[]>('GET', `/mappings/${mappingId}/aliases`)
  },
  /** Replaces the whole list: an arr alias left out is dropped until the next sync. */
  setAliases(mappingId: number, aliases: string[]) {
    return request<Alias[]>('PUT', `/mappings/${mappingId}/aliases`, { aliases })
  },
  drafts(filter: DraftFilter & PageParams = {}) {
    return request<DraftList>('GET', `/drafts${query({ ...filter })}`)
  },
  draft(id: number) {
    return request<StoredDraft>('GET', `/drafts/${id}`)
  },
  checkTitle(check: CheckRequest) {
    return request<DraftCheck>('POST', '/drafts/check', check)
  },
  draftShow(req: ShowDraftRequest) {
    return request<StoredDraft>('POST', '/drafts/show', req)
  },
  draftFilm(req: FilmDraftRequest) {
    return request<StoredDraft>('POST', '/drafts/film', req)
  },
  deleteDraft(id: number) {
    return request<null>('DELETE', `/drafts/${id}`)
  },
  users() {
    return request<User[]>('GET', '/users')
  },
  createUser(user: NewUser) {
    return request<User>('POST', '/users', user)
  },
  deleteUser(id: number) {
    return request<null>('DELETE', `/users/${id}`)
  },
}
