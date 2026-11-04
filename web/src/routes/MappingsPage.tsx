import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, type DryRun, type Mapping, type MappingFilter, type MappingPatch } from '../lib/api'
import { EmptyState } from '../components/EmptyState'
import { Select } from '../components/fields'
import { MappingDrawer } from '../components/MappingDrawer'
import { MappingsTable } from '../components/MappingsTable'
import { Pager } from '../components/Pager'
import { SearchBox } from '../components/SearchBox'
import { fuzzySearch, type FuzzyKey } from '../lib/fuzzy'
import { oneOf, useFilters, useListParams } from '../lib/urlState'

// Aliases are not in the list payload (they load per mapping in the drawer), so search leaves them out.
const searchKeys: FuzzyKey<Mapping>[] = [
  { name: 'title', get: (m) => m.title, weight: 3 },
  { name: 'tvdb', get: (m) => m.tvdb_id, exact: true },
  { name: 'tmdb', get: (m) => m.tmdb_id, exact: true },
  { name: 'channels', get: (m) => m.channel_ids },
  { name: 'language', get: (m) => m.language, weight: 0.5 },
]

// Kind and status live in the URL (`?kind=podcast&status=active`) next to the search and page.
const filters = {
  kind: oneOf(['show', 'podcast', 'daily', 'film']),
  status: oneOf(['active', 'inactive']),
}

export function MappingsPage() {
  const queryClient = useQueryClient()
  const [editing, setEditing] = useState<Mapping | null>(null)
  const [run, setRun] = useState<DryRun | null>(null)
  const list = useListParams()
  const { kind, status, set } = useFilters(filters)

  const filter: MappingFilter = {
    kind: kind || undefined,
    active: status === '' ? undefined : status === 'active',
  }
  const mappings = useQuery({ queryKey: ['mappings', filter], queryFn: () => api.mappings(filter) })
  const settings = useQuery({ queryKey: ['settings'], queryFn: api.settings })
  const tag = settings.data?.arr?.tag

  // Ranked best first while searching, else the server's order; then the page the pager shows.
  const found = mappings.data ? fuzzySearch(mappings.data, list.q, searchKeys) : []
  const pages = Math.max(1, Math.ceil(found.length / list.size))
  const at = Math.min(list.page, pages)
  const shown = found.slice((at - 1) * list.size, at * list.size)

  const aliases = useQuery({
    queryKey: ['aliases', editing?.id],
    queryFn: () => api.aliases((editing as Mapping).id),
    enabled: editing !== null,
  })

  const save = useMutation({
    mutationFn: async ({ id, patch, aliases }: { id: number; patch: MappingPatch; aliases: string[] | null }) => {
      const saved = await api.patchMapping(id, patch)
      if (aliases !== null) queryClient.setQueryData(['aliases', id], await api.setAliases(id, aliases))
      return saved
    },
    onSuccess: (saved) => {
      setEditing(saved)
      queryClient.invalidateQueries({ queryKey: ['mappings'] })
    },
  })
  const test = useMutation({
    mutationFn: ({ id, patch }: { id: number; patch: MappingPatch }) => api.testMapping(id, patch),
    onSuccess: setRun,
  })

  function open(m: Mapping | null) {
    setEditing(m)
    setRun(null)
    save.reset()
    test.reset()
  }

  const error = (save.error ?? test.error ?? aliases.error)?.message ?? null

  return (
    <div>
      <div className="sm:flex sm:items-end">
        <div className="sm:flex-auto">
          <h1 className="text-base font-semibold text-ink">Shows and films</h1>
          <p className="mt-2 text-sm text-muted">
            Every series and film Tubarr serves, with the rules that pick the right video. Sonarr and Radarr add them
            {tag ? (
              <>
                {' '}
                through the <span className="font-medium text-ink">{tag}</span> tag.
              </>
            ) : (
              ' through the tag set in the config.'
            )}
          </p>
        </div>
        <div className="mt-4 flex flex-wrap items-end gap-4 sm:mt-0 sm:ml-16 sm:flex-nowrap">
          <div className="w-full sm:w-64">
            <SearchBox
              id="search-mappings"
              label="Search"
              value={list.q}
              onChange={list.setQ}
              placeholder="Title, id, channel"
            />
          </div>
          <Select
            id="filter-kind"
            label="Kind"
            value={kind}
            onChange={(v) => set('kind', v as typeof kind)}
            options={[
              { value: '', label: 'All' },
              { value: 'show', label: 'Shows' },
              { value: 'podcast', label: 'Podcasts' },
              { value: 'daily', label: 'Daily shows' },
              { value: 'film', label: 'Films' },
            ]}
          />
          <Select
            id="filter-active"
            label="Status"
            value={status}
            onChange={(v) => set('status', v as typeof status)}
            options={[
              { value: '', label: 'All' },
              { value: 'active', label: 'Active' },
              { value: 'inactive', label: 'Inactive' },
            ]}
          />
        </div>
      </div>

      {mappings.isPending && <p className="mt-8 text-sm text-muted">Loading...</p>}
      {mappings.isError && (
        <p role="alert" className="mt-8 text-sm text-brand-text">
          {mappings.error.message}
        </p>
      )}
      {mappings.data && mappings.data.length === 0 && (
        <div className="mt-8">
          <EmptyState
            title="No mappings"
            text={`Tag a series in Sonarr or a film in Radarr with ${tag ? `the ${tag} tag` : 'the configured tag'} and it shows up here after the next sync.`}
          />
        </div>
      )}
      {mappings.data && mappings.data.length > 0 && found.length === 0 && (
        <div className="mt-8">
          <EmptyState
            title="No matches"
            text={`No shows match "${list.q}".`}
            action={
              <button
                type="button"
                onClick={() => list.setQ('')}
                className="rounded-md bg-surface px-3 py-2 text-sm font-semibold text-ink shadow-xs ring-1 ring-line ring-inset hover:bg-surface-2"
              >
                Show all
              </button>
            }
          />
        </div>
      )}
      {found.length > 0 && (
        <MappingsTable
          mappings={shown}
          onEdit={open}
          footer={
            <Pager page={at} size={list.size} total={found.length} onPage={list.setPage} onSize={list.setSize} />
          }
        />
      )}

      <MappingDrawer
        mapping={editing}
        aliases={aliases.data}
        onClose={() => open(null)}
        onSave={(patch, list) => editing && save.mutate({ id: editing.id, patch, aliases: list })}
        onTest={(patch) => editing && test.mutate({ id: editing.id, patch })}
        saving={save.isPending}
        testing={test.isPending}
        error={error}
        run={run}
        channels={settings.data?.channels}
      />
    </div>
  )
}
