// The check and new-draft forms, ported from Tailwind Plus application-ui/overlays/modal-dialogs/
// simple-with-gray-footer like the pin form. A check only searches; a new draft is stored.
import { useState, type FormEvent } from 'react'
import { Dialog, DialogBackdrop, DialogPanel, DialogTitle } from '@headlessui/react'
import type {
  CheckRequest,
  DraftCheck,
  DraftKind,
  FilmDraftRequest,
  Preset,
  ShowDraftRequest,
  DraftVerdict,
} from '../lib/api'
import { Badge, type Tone } from './Badge'
import { Field, Select, TimeZoneInput, inputClass, primaryButton, secondaryButton } from './fields'

export type DraftMode = 'check' | 'new'

export const verdictLabel: Record<DraftVerdict, string> = {
  found: 'already there',
  possible: 'maybe there',
  probably_missing: 'probably missing',
  not_checked: 'not checked',
  unknown: 'search failed',
}

export const verdictTone: Record<DraftVerdict, Tone> = {
  found: 'green',
  possible: 'yellow',
  probably_missing: 'cyan',
  not_checked: 'gray',
  unknown: 'red',
}

/** The show request from the form: blank optional fields stay out. */
export function showRequest(f: {
  channel: string
  title: string
  playlist: string
  season: string
  preset: Preset
  timezone: string
}): ShowDraftRequest {
  const req: ShowDraftRequest = { channel_id: f.channel.trim(), title: f.title.trim() }
  if (f.playlist.trim()) req.playlist_id = f.playlist.trim()
  if (f.season.trim()) req.season = Number(f.season)
  if (f.preset !== 'drama') req.preset = f.preset
  if (f.timezone.trim()) req.timezone = f.timezone.trim()
  return req
}

function CheckAnswer({ check }: { check: DraftCheck }) {
  return (
    <div className="rounded-md bg-surface-2 p-3 text-sm">
      <div className="flex items-center gap-x-2">
        <Badge tone={verdictTone[check.verdict]}>{verdictLabel[check.verdict]}</Badge>
      </div>
      <p className="mt-2 text-ink wrap-anywhere">{check.summary}</p>
      {check.sources.some((s) => s.state !== 'checked') && (
        <ul className="mt-2 space-y-1 text-muted">
          {check.sources
            .filter((s) => s.state !== 'checked')
            .map((s) => (
              <li key={s.source}>{s.reason}</li>
            ))}
        </ul>
      )}
    </div>
  )
}

function DraftForm({
  mode,
  onClose,
  onCheck,
  onShow,
  onFilm,
  busy,
  error,
  check,
}: {
  mode: DraftMode
  onClose: () => void
  onCheck: (req: CheckRequest) => void
  onShow: (req: ShowDraftRequest) => void
  onFilm: (req: FilmDraftRequest) => void
  busy: boolean
  error: string | null
  check: DraftCheck | null
}) {
  const [kind, setKind] = useState<DraftKind>('show')
  const [title, setTitle] = useState('')
  const [original, setOriginal] = useState('')
  const [year, setYear] = useState('')
  const [channel, setChannel] = useState('')
  const [playlist, setPlaylist] = useState('')
  const [season, setSeason] = useState('')
  const [preset, setPreset] = useState<Preset>('drama')
  const [timezone, setTimezone] = useState('')
  const [video, setVideo] = useState('')

  function submit(e: FormEvent) {
    e.preventDefault()
    if (mode === 'check') {
      const req: CheckRequest = { kind, title: title.trim() }
      if (original.trim()) req.original_title = original.trim()
      if (year.trim()) req.year = Number(year)
      onCheck(req)
    } else if (kind === 'show') {
      onShow(showRequest({ channel, title, playlist, season, preset, timezone }))
    } else {
      onFilm({ video_id: video.trim() })
    }
  }

  const site = kind === 'show' ? 'TheTVDB' : 'TMDB'
  return (
    <form onSubmit={submit}>
      <div className="px-4 pt-5 pb-4 sm:p-6 sm:pb-4">
        <DialogTitle as="h3" className="text-base font-semibold text-ink">
          {mode === 'check' ? 'Check a title' : 'New draft'}
        </DialogTitle>
        <p className="mt-2 text-sm text-muted">
          {mode === 'check'
            ? `Searches Sonarr or Radarr and ${site} for the title. Nothing is stored.`
            : `Drafts the ${site} entry from the uploads Tubarr already knows. It costs no YouTube lookups.`}
        </p>
        <div className="mt-4 space-y-4">
          <Select
            id="draft-kind"
            label="Kind"
            value={kind}
            onChange={(v) => setKind(v as DraftKind)}
            options={[
              { value: 'show', label: 'Show (TheTVDB)' },
              { value: 'film', label: 'Film (TMDB)' },
            ]}
          />
          {mode === 'check' && (
            <>
              <Field id="draft-title" label="Title">
                <input id="draft-title" required value={title} onChange={(e) => setTitle(e.target.value)} className={inputClass} />
              </Field>
              <div className="grid grid-cols-2 gap-4">
                <Field id="draft-original" label="Original title">
                  <input
                    id="draft-original"
                    value={original}
                    onChange={(e) => setOriginal(e.target.value)}
                    className={inputClass}
                  />
                </Field>
                <Field id="draft-year" label="Year">
                  <input
                    id="draft-year"
                    type="number"
                    min={1900}
                    max={2100}
                    value={year}
                    onChange={(e) => setYear(e.target.value)}
                    className={inputClass}
                  />
                </Field>
              </div>
            </>
          )}
          {mode === 'new' && kind === 'show' && (
            <>
              <Field id="draft-channel" label="Channel id" help="The official channel, like UC4JCksJF76g_MdzPVBJoC3Q.">
                <input
                  id="draft-channel"
                  required
                  spellCheck={false}
                  value={channel}
                  onChange={(e) => setChannel(e.target.value)}
                  className={inputClass}
                />
              </Field>
              <Field id="draft-title" label="Series name" help="As the channel writes it in its upload titles.">
                <input id="draft-title" required value={title} onChange={(e) => setTitle(e.target.value)} className={inputClass} />
              </Field>
              <div className="grid grid-cols-2 gap-4">
                <Field id="draft-playlist" label="Playlist id">
                  <input
                    id="draft-playlist"
                    spellCheck={false}
                    value={playlist}
                    onChange={(e) => setPlaylist(e.target.value)}
                    className={inputClass}
                  />
                </Field>
                <Field id="draft-season" label="Season">
                  <input
                    id="draft-season"
                    type="number"
                    min={0}
                    placeholder="1"
                    value={season}
                    onChange={(e) => setSeason(e.target.value)}
                    className={inputClass}
                  />
                </Field>
              </div>
              <Select
                id="draft-preset"
                label="Numbering"
                value={preset}
                onChange={(v) => setPreset(v as Preset)}
                options={[
                  { value: 'drama', label: 'Drama, by episode number in the title' },
                  { value: 'podcast', label: 'Podcast, by the running #N' },
                ]}
              />
              <Field id="draft-timezone" label="Time zone" help="Only for a channel with no [channels] entry.">
                <TimeZoneInput id="draft-timezone" value={timezone} onChange={setTimezone} />
              </Field>
            </>
          )}
          {mode === 'new' && kind === 'film' && (
            <Field id="draft-video" label="Video id" help="The film's upload on the official channel.">
              <input
                id="draft-video"
                required
                spellCheck={false}
                value={video}
                onChange={(e) => setVideo(e.target.value)}
                className={inputClass}
              />
            </Field>
          )}
          {check && <CheckAnswer check={check} />}
          {error && (
            <p role="alert" className="text-sm text-brand-text">
              {error}
            </p>
          )}
        </div>
      </div>
      <div className="gap-x-3 bg-surface-2 px-4 py-3 sm:flex sm:flex-row-reverse sm:px-6">
        <button type="submit" disabled={busy} className={primaryButton}>
          {mode === 'check' ? (busy ? 'Checking...' : 'Check') : busy ? 'Drafting...' : 'Draft'}
        </button>
        <button type="button" data-autofocus onClick={onClose} className={`mt-3 sm:mt-0 ${secondaryButton}`}>
          {check ? 'Close' : 'Cancel'}
        </button>
      </div>
    </form>
  )
}

export function DraftDialog(props: {
  mode: DraftMode | null
  onClose: () => void
  onCheck: (req: CheckRequest) => void
  onShow: (req: ShowDraftRequest) => void
  onFilm: (req: FilmDraftRequest) => void
  busy: boolean
  error: string | null
  check: DraftCheck | null
}) {
  const { mode, onClose } = props
  return (
    <Dialog open={mode !== null} onClose={onClose} className="relative z-50">
      <DialogBackdrop transition className="fixed inset-0 bg-canvas/75 transition-opacity data-closed:opacity-0" />
      <div className="fixed inset-0 z-10 w-screen overflow-y-auto">
        <div className="flex min-h-full items-end justify-center p-4 text-center sm:items-center sm:p-0">
          <DialogPanel
            transition
            className="relative transform overflow-hidden rounded-lg bg-surface text-left shadow-xl ring-1 ring-line transition-all data-closed:translate-y-4 data-closed:opacity-0 sm:my-8 sm:w-full sm:max-w-lg"
          >
            {mode && <DraftForm key={mode} {...props} mode={mode} />}
          </DialogPanel>
        </div>
      </div>
    </Dialog>
  )
}
