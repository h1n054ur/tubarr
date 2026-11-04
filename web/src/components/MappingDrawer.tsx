// The rules editor, ported from Tailwind Plus application-ui/overlays/drawers/create-project-form-example.
import { useState, type FormEvent } from 'react'
import { Dialog, DialogPanel, DialogTitle } from '@headlessui/react'
import { XMarkIcon } from '@heroicons/react/24/outline'
import type { Alias, DryRun, LanguageSource, Mapping, MappingPatch, PlaylistMode, Settings, Verdict } from '../lib/api'
import { when } from '../lib/format'
import { channelZone, podcastDefaults, zoneHelp, type ChannelZone } from '../lib/podcast'
import { Alert } from './Alert'
import { Badge, type Tone } from './Badge'
import { Field, Select, TimeZoneInput, Toggle, inputClass, secondaryButton } from './fields'
import { kindLabel } from './MappingsTable'

type RootMode = 'follow' | 'none' | 'custom'

export interface RulesForm {
  language: string
  channelIds: string
  episodeRegex: string
  excludes: string
  minDurationMins: string
  utcOffsetMins: string
  timezone: string
  playlistId: string
  playlistSeason: string
  playlistOffset: string
  playlistMode: PlaylistMode
  backfillLimit: string
  titleNumber: boolean
  audioVersion: boolean
  episodeThumbnails: boolean
  rootMode: RootMode
  rootPath: string
}

export function formFrom(m: Mapping): RulesForm {
  return {
    language: m.language,
    channelIds: m.channel_ids.join('\n'),
    episodeRegex: m.episode_regex ?? '',
    excludes: m.excludes.join('\n'),
    minDurationMins: m.min_duration_secs ? String(Math.round(m.min_duration_secs / 60)) : '',
    utcOffsetMins: m.utc_offset_mins === null ? '' : String(m.utc_offset_mins),
    timezone: m.timezone ?? '',
    playlistId: m.playlist_id ?? '',
    playlistSeason: String(m.playlist_season),
    playlistOffset: String(m.playlist_offset),
    playlistMode: m.playlist_mode,
    backfillLimit: m.backfill_limit === null ? '' : String(m.backfill_limit),
    titleNumber: m.title_number,
    audioVersion: m.audio_version,
    episodeThumbnails: m.episode_thumbnails,
    rootMode: m.root_folder === null ? 'follow' : m.root_folder === 'none' ? 'none' : 'custom',
    rootPath: m.root_folder !== null && m.root_folder !== 'none' ? m.root_folder : '',
  }
}

/** The root folder override to save: null follows the channel or language, "none" never moves. */
export function rootFolderFrom(form: RulesForm): string | null {
  if (form.rootMode === 'none') return 'none'
  if (form.rootMode === 'custom') return form.rootPath.trim() || null
  return null
}

function arrName(kind: Mapping['kind']) {
  return kind === 'film' ? 'Radarr' : 'Sonarr'
}

const languageSourceLabel = (source: LanguageSource, kind: Mapping['kind']) =>
  source === 'arr' ? `from ${arrName(kind)}` : source === 'channel' ? 'from channel' : 'set by you'

/** Aliases are names, which may hold commas, so only a new line splits them. */
function aliasLines(text: string): string[] {
  return text
    .split('\n')
    .map((s) => s.trim())
    .filter(Boolean)
}

function lines(text: string): string[] {
  return text
    .split(/[\n,]/)
    .map((s) => s.trim())
    .filter(Boolean)
}

function numberOrNull(text: string): number | null {
  const t = text.trim()
  return t === '' ? null : Number(t)
}

/** Every field the form shows, so a save or a test sends the whole draft. */
export function patchFrom(form: RulesForm, kind: Mapping['kind']): MappingPatch {
  const mins = numberOrNull(form.minDurationMins)
  const patch: MappingPatch = {
    language: form.language.trim(),
    channel_ids: lines(form.channelIds),
    excludes: lines(form.excludes),
    min_duration_secs: mins === null ? null : Math.round(mins * 60),
  }
  if (kind === 'show') {
    patch.episode_regex = form.episodeRegex.trim() || null
    patch.playlist_id = form.playlistId.trim() || null
    patch.playlist_season = Number(form.playlistSeason || 1)
    patch.playlist_offset = Number(form.playlistOffset || 1)
    patch.playlist_mode = form.playlistMode
    patch.title_number = form.titleNumber
  }
  if (usesOffset(form, kind)) {
    patch.timezone = form.timezone.trim() || null
    patch.utc_offset_mins = numberOrNull(form.utcOffsetMins)
  }
  if (kind !== 'film') {
    patch.backfill_limit = numberOrNull(form.backfillLimit)
    patch.audio_version = form.audioVersion
    patch.episode_thumbnails = form.episodeThumbnails
  }
  return patch
}

/** Daily shows and shows numbered by #N date their uploads by the channel's day. */
function usesOffset(form: RulesForm, kind: Mapping['kind']) {
  return kind === 'daily' || (kind === 'show' && form.titleNumber)
}

/** The podcast preset over the form: keeps the user's own excludes and zone, else takes the channel's zone. */
export function withPodcastDefaults(form: RulesForm, configured: ChannelZone | null): RulesForm {
  const zoneSet = form.timezone.trim() !== '' || form.utcOffsetMins.trim() !== ''
  const excludes = lines(form.excludes)
  for (const word of podcastDefaults.excludes)
    if (!excludes.some((e) => e.toLowerCase() === word.toLowerCase())) excludes.push(word)
  return {
    ...form,
    titleNumber: true,
    episodeThumbnails: true,
    backfillLimit: form.backfillLimit || String(podcastDefaults.backfillLimit),
    minDurationMins: String(podcastDefaults.minDurationMins),
    excludes: excludes.join('\n'),
    ...(zoneSet || configured === null
      ? {}
      : 'timezone' in configured
        ? { timezone: configured.timezone }
        : { utcOffsetMins: String(configured.offset) }),
  }
}

// The matcher's built-in lists and floors (matcher/title.rs for shows and daily shows, matcher/films.rs
// for films).
const excludesHelp = {
  show:
    'Title words that rule a video out, one per line. Always excluded: Teaser, Promo, Highlights, Trailer, Best Scene, BTS (behind the scenes), Review, Reaction and Shorts.',
  film:
    "Title words that rule a video out, one per line. Always excluded, unless the word is in the film's name or aliases: Trailer, Teaser, Promo, Song, Jukebox, Scene, Making, Review, Reaction, Interview, Climax, Dialogue, Shorts, Status and Spoof.",
}

const minDurationHelp = {
  show: 'Empty means 25.',
  film: 'Empty means [films] min_duration in the config, 80 by default.',
}

const verdictTone: Record<Verdict, Tone> = { matched: 'green', unsure: 'yellow', excluded: 'gray' }

function minutes(secs: number | null) {
  return secs === null ? 'length unknown' : `${Math.round(secs / 60)} min`
}

export function DryRunResults({ run, onAddAlias }: { run: DryRun; onAddAlias?: (alias: string) => void }) {
  const hint = run.name_hint
  return (
    <div className="pt-6 pb-6">
      <h3 className="text-sm/6 font-medium text-ink">Test against channel</h3>
      <p className="mt-1 text-sm text-muted">
        {run.counts.matched} matched, {run.counts.unsure} unsure, {run.counts.excluded} excluded. Nothing was saved.
      </p>
      {hint && (
        <div className="mt-4">
          <Alert tone="info" title="No titles match the show name">
            <p>{hint.message}</p>
            {hint.alias && onAddAlias && (
              <button type="button" onClick={() => onAddAlias(hint.alias!)} className={`mt-3 ${secondaryButton}`}>
                Add "{hint.alias}" as an alias
              </button>
            )}
          </Alert>
        </div>
      )}
      {run.videos.length === 0 ? (
        <p className="mt-4 text-sm text-muted">No videos known on these channels yet.</p>
      ) : (
        <ul role="list" className="mt-4 divide-y divide-line">
          {run.videos.map((v) => (
            <li key={v.video_id} className="py-3">
              <div className="flex items-start justify-between gap-x-3">
                <p className="text-sm font-medium text-ink">{v.title}</p>
                <Badge tone={verdictTone[v.verdict]}>{v.verdict}</Badge>
              </div>
              <p className="mt-1 text-xs text-muted">
                {v.reason} · {minutes(v.duration_secs)} · {v.video_id}
              </p>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

export function MappingDrawer({
  mapping,
  aliases,
  onClose,
  onSave,
  onTest,
  saving,
  testing,
  error,
  run,
  channels,
}: {
  mapping: Mapping | null
  onClose: () => void
  /** Undefined while loading. */
  aliases: Alias[] | undefined
  /** `aliases` is the full list to save, or null when the user left them alone. */
  onSave: (patch: MappingPatch, aliases: string[] | null) => void
  onTest: (patch: MappingPatch) => void
  saving: boolean
  testing: boolean
  error: string | null
  run: DryRun | null
  /** The `[channels]` config, for the podcast defaults' time zone. */
  channels?: Settings['channels']
}) {
  return (
    <Dialog open={mapping !== null} onClose={onClose} className="relative z-50">
      <div className="fixed inset-0 bg-canvas/60" />
      <div className="fixed inset-0 overflow-hidden">
        <div className="absolute inset-0 overflow-hidden">
          <div className="pointer-events-none fixed inset-y-0 right-0 flex max-w-full pl-10 sm:pl-16">
            <DialogPanel
              transition
              className="pointer-events-auto w-screen max-w-xl transform transition duration-500 ease-in-out data-closed:translate-x-full sm:duration-700"
            >
              {mapping && (
                <RulesEditor
                  key={mapping.id}
                  mapping={mapping}
                  aliases={aliases}
                  onClose={onClose}
                  onSave={onSave}
                  onTest={onTest}
                  saving={saving}
                  testing={testing}
                  error={error}
                  run={run}
                  channels={channels}
                />
              )}
            </DialogPanel>
          </div>
        </div>
      </div>
    </Dialog>
  )
}

function RulesEditor({
  mapping,
  aliases,
  onClose,
  onSave,
  onTest,
  saving,
  testing,
  error,
  run,
  channels,
}: {
  mapping: Mapping
  onClose: () => void
  /** Undefined while loading. */
  aliases: Alias[] | undefined
  /** `aliases` is the full list to save, or null when the user left them alone. */
  onSave: (patch: MappingPatch, aliases: string[] | null) => void
  onTest: (patch: MappingPatch) => void
  saving: boolean
  testing: boolean
  error: string | null
  run: DryRun | null
  channels?: Settings['channels']
}) {
  const [form, setForm] = useState<RulesForm>(() => formFrom(mapping))
  // A save turns handles and links into channel ids: show the ids once they come back.
  const savedChannels = mapping.channel_ids.join('\n')
  const [shownChannels, setShownChannels] = useState(savedChannels)
  if (savedChannels !== shownChannels) {
    setShownChannels(savedChannels)
    setForm((f) => ({ ...f, channelIds: savedChannels }))
  }
  const named = mapping.channel_ids.filter((id) => mapping.channel_titles[id])
  // Set by the podcast defaults: a podcast's zone is picked, never assumed to be Pakistan's.
  const [zoneRequired, setZoneRequired] = useState(false)
  const configuredZone = channelZone(channels, lines(form.channelIds))
  // Null until edited, so aliases that load after the drawer opens still show.
  const [aliasDraft, setAliasDraft] = useState<string | null>(null)
  const set = (key: keyof RulesForm) => (e: { target: { value: string } }) =>
    setForm((f) => ({ ...f, [key]: e.target.value }))
  const arrAliases = (aliases ?? []).filter((a) => a.source === 'arr').map((a) => a.alias)
  const userAliases = (aliases ?? []).filter((a) => a.source === 'user').map((a) => a.alias)
  const languageSource: LanguageSource = form.language.trim() !== mapping.language ? 'user' : mapping.language_source

  function applyPodcastDefaults() {
    setForm((f) => withPodcastDefaults(f, configuredZone))
    setZoneRequired(true)
  }

  function addAlias(alias: string) {
    const current = aliasDraft ?? userAliases.join('\n')
    if (!aliasLines(current).includes(alias)) setAliasDraft(current ? `${current}\n${alias}` : alias)
  }

  function submit(e: FormEvent) {
    e.preventDefault()
    // The endpoint takes the whole list, so the arr's names go back with the user's.
    const list = aliases && aliasDraft !== null ? [...arrAliases, ...aliasLines(aliasDraft)] : null
    onSave({ ...patchFrom(form, mapping.kind), root_folder: rootFolderFrom(form) }, list)
  }

  return (
    <form onSubmit={submit} className="flex h-full flex-col divide-y divide-line bg-surface shadow-xl">
      <div className="h-0 flex-1 overflow-y-auto">
        <div className="bg-brand px-4 py-6 sm:px-6">
          <div className="flex items-center justify-between">
            <DialogTitle className="text-base font-semibold text-on-brand">{mapping.title}</DialogTitle>
            <div className="ml-3 flex h-7 items-center">
              <button
                type="button"
                onClick={onClose}
                className="relative rounded-md text-on-brand/70 hover:text-on-brand focus-visible:ring-2 focus-visible:ring-on-brand focus-visible:outline-hidden"
              >
                <span className="absolute -inset-2.5" />
                <span className="sr-only">Close panel</span>
                <XMarkIcon aria-hidden="true" className="size-6" />
              </button>
            </div>
          </div>
          <p className="mt-1 text-sm text-on-brand/80">
            {kindLabel[mapping.kind]}. Title and ids come from {mapping.kind === 'film' ? 'Radarr' : 'Sonarr'}, the
            rules below are Tubarr's.
          </p>
        </div>
        <div className="divide-y divide-line px-4 sm:px-6">
          <div className="space-y-6 pt-6 pb-5">
            <Field id="channel-ids" label="Official channels" help="Channel id, @handle or link, one per line.">
              <textarea
                id="channel-ids"
                rows={2}
                value={form.channelIds}
                onChange={set('channelIds')}
                className={`${inputClass} font-mono`}
              />
              {named.length > 0 && (
                <ul aria-label="Channel names" className="mt-2 space-y-1 text-sm text-muted">
                  {named.map((id) => (
                    <li key={id}>
                      <span className="font-mono">{id}</span> {mapping.channel_titles[id]}
                    </li>
                  ))}
                </ul>
              )}
            </Field>
            {mapping.kind === 'show' && (
              <Field
                id="episode-regex"
                label="Episode regex"
                help="Needs a named group ep. Empty uses the default rule, which also checks the show title."
              >
                <input
                  id="episode-regex"
                  type="text"
                  value={form.episodeRegex}
                  onChange={set('episodeRegex')}
                  className={`${inputClass} font-mono`}
                />
              </Field>
            )}
            <Field id="excludes" label="Excludes" help={excludesHelp[mapping.kind === 'film' ? 'film' : 'show']}>
              <textarea id="excludes" rows={3} value={form.excludes} onChange={set('excludes')} className={inputClass} />
            </Field>
            <div className="grid grid-cols-1 gap-6 sm:grid-cols-2">
              <Field id="min-duration" label="Minimum length (minutes)" help={minDurationHelp[mapping.kind === 'film' ? 'film' : 'show']}>
                <input
                  id="min-duration"
                  type="number"
                  min={1}
                  value={form.minDurationMins}
                  onChange={set('minDurationMins')}
                  className={inputClass}
                />
              </Field>
              <Field
                id="language"
                label="Language"
                corner={<Badge tone={languageSource === 'user' ? 'cyan' : 'gray'}>{languageSourceLabel(languageSource, mapping.kind)}</Badge>}
              >
                <input id="language" type="text" value={form.language} onChange={set('language')} className={inputClass} />
              </Field>
            </div>
            {mapping.kind !== 'film' && (
              <Field
                id="backfill-limit"
                label="Back episodes to release"
                help="Of the episodes uploaded before the show was added, only this many of the newest. Empty releases them all."
              >
                <input
                  id="backfill-limit"
                  type="number"
                  min={0}
                  value={form.backfillLimit}
                  onChange={set('backfillLimit')}
                  className={inputClass}
                />
              </Field>
            )}
            {mapping.kind === 'show' && (
              <div className="flex items-center justify-between gap-x-4">
                <p className="text-sm text-muted">
                  A video podcast: match by #N, the 10 newest back episodes, at least 20 minutes, no clips, YouTube thumbnails as episode images.
                </p>
                <button type="button" onClick={applyPodcastDefaults} className={`shrink-0 ${secondaryButton}`}>
                  Use podcast defaults
                </button>
              </div>
            )}
            {usesOffset(form, mapping.kind) && (
              <>
                <Field
                  id="timezone"
                  label="Channel time zone"
                  help={
                    zoneRequired
                      ? zoneHelp(configuredZone)
                      : 'An IANA name, like America/Chicago. It follows daylight saving and wins over the offset.'
                  }
                >
                  <TimeZoneInput
                    id="timezone"
                    required={zoneRequired && form.utcOffsetMins.trim() === ''}
                    value={form.timezone}
                    onChange={(v) => setForm((f) => ({ ...f, timezone: v }))}
                  />
                </Field>
                <Field
                  id="utc-offset"
                  label="Fixed UTC offset (minutes)"
                  help="Only without a time zone. Both empty means Pakistan, 300."
                >
                  <input
                    id="utc-offset"
                    type="number"
                    min={-720}
                    max={840}
                    value={form.utcOffsetMins}
                    onChange={set('utcOffsetMins')}
                    className={inputClass}
                  />
                </Field>
              </>
            )}
            {mapping.kind === 'show' && (
              <Toggle
                id="title-number"
                label="Match by number in the title"
                help="For podcasts that number episodes like #2569: matched against TheTVDB episode titles"
                checked={form.titleNumber}
                onChange={(on) => setForm((f) => ({ ...f, titleNumber: on }))}
              />
            )}
            {mapping.kind !== 'film' && (
              <Toggle
                id="audio-version"
                label="Audio only version"
                help="Adds an Audio only choice next to each episode in Jellyfin"
                checked={form.audioVersion}
                onChange={(on) => setForm((f) => ({ ...f, audioVersion: on }))}
              />
            )}
            {mapping.kind !== 'film' && (
              <Toggle
                id="episode-thumbnails"
                label="Episode images from YouTube"
                help="Saves each episode's YouTube thumbnail as its image in Jellyfin, for shows TheTVDB has no stills for"
                checked={form.episodeThumbnails}
                onChange={(on) => setForm((f) => ({ ...f, episodeThumbnails: on }))}
              />
            )}
            {mapping.kind === 'show' && (
              <div className="grid grid-cols-1 gap-6 sm:grid-cols-2">
                <Field id="playlist-id" label="Playlist (PL...)">
                  <input id="playlist-id" type="text" value={form.playlistId} onChange={set('playlistId')} className={inputClass} />
                </Field>
                <Select
                  id="playlist-mode"
                  label="Use playlist for"
                  value={form.playlistMode}
                  onChange={(v) => setForm((f) => ({ ...f, playlistMode: v as PlaylistMode }))}
                  options={[
                    { value: 'source', label: 'finding videos (titles give the number)' },
                    { value: 'order', label: 'episode order (position gives the number)' },
                  ]}
                />
                {form.playlistMode === 'order' && (
                  <>
                    <Field id="playlist-season" label="Its season">
                      <input id="playlist-season" type="number" min={0} value={form.playlistSeason} onChange={set('playlistSeason')} className={inputClass} />
                    </Field>
                    <Field id="playlist-offset" label="First episode">
                      <input id="playlist-offset" type="number" min={0} value={form.playlistOffset} onChange={set('playlistOffset')} className={inputClass} />
                    </Field>
                  </>
                )}
              </div>
            )}
            <AliasesField
              arrName={arrName(mapping.kind)}
              arrAliases={arrAliases}
              loading={aliases === undefined}
              value={aliasDraft ?? userAliases.join('\n')}
              onChange={setAliasDraft}
            />
            {error && (
              <p role="alert" className="text-sm text-brand-text">
                {error}
              </p>
            )}
          </div>
          <LibraryFolder mapping={mapping} form={form} setForm={setForm} />
          {run && <DryRunResults run={run} onAddAlias={addAlias} />}
        </div>
      </div>
      <div className="flex shrink-0 justify-end gap-x-4 px-4 py-4">
        <button
          type="button"
          disabled={testing}
          onClick={() => onTest(patchFrom(form, mapping.kind))}
          className="rounded-md bg-ink/10 px-3 py-2 text-sm font-semibold text-ink ring-1 ring-line ring-inset hover:bg-ink/20 disabled:opacity-50"
        >
          {testing ? 'Testing...' : 'Test against channel'}
        </button>
        <button
          type="submit"
          disabled={saving}
          className="inline-flex justify-center rounded-md bg-brand px-3 py-2 text-sm font-semibold text-on-brand shadow-xs hover:bg-brand-dark focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand disabled:opacity-50"
        >
          {saving ? 'Saving...' : 'Save'}
        </button>
      </div>
    </form>
  )
}

function AliasesField({
  arrName,
  arrAliases,
  loading,
  value,
  onChange,
}: {
  arrName: string
  arrAliases: string[]
  loading: boolean
  value: string
  onChange: (value: string) => void
}) {
  return (
    <Field
      id="aliases"
      label="Aliases"
      help={`Other names it goes by on YouTube, one per line. Names from ${arrName} come back on every sync.`}
    >
      {arrAliases.length > 0 && (
        <ul role="list" aria-label={`Aliases from ${arrName}`} className="mb-2 flex flex-wrap gap-2">
          {arrAliases.map((a) => (
            <li key={a}>
              <Badge tone="gray">
                {a}
                <span className="ml-1.5 text-muted/70">from {arrName}</span>
              </Badge>
            </li>
          ))}
        </ul>
      )}
      <textarea
        id="aliases"
        rows={2}
        disabled={loading}
        placeholder={loading ? 'Loading...' : undefined}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className={`${inputClass} disabled:opacity-50`}
      />
    </Field>
  )
}

const resultTone = (result: string): Tone => (result === 'moved' ? 'green' : result === 'missing' ? 'yellow' : 'red')

// Layout from Tailwind Plus application-ui/data-display/description-lists (narrow, left aligned).
function LibraryFolder({
  mapping,
  form,
  setForm,
}: {
  mapping: Mapping
  form: RulesForm
  setForm: (update: (f: RulesForm) => RulesForm) => void
}) {
  const arr = arrName(mapping.kind)
  const notThere =
    mapping.root_folder_result === 'missing' &&
    mapping.wanted_root_folder !== null &&
    mapping.wanted_root_folder !== mapping.current_root_folder
  return (
    <div className="space-y-4 pt-6 pb-6">
      <div>
        <h3 className="text-sm/6 font-medium text-ink">Library folder</h3>
        <p className="mt-1 text-sm text-muted">The {arr} root folder this title lives in.</p>
      </div>
      <dl className="divide-y divide-line text-sm">
        <div className="flex justify-between gap-x-4 py-2">
          <dt className="text-muted">Current</dt>
          <dd className="font-mono text-ink">{mapping.current_root_folder ?? 'unknown'}</dd>
        </div>
        <div className="flex justify-between gap-x-4 py-2">
          <dt className="text-muted">Wanted</dt>
          <dd className="font-mono text-ink">{mapping.wanted_root_folder ?? 'no routing'}</dd>
        </div>
        <div className="flex justify-between gap-x-4 py-2">
          <dt className="text-muted">Last move</dt>
          <dd className="flex items-center gap-x-2 text-ink">
            {mapping.root_folder_result ? (
              <>
                <Badge tone={resultTone(mapping.root_folder_result)}>{mapping.root_folder_result}</Badge>
                <span className="text-muted">{when(mapping.root_folder_result_at)}</span>
              </>
            ) : (
              'none yet'
            )}
          </dd>
        </div>
      </dl>
      {notThere && (
        <Alert tone="info" title={`${arr} has no root folder ${mapping.wanted_root_folder}`}>
          Add this root folder in {arr}, then save again or wait for the next sync.
        </Alert>
      )}
      <Select
        id="root-mode"
        label="Move to"
        value={form.rootMode}
        onChange={(v) => setForm((f) => ({ ...f, rootMode: v as RootMode }))}
        options={[
          { value: 'follow', label: 'Follow channel/language' },
          { value: 'none', label: "Don't move" },
          { value: 'custom', label: 'Custom folder' },
        ]}
      />
      {form.rootMode === 'custom' && (
        <Field id="root-path" label="Custom folder" help={`An absolute path ${arr} lists as a root folder.`}>
          <input
            id="root-path"
            type="text"
            placeholder="/tv/urdu"
            value={form.rootPath}
            onChange={(e) => setForm((f) => ({ ...f, rootPath: e.target.value }))}
            className={`${inputClass} font-mono`}
          />
        </Field>
      )}
    </div>
  )
}
