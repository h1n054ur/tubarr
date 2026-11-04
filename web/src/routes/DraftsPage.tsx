import { useEffect, useState } from 'react'
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { DocumentTextIcon } from '@heroicons/react/24/outline'
import {
  api,
  type CheckRequest,
  type DraftCheck,
  type DraftList,
  type FilmDraftRequest,
  type ShowDraftRequest,
  type StoredDraft,
} from '../lib/api'
import { date } from '../lib/date'
import { Alert } from '../components/Alert'
import { Badge } from '../components/Badge'
import { DraftDetail } from '../components/DraftDetail'
import { DraftDialog, verdictLabel, verdictTone, type DraftMode } from '../components/DraftDialog'
import { EmptyState } from '../components/EmptyState'
import { Select, primaryButton, secondaryButton } from '../components/fields'
import { Pager } from '../components/Pager'
import { SearchBox } from '../components/SearchBox'
import { Stacked, Table, Td, Th } from '../components/Table'
import { oneOf, useFilters, useListParams, type FilterParam } from '../lib/urlState'

// Where the keys go, said in plain text: there is no public docs site to link to yet.
export const keysHint =
  "Set tvdb_api_key_file and tmdb_read_token_file under [drafts] in Tubarr's config file (see the configuration reference in the docs)."

// The kind filter and the open draft live in the URL (`?kind=film&draft=12`) next to the search and page.
const filters = {
  kind: oneOf(['show', 'film']),
  draft: { values: (v: string) => /^[1-9]\d*$/.test(v), default: '', resetsPage: false } as FilterParam,
}

/** Which keys are missing, as one sentence, or null when both sites can be asked. */
function missingKeys(keys: DraftList['keys']): string | null {
  const missing = [!keys.tvdb.ready && 'TheTVDB', !keys.tmdb.ready && 'TMDB'].filter(Boolean)
  if (missing.length === 0) return null
  return `No ${missing.join(' or ')} key is set, so drafts are not checked against ${missing.length > 1 ? 'those sites' : 'it'} unless Sonarr or Radarr answers.`
}

export function DraftsPage() {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const list = useListParams()
  const { kind, draft: openId, set } = useFilters(filters)
  const [mode, setMode] = useState<DraftMode | null>(null)
  const [check, setCheck] = useState<DraftCheck | null>(null)

  const filter = {
    kind: kind || undefined,
    q: list.q || undefined,
    limit: list.size,
    offset: (list.page - 1) * list.size,
  }
  const drafts = useQuery({
    queryKey: ['drafts', filter],
    queryFn: () => api.drafts(filter),
    placeholderData: keepPreviousData,
    enabled: !openId,
  })
  const total = drafts.data?.total ?? 0
  const items = drafts.data?.items ?? []

  const pages = Math.max(1, Math.ceil(total / list.size))
  const { page, setPage } = list
  useEffect(() => {
    if (drafts.data && !drafts.isPlaceholderData && page > pages) setPage(pages)
  }, [drafts.data, drafts.isPlaceholderData, page, pages, setPage])

  // Opening a draft adds a history entry, so back returns to the list.
  const open = (id: number | null) =>
    navigate({
      to: '.',
      search: (prev: Record<string, unknown>) => {
        const next = { ...prev }
        if (id === null) delete next.draft
        else next.draft = id
        return next
      },
    })

  const checking = useMutation({
    mutationFn: (req: CheckRequest) => api.checkTitle(req),
    onSuccess: setCheck,
  })
  const making = useMutation({
    mutationFn: (req: { show: ShowDraftRequest } | { film: FilmDraftRequest }): Promise<StoredDraft> =>
      'show' in req ? api.draftShow(req.show) : api.draftFilm(req.film),
    onSuccess: (made) => {
      setMode(null)
      queryClient.invalidateQueries({ queryKey: ['drafts'] })
      open(made.id)
    },
  })

  function openDialog(next: DraftMode | null) {
    checking.reset()
    making.reset()
    setCheck(null)
    setMode(next)
  }

  const dialog = (
    <DraftDialog
      mode={mode}
      onClose={() => openDialog(null)}
      onCheck={(req) => checking.mutate(req)}
      onShow={(req) => making.mutate({ show: req })}
      onFilm={(req) => making.mutate({ film: req })}
      busy={checking.isPending || making.isPending}
      error={checking.error?.message ?? making.error?.message ?? null}
      check={check}
    />
  )

  if (openId) return <DraftDetail id={Number(openId)} onBack={() => open(null)} onOpen={open} />

  const keyNote = drafts.data ? missingKeys(drafts.data.keys) : null
  const filtered = Boolean(list.q || kind)
  return (
    <div>
      <div>
        <h1 className="text-base font-semibold text-ink">Drafts</h1>
        <p className="mt-2 text-sm text-muted">
          Sonarr only adds shows TheTVDB has and Radarr only films TMDB has. For a title missing there, check it and
          draft the entry from the official channel's uploads, then enter it on the site by hand.
        </p>
        {/* Under the text at every width: beside it, the filters and buttons squeeze the text to a sliver. */}
        <div className="mt-4 grid grid-cols-2 items-end gap-4 sm:flex sm:flex-wrap">
          <div className="col-span-2 sm:w-64">
            <SearchBox id="search-drafts" label="Search" value={list.q} onChange={list.setQ} placeholder="Title" />
          </div>
          <Select
            id="filter-kind"
            label="Kind"
            value={kind}
            onChange={(v) => set('kind', v as typeof kind)}
            options={[
              { value: '', label: 'All' },
              { value: 'show', label: 'Shows' },
              { value: 'film', label: 'Films' },
            ]}
          />
          <div className="flex gap-x-3">
            <button type="button" onClick={() => openDialog('check')} className={secondaryButton}>
              Check
            </button>
            <button type="button" onClick={() => openDialog('new')} className={primaryButton}>
              New draft
            </button>
          </div>
        </div>
      </div>

      {keyNote && total > 0 && (
        <div className="mt-6">
          <Alert tone="info" title="Checks need keys">
            {keyNote} {keysHint}
          </Alert>
        </div>
      )}
      {drafts.isPending && <p className="mt-8 text-sm text-muted">Loading...</p>}
      {drafts.isError && (
        <p role="alert" className="mt-8 text-sm text-brand-text">
          {drafts.error.message}
        </p>
      )}
      {drafts.data && total === 0 && !filtered && (
        <div className="mt-8">
          <EmptyState
            icon={DocumentTextIcon}
            title="No drafts yet"
            text="A draft lays out a show for TheTVDB or a film for TMDB from what Tubarr knows about the official channel, ready to enter by hand. Tubarr never signs in there or posts anything. To check whether a title is already there, set the TheTVDB and TMDB keys in the [drafts] section of the config. Without them drafts are still made, marked as not checked."
            action={
              <div className="flex flex-col items-center gap-y-3">
                <button type="button" onClick={() => openDialog('new')} className={primaryButton}>
                  New draft
                </button>
                <p className="max-w-md text-sm text-muted">{keysHint}</p>
              </div>
            }
          />
        </div>
      )}
      {drafts.data && total === 0 && filtered && (
        <div className="mt-8">
          <EmptyState
            title="No matches"
            text={list.q ? `No drafts match "${list.q}".` : 'No drafts of this kind.'}
            action={
              <button
                type="button"
                onClick={() => {
                  list.setQ('')
                  set('kind', '')
                }}
                className="rounded-md bg-surface px-3 py-2 text-sm font-semibold text-ink shadow-xs ring-1 ring-line ring-inset hover:bg-surface-2"
              >
                Show all
              </button>
            }
          />
        </div>
      )}
      {items.length > 0 && (
        <Table
          footer={<Pager page={list.page} size={list.size} total={total} onPage={list.setPage} onSize={list.setSize} />}
          head={
            <>
              <Th first>Title</Th>
              <Th hideBelow="sm">Kind</Th>
              <Th hideBelow="sm">Check</Th>
              <Th hideBelow="lg">Episodes</Th>
              <Th hideBelow="md">Drafted</Th>
              <Th last>
                <span className="sr-only">Actions</span>
              </Th>
            </>
          }
        >
          {items.map((d) => {
            const verdict = d.verdict ? (
              <Badge tone={verdictTone[d.verdict]}>{verdictLabel[d.verdict]}</Badge>
            ) : (
              <span>Unknown</span>
            )
            const episodes = d.episodes === null ? 'None' : String(d.episodes)
            return (
              <tr key={d.id}>
                <Td first wrap stackBelow="sm">
                  <div className="font-medium text-ink wrap-break-word">{d.title}</div>
                  <Stacked
                    below="lg"
                    items={[
                      { label: 'Kind', value: d.kind, below: 'sm' },
                      { label: 'Check', value: verdict, below: 'sm' },
                      ...(d.kind === 'show' ? [{ label: 'Episodes', value: `${episodes} episodes` }] : []),
                      { label: 'Drafted', value: date(d.created_at), below: 'md' },
                    ]}
                  />
                </Td>
                <Td hideBelow="sm">{d.kind}</Td>
                <Td hideBelow="sm">{verdict}</Td>
                <Td hideBelow="lg">{d.kind === 'show' ? episodes : ''}</Td>
                <Td hideBelow="md">{date(d.created_at)}</Td>
                <td className="py-4 pr-4 pl-3 text-right align-top text-sm font-medium whitespace-nowrap sm:pr-6 sm:align-middle">
                  <button type="button" onClick={() => open(d.id)} className="text-brand-text hover:text-brand-dark">
                    Open<span className="sr-only">, {d.title}</span>
                  </button>
                </td>
              </tr>
            )
          })}
        </Table>
      )}
      {dialog}
    </div>
  )
}
