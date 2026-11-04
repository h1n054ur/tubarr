// The pin form, ported from Tailwind Plus application-ui/overlays/modal-dialogs/simple-with-gray-footer.
import { useState, type FormEvent } from 'react'
import { Dialog, DialogBackdrop, DialogPanel, DialogTitle } from '@headlessui/react'
import type { NewPin, UnmatchedNote } from '../lib/api'
import { date } from '../lib/date'
import { Field, inputClass, primaryButton, secondaryButton } from './fields'

function initialDate(note: UnmatchedNote): string {
  return note.published_at === null ? '' : date(note.published_at)
}

/** What to send for a note, from the form. Shows take season and episode, daily shows a date. */
export function pinFrom(note: UnmatchedNote, season: string, episode: string, airDate: string): NewPin {
  const pin: NewPin = { mapping_id: note.mapping_id, video_id: note.video_id }
  if (note.mapping_kind === 'show') {
    pin.season = Number(season)
    pin.episode = Number(episode)
  }
  if (note.mapping_kind === 'daily') pin.air_date = airDate.trim()
  return pin
}

function PinForm({
  note,
  onClose,
  onPin,
  saving,
  error,
}: {
  note: UnmatchedNote
  onClose: () => void
  onPin: (pin: NewPin) => void
  saving: boolean
  error: string | null
}) {
  const [season, setSeason] = useState(note.season === null ? '' : String(note.season))
  const [episode, setEpisode] = useState(note.episode === null ? '' : String(note.episode))
  const [airDate, setAirDate] = useState(initialDate(note))

  function submit(e: FormEvent) {
    e.preventDefault()
    onPin(pinFrom(note, season, episode, airDate))
  }

  const target =
    note.mapping_kind === 'film' ? 'the film' : note.mapping_kind === 'daily' ? 'one air date' : 'one episode'

  return (
    <form onSubmit={submit}>
      <div className="px-4 pt-5 pb-4 sm:p-6 sm:pb-4">
        <DialogTitle as="h3" className="text-base font-semibold text-ink">
          Pin to {target}
        </DialogTitle>
        <p className="mt-2 text-sm text-muted">
          {note.video_title ?? note.video_id} becomes the video for {note.mapping_title}. A pin wins over the matcher,
          which runs again in a moment.
        </p>
        <div className="mt-4 space-y-4">
          {note.mapping_kind === 'show' && (
            <div className="grid grid-cols-2 gap-4">
              <Field id="pin-season" label="Season">
                <input
                  id="pin-season"
                  type="number"
                  min={0}
                  required
                  value={season}
                  onChange={(e) => setSeason(e.target.value)}
                  className={inputClass}
                />
              </Field>
              <Field id="pin-episode" label="Episode">
                <input
                  id="pin-episode"
                  type="number"
                  min={1}
                  required
                  value={episode}
                  onChange={(e) => setEpisode(e.target.value)}
                  className={inputClass}
                />
              </Field>
            </div>
          )}
          {note.mapping_kind === 'daily' && (
            <Field id="pin-date" label="Air date" help="YYYY-MM-DD, as Sonarr lists the episode.">
              <input
                id="pin-date"
                type="text"
                required
                pattern="\d{4}-\d{2}-\d{2}"
                value={airDate}
                onChange={(e) => setAirDate(e.target.value)}
                className={inputClass}
              />
            </Field>
          )}
          {error && (
            <p role="alert" className="text-sm text-brand-text">
              {error}
            </p>
          )}
        </div>
      </div>
      <div className="gap-x-3 bg-surface-2 px-4 py-3 sm:flex sm:flex-row-reverse sm:px-6">
        <button type="submit" disabled={saving} className={primaryButton}>
          {saving ? 'Pinning...' : 'Pin'}
        </button>
        <button type="button" data-autofocus onClick={onClose} className={`mt-3 sm:mt-0 ${secondaryButton}`}>
          Cancel
        </button>
      </div>
    </form>
  )
}

export function PinDialog(props: {
  note: UnmatchedNote | null
  onClose: () => void
  onPin: (pin: NewPin) => void
  saving: boolean
  error: string | null
}) {
  const { note, onClose } = props
  return (
    <Dialog open={note !== null} onClose={onClose} className="relative z-50">
      <DialogBackdrop transition className="fixed inset-0 bg-canvas/75 transition-opacity data-closed:opacity-0" />
      <div className="fixed inset-0 z-10 w-screen overflow-y-auto">
        <div className="flex min-h-full items-end justify-center p-4 text-center sm:items-center sm:p-0">
          <DialogPanel
            transition
            className="relative transform overflow-hidden rounded-lg bg-surface text-left shadow-xl ring-1 ring-line transition-all data-closed:translate-y-4 data-closed:opacity-0 sm:my-8 sm:w-full sm:max-w-lg"
          >
            {note && <PinForm key={note.id} {...props} note={note} />}
          </DialogPanel>
        </div>
      </div>
    </Dialog>
  )
}
