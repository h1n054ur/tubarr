import { useEffect, useState } from 'react'
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { CheckCircleIcon } from '@heroicons/react/24/outline'
import { api, type NewPin, type UnmatchedNote, type UnmatchedState } from '../lib/api'
import { minutes, uploadDate, youtube } from '../lib/format'
import { Badge, type Tone } from '../components/Badge'
import { EmptyState } from '../components/EmptyState'
import { Select } from '../components/fields'
import { Pager } from '../components/Pager'
import { PinDialog } from '../components/PinDialog'
import { SearchBox } from '../components/SearchBox'
import { Stacked, Table, Td, Th } from '../components/Table'
import { oneOf, useFilters, useListParams, type FilterParam } from '../lib/urlState'

const stateTone: Record<UnmatchedState, Tone> = {
  pending: 'cyan',
  duplicate: 'gray',
  unsure: 'yellow',
  partial: 'red',
  capped: 'gray',
}

// The filters live in the URL (`?reason=unsure&mapping=12&show=all`) next to the search and page.
const filters = {
  reason: oneOf(['unsure', 'pending', 'duplicate', 'partial', 'capped']),
  mapping: { values: (v: string) => /^[1-9]\d*$/.test(v), default: '' } as FilterParam,
  show: oneOf(['open', 'all'], 'open'),
}

export function UnmatchedPage() {
  const queryClient = useQueryClient()
  const [pinning, setPinning] = useState<UnmatchedNote | null>(null)
  const list = useListParams()
  const { reason, mapping, show, set } = useFilters(filters)

  const filter = {
    state: reason || undefined,
    mapping_id: mapping ? Number(mapping) : undefined,
    ignored: show === 'all' ? true : undefined,
    q: list.q || undefined,
    limit: list.size,
    offset: (list.page - 1) * list.size,
  }
  // The last page stays on screen while the next one loads, so the table does not flash empty.
  const notes = useQuery({
    queryKey: ['unmatched', filter],
    queryFn: () => api.unmatched(filter),
    placeholderData: keepPreviousData,
  })
  const total = notes.data?.total ?? 0
  const items = notes.data?.items ?? []

  // Past the end (the last row on the last page ignored, or a stale link): go to the last page.
  const pages = Math.max(1, Math.ceil(total / list.size))
  const { page, setPage } = list
  useEffect(() => {
    if (notes.data && !notes.isPlaceholderData && page > pages) setPage(pages)
  }, [notes.data, notes.isPlaceholderData, page, pages, setPage])
  const mappings = useQuery({ queryKey: ['mappings', {}], queryFn: () => api.mappings() })

  const ignore = useMutation({
    mutationFn: ({ id, ignored }: { id: number; ignored: boolean }) => api.setIgnored(id, ignored),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['unmatched'] }),
  })
  const pin = useMutation({
    mutationFn: (p: NewPin) => api.pin(p),
    onSuccess: () => {
      setPinning(null)
      queryClient.invalidateQueries({ queryKey: ['unmatched'] })
    },
  })

  function openPin(note: UnmatchedNote | null) {
    pin.reset()
    setPinning(note)
  }

  return (
    <div>
      <div>
        <h1 className="text-base font-semibold text-ink">Unmatched</h1>
        <p className="mt-2 text-sm text-muted">
          Videos the matcher saw for a mapping but did not release, and why. Pin one to settle it, or ignore it once you
          have looked.
        </p>
        {/* Four filters do not fit beside the text, so they sit under it at every width. Phones: Search, then
            Reason and Show side by side, then Mapping (long titles) full width. Wider: one wrapping row, with
            Mapping capped so a long title cannot widen it. */}
        <div className="mt-4 grid grid-cols-2 items-end gap-4 sm:flex sm:flex-wrap">
          <div className="col-span-2 sm:w-64">
            <SearchBox
              id="search-unmatched"
              label="Search"
              value={list.q}
              onChange={list.setQ}
              placeholder="Title, video id, channel"
            />
          </div>
          <Select
            id="filter-state"
            label="Reason"
            value={reason}
            onChange={(v) => set('reason', v as typeof reason)}
            options={[
              { value: '', label: 'All' },
              { value: 'unsure', label: 'Unsure' },
              { value: 'pending', label: 'Pending' },
              { value: 'duplicate', label: 'Duplicate' },
              { value: 'partial', label: 'Partial' },
              { value: 'capped', label: 'Capped' },
            ]}
          />
          <div className="order-last col-span-2 min-w-0 sm:order-none sm:w-64">
            <Select
              id="filter-mapping"
              label="Mapping"
              value={mapping}
              onChange={(v) => set('mapping', v)}
              options={[
                { value: '', label: 'All' },
                ...(mappings.data ?? []).map((m) => ({ value: String(m.id), label: m.title })),
              ]}
            />
          </div>
          <Select
            id="filter-ignored"
            label="Show"
            value={show}
            onChange={(v) => set('show', v as typeof show)}
            options={[
              { value: 'open', label: 'Not ignored' },
              { value: 'all', label: 'All, with ignored' },
            ]}
          />
        </div>
      </div>

      {notes.isPending && <p className="mt-8 text-sm text-muted">Loading...</p>}
      {notes.isError && (
        <p role="alert" className="mt-8 text-sm text-brand-text">
          {notes.error.message}
        </p>
      )}
      {ignore.isError && (
        <p role="alert" className="mt-4 text-sm text-brand-text">
          {ignore.error.message}
        </p>
      )}
      {notes.data && total === 0 && !list.q && (
        <div className="mt-8">
          <EmptyState
            icon={CheckCircleIcon}
            title="Nothing unmatched"
            text="Every video the matcher saw became a release or fits no mapping."
          />
        </div>
      )}
      {notes.data && total === 0 && list.q && (
        <div className="mt-8">
          <EmptyState
            title="No matches"
            text={`No unmatched videos match "${list.q}".`}
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
      {items.length > 0 && (
        <Table
          footer={<Pager page={list.page} size={list.size} total={total} onPage={list.setPage} onSize={list.setSize} />}
          head={
            <>
              <Th first>Video</Th>
              <Th hideBelow="md">Mapping</Th>
              <Th hideBelow="xl">Length</Th>
              <Th hideBelow="xl">Uploaded</Th>
              <Th hideBelow="sm">Reason</Th>
              <Th last>
                <span className="sr-only">Actions</span>
              </Th>
            </>
          }
        >
          {items.map((n) => {
            const length = minutes(n.duration_secs)
            const uploaded = uploadDate(n.published_at, n.upload_date)
            const badges = (
              <div className="flex items-center gap-x-2">
                <Badge tone={stateTone[n.state]}>{n.state}</Badge>
                {n.ignored && <Badge tone="gray">ignored</Badge>}
              </div>
            )
            return (
              <tr key={n.id} className={n.ignored ? 'opacity-60' : undefined}>
                <Td first wrap stackBelow="sm">
                  <div className="font-medium text-ink wrap-break-word">{n.video_title ?? n.video_id}</div>
                  <div className="mt-1 text-muted wrap-anywhere">
                    {n.video_id}
                    {n.channel_id ? ` · ${n.channel_id}` : ''}
                  </div>
                  <Stacked
                    below="xl"
                    items={[
                      { label: 'Mapping', value: n.mapping_title, below: 'md' },
                      { label: 'Length', value: length },
                      { label: 'Uploaded', value: uploaded },
                      {
                        label: 'Reason',
                        below: 'sm',
                        value: (
                          <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                            {badges}
                            <span>{n.reason}</span>
                          </div>
                        ),
                      },
                    ]}
                  />
                </Td>
                <Td wrap hideBelow="md">
                  {n.mapping_title}
                </Td>
                <Td hideBelow="xl">{length}</Td>
                <Td hideBelow="xl">{uploaded}</Td>
                <Td wrap hideBelow="sm">
                  {badges}
                  <div className="mt-1">{n.reason}</div>
                </Td>
                {/* The actions stay in their own column; on phones they stack so the title keeps its width. */}
                <td className="py-4 pr-4 pl-3 text-right align-top text-sm font-medium whitespace-nowrap sm:pr-6 sm:align-middle">
                  <div className="flex flex-col items-end gap-y-3 sm:flex-row sm:justify-end sm:gap-x-4">
                    <button type="button" onClick={() => openPin(n)} className="text-brand-text hover:text-brand-dark">
                      Pin<span className="sr-only">, {n.video_id}</span>
                    </button>
                    <button
                      type="button"
                      onClick={() => ignore.mutate({ id: n.id, ignored: !n.ignored })}
                      className="text-muted hover:text-ink"
                    >
                      {n.ignored ? 'Show again' : 'Ignore'}
                      <span className="sr-only">, {n.video_id}</span>
                    </button>
                    <a href={youtube(n.video_id)} target="_blank" rel="noreferrer" className="text-accent hover:text-ink">
                      YouTube<span className="sr-only">, {n.video_id}</span>
                    </a>
                  </div>
                </td>
              </tr>
            )
          })}
        </Table>
      )}

      <PinDialog
        note={pinning}
        onClose={() => openPin(null)}
        onPin={(p) => pin.mutate(p)}
        saving={pin.isPending}
        error={pin.error?.message ?? null}
      />
    </div>
  )
}
