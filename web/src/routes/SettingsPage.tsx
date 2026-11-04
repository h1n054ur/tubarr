import { useState, type FormEvent } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  api,
  type ArrApp,
  type ArrConnection,
  type Kind,
  type NewManualMapping,
  type Preset,
  type Settings,
  type WiringItem,
} from '../lib/api'
import { Badge, type Tone } from '../components/Badge'
import { DescriptionList } from '../components/DescriptionList'
import { MappingTransfer } from '../components/MappingTransfer'
import { UsersSection } from '../components/UsersSection'
import { Field, Select, TimeZoneInput, inputClass, primaryButton, secondaryButton } from '../components/fields'
import { configSections } from '../lib/config'
import { channelZone, zoneHelp, zoneLabel } from '../lib/podcast'

const arrName = { sonarr: 'Sonarr', radarr: 'Radarr' } as const

function ArrTest({ app, configured }: { app: ArrApp; configured: boolean }) {
  const test = useMutation({ mutationFn: () => api.testArr(app) })
  const name = arrName[app]
  const result = test.isError
    ? test.error.message
    : test.data
      ? test.data.ok
        ? `${name} answered: tag "${test.data.tag}" has id ${test.data.tag_id}.`
        : test.data.error
      : null
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
      <button
        type="button"
        onClick={() => test.mutate()}
        disabled={!configured || test.isPending}
        className={secondaryButton}
      >
        {test.isPending ? 'Testing...' : `Test ${name}`}
      </button>
      <span role="status" className="text-sm text-muted">
        {configured ? result : `${name} is not connected.`}
      </span>
    </div>
  )
}

const wiringTone: Record<WiringItem['state'], Tone> = { ok: 'green', missing: 'red', optional: 'gray' }
const wiringWord: Record<WiringItem['state'], string> = { ok: 'In place', missing: 'Missing', optional: 'Optional' }

/** Read only: lists what Tubarr needs in the arr, each in place or with what to add. */
function ArrWiring({ app, configured }: { app: ArrApp; configured: boolean }) {
  const check = useMutation({ mutationFn: () => api.arrWiring(app) })
  const name = arrName[app]
  return (
    <div>
      <button
        type="button"
        onClick={() => check.mutate()}
        disabled={!configured || check.isPending}
        className={secondaryButton}
      >
        {check.isPending ? 'Checking...' : `Check ${name} setup`}
      </button>
      {check.isError && (
        <p role="alert" className="mt-2 text-sm text-brand-text">
          {check.error.message}
        </p>
      )}
      {check.data && !check.data.ok && (
        <p role="alert" className="mt-2 text-sm text-brand-text">
          {check.data.error}
        </p>
      )}
      {check.data?.ok && (
        <ul aria-label={`${name} setup`} className="mt-3 divide-y divide-line rounded-md border border-line">
          {check.data.items.map((item) => (
            <li key={item.key} className="flex flex-col gap-1 px-3 py-2 sm:flex-row sm:items-start sm:gap-3">
              <span className="shrink-0">
                <Badge tone={wiringTone[item.state]}>{wiringWord[item.state]}</Badge>
              </span>
              <span className="text-sm">
                <span className="font-medium text-ink">{item.label}</span>
                <span className="block text-muted">{item.detail}</span>
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

/** One arr: read only when the config file sets it, else a form that tests before it saves. */
function ArrConnectionCard({ app, conn }: { app: ArrApp; conn: ArrConnection }) {
  const queryClient = useQueryClient()
  const name = arrName[app]
  const [url, setUrl] = useState(conn.url ?? '')
  const [key, setKey] = useState('')
  const done = () => queryClient.invalidateQueries({ queryKey: ['arr-connections'] })
  const save = useMutation({
    mutationFn: () => api.saveArr(app, key.trim() === '' ? { url } : { url, api_key: key }),
    onSuccess: () => {
      setKey('')
      remove.reset()
      done()
    },
  })
  const remove = useMutation({
    mutationFn: () => api.removeArr(app),
    onSuccess: () => {
      setUrl('')
      save.reset()
      done()
    },
  })
  const fromFile = conn.source === 'config'
  const saved = conn.source === 'saved'

  function submit(e: FormEvent) {
    e.preventDefault()
    save.mutate()
  }

  return (
    <div className="px-4 py-6 sm:px-6">
      <h3 className="text-sm/6 font-semibold text-ink">{name}</h3>
      {fromFile ? (
        <p className="mt-1 text-sm text-muted">
          {conn.url}. Set in the config file, so it is read only here. Change it there and restart.
        </p>
      ) : (
        <form onSubmit={submit} className="mt-3 grid max-w-2xl grid-cols-1 gap-4 sm:grid-cols-2">
          <Field id={`${app}-url`} label={`${name} URL`}>
            <input
              id={`${app}-url`}
              type="url"
              required
              placeholder={app === 'sonarr' ? 'http://sonarr:8989' : 'http://radarr:7878'}
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              className={inputClass}
            />
          </Field>
          <Field id={`${app}-key`} label={`${name} API key`} help={`In ${name} under Settings > General.`}>
            <input
              id={`${app}-key`}
              type="password"
              autoComplete="off"
              required={!saved}
              placeholder={saved ? 'Saved. Leave empty to keep it.' : undefined}
              value={key}
              onChange={(e) => setKey(e.target.value)}
              className={inputClass}
            />
          </Field>
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2 sm:col-span-2">
            <button type="submit" disabled={save.isPending} className={primaryButton}>
              {save.isPending ? 'Testing...' : 'Test and save'}
            </button>
            {saved && (
              <button
                type="button"
                onClick={() => remove.mutate()}
                disabled={remove.isPending}
                className={secondaryButton}
              >
                Remove
              </button>
            )}
            {save.isError && (
              <span role="alert" className="text-sm text-brand-text">
                {save.error.message}
              </span>
            )}
            {remove.isError && (
              <span role="alert" className="text-sm text-brand-text">
                {remove.error.message}
              </span>
            )}
            {save.isSuccess && (
              <span role="status" className="text-sm text-muted">
                Connected. {name} {save.data.version} answered. Syncing will start in a moment.
              </span>
            )}
            {remove.isSuccess && (
              <span role="status" className="text-sm text-muted">
                Removed. Tubarr no longer uses this {name}.
              </span>
            )}
          </div>
        </form>
      )}
      <div className="mt-4 flex flex-col gap-4">
        <ArrTest app={app} configured={conn.source !== null} />
        <ArrWiring app={app} configured={conn.source !== null} />
      </div>
    </div>
  )
}

function ArrConnections({ tag }: { tag: string }) {
  const conns = useQuery({ queryKey: ['arr-connections'], queryFn: api.arrConnections })
  return (
    <section className="mt-10 overflow-hidden rounded-lg border border-line bg-surface shadow-sm">
      <div className="bg-surface-2 px-4 py-5 sm:px-6">
        <h2 className="text-base/7 font-semibold text-ink">Connections</h2>
        <p className="mt-1 max-w-2xl text-sm/6 text-muted">
          Each test looks up the {tag} tag, which proves the address, the key and the tag. The setup check reads
          what Tubarr needs in the arr and changes nothing.
        </p>
      </div>
      {conns.isError && (
        <p role="alert" className="border-t border-line px-4 py-6 text-sm text-brand-text sm:px-6">
          {conns.error.message}
        </p>
      )}
      {conns.data && (
        <div className="divide-y divide-line border-t border-line">
          {(['sonarr', 'radarr'] as const).map((app) => (
            <ArrConnectionCard key={app} app={app} conn={conns.data[app]} />
          ))}
        </div>
      )}
    </section>
  )
}

const emptyMapping = { kind: 'show', preset: 'drama', title: '', id: '', language: '', channels: '', timezone: '' }

function channelList(text: string) {
  return text
    .split(/[\n,]/)
    .map((s) => s.trim())
    .filter(Boolean)
}

function AddMapping({ channels }: { channels: Settings['channels'] }) {
  const queryClient = useQueryClient()
  const [form, setForm] = useState(emptyMapping)
  const create = useMutation({
    mutationFn: (m: NewManualMapping) => api.createMapping(m),
    onSuccess: () => {
      setForm(emptyMapping)
      queryClient.invalidateQueries({ queryKey: ['mappings'] })
    },
  })
  const film = form.kind === 'film'
  const podcast = form.kind === 'show' && form.preset === 'podcast'
  const configuredZone = channelZone(channels, channelList(form.channels))

  function submit(e: FormEvent) {
    e.preventDefault()
    const m: NewManualMapping = {
      kind: form.kind as Kind,
      title: form.title.trim(),
      language: form.language.trim(),
      channel_ids: channelList(form.channels),
    }
    if (film) m.tmdb_id = Number(form.id)
    else m.tvdb_id = Number(form.id)
    if (podcast) {
      m.preset = 'podcast'
      if (form.timezone.trim() !== '') m.timezone = form.timezone.trim()
    }
    create.mutate(m)
  }

  const set = (k: keyof typeof emptyMapping) => (v: string) => setForm({ ...form, [k]: v })

  return (
    // Ported from Tailwind Plus application-ui/forms/form-layouts/two-column-with-cards, framed like
    // DescriptionList and Table: heading in the surface-2 header, the Add button in a footer row.
    <form onSubmit={submit} className="mt-10 overflow-hidden rounded-lg border border-line bg-surface shadow-sm">
      <div className="bg-surface-2 px-4 py-5 sm:px-6">
        <h2 className="text-base/7 font-semibold text-ink">Add mapping by hand</h2>
        <p className="mt-1 max-w-2xl text-sm/6 text-muted">
          For a title Sonarr or Radarr does not tag. Edit its rules afterwards on Shows and films.
        </p>
      </div>
      <div className="grid max-w-2xl grid-cols-1 gap-6 border-t border-line px-4 py-6 sm:grid-cols-2 sm:px-6">
        <Select
          id="new-kind"
          label="Kind"
          value={form.kind}
          onChange={set('kind')}
          options={[
            { value: 'show', label: 'Show' },
            { value: 'daily', label: 'Daily show' },
            { value: 'film', label: 'Film' },
          ]}
        />
        {form.kind === 'show' && (
          <Select
            id="new-preset"
            label="Starting rules"
            value={form.preset}
            onChange={(v) => set('preset')(v as Preset)}
            options={[
              { value: 'drama', label: 'Drama (Episode N in the title)' },
              { value: 'podcast', label: 'Podcast (#N in the title)' },
            ]}
          />
        )}
        <Field id="new-title" label="Title">
          <input
            id="new-title"
            required
            value={form.title}
            onChange={(e) => set('title')(e.target.value)}
            className={inputClass}
          />
        </Field>
        <Field id="new-id" label={film ? 'TMDB id' : 'TVDB id'}>
          <input
            id="new-id"
            type="number"
            min={1}
            required
            value={form.id}
            onChange={(e) => set('id')(e.target.value)}
            className={inputClass}
          />
        </Field>
        <Field id="new-language" label="Language">
          <input
            id="new-language"
            required
            value={form.language}
            onChange={(e) => set('language')(e.target.value)}
            className={inputClass}
          />
        </Field>
        <div className="sm:col-span-2">
          <Field
            id="new-channels"
            label="Channels"
            help="Channel id, @handle or link, one per line. Films may leave this empty."
          >
            <textarea
              id="new-channels"
              rows={2}
              value={form.channels}
              onChange={(e) => set('channels')(e.target.value)}
              className={inputClass}
            />
          </Field>
        </div>
        {podcast && (
          <div className="sm:col-span-2">
            <p className="mb-4 text-sm text-muted">
              Matches the #N in each title against TheTVDB episode titles, releases the 10 newest back episodes, skips
              uploads under 20 minutes and titles with Clip or Best of.
            </p>
            <Field id="new-timezone" label="Channel time zone" help={zoneHelp(configuredZone)}>
              <TimeZoneInput
                id="new-timezone"
                required={configuredZone === null}
                placeholder={configuredZone === null ? undefined : zoneLabel(configuredZone)}
                value={form.timezone}
                onChange={set('timezone')}
              />
            </Field>
          </div>
        )}
      </div>
      <div className="flex items-center justify-end gap-x-4 border-t border-line px-4 py-4 sm:px-6">
        {create.isError && (
          <span role="alert" className="text-sm text-brand-text">
            {create.error.message}
          </span>
        )}
        {create.isSuccess && (
          <span role="status" className="text-sm text-muted">
            Added {create.data.title}.
          </span>
        )}
        <button type="submit" disabled={create.isPending} className={primaryButton}>
          {create.isPending ? 'Adding...' : 'Add mapping'}
        </button>
      </div>
    </form>
  )
}

export function SettingsPage() {
  const settings = useQuery({ queryKey: ['settings'], queryFn: api.settings })

  return (
    <div>
      <h1 className="text-base font-semibold text-ink">Settings</h1>
      <p className="mt-2 text-sm text-muted">
        The config Tubarr runs with. Sonarr and Radarr can be connected here, everything else is changed in the config file and takes a restart. Secrets show only whether they are set.
      </p>

      {settings.isPending && <p className="mt-8 text-sm text-muted">Loading...</p>}
      {settings.isError && (
        <p role="alert" className="mt-8 text-sm text-brand-text">
          {settings.error.message}
        </p>
      )}
      {settings.data && (
        <>
          <ArrConnections tag={settings.data.arr.tag} />
          <section className="mt-10">
            <h2 className="text-base/7 font-semibold text-ink">Effective config</h2>
            {configSections(settings.data).map((section) => (
              <DescriptionList key={section.title} title={section.title} items={section.items} level="h3" />
            ))}
          </section>
        </>
      )}

      <UsersSection auth={settings.data?.auth} />

      <AddMapping channels={settings.data?.channels} />

      <MappingTransfer />
    </div>
  )
}
