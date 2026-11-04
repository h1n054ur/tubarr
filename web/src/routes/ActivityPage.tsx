import { useEffect } from 'react'
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, type ActivityEvent } from '../lib/api'
import { when } from '../lib/format'
import { Badge } from '../components/Badge'
import { EmptyState } from '../components/EmptyState'
import { Select, secondaryButton } from '../components/fields'
import { Pager } from '../components/Pager'
import { Stacked, Table, Td, Th } from '../components/Table'
import { oneOf, useFilters, useListParams } from '../lib/urlState'

function eventDetail(e: ActivityEvent) {
  if (e.kind === 'search') return { what: `${e.function}: ${e.query || 'no terms'}`, result: `${e.results} results` }
  if (e.kind === 'arr_push')
    return {
      what: `pushed to ${e.app}: ${e.release}`,
      result: e.reasons.length > 0 ? `${e.outcome}: ${e.reasons.join('; ')}` : e.outcome,
    }
  return { what: e.video_ids, result: `${e.status}${e.client ? ` · ${e.client}` : ''}` }
}

function eventTone(e: ActivityEvent) {
  if (e.kind === 'search') return 'cyan'
  if (e.kind === 'play') return 'green'
  return e.outcome === 'failed' || e.outcome === 'rejected' ? 'red' : 'yellow'
}

// The filters live in the URL (`?category=tv&kind=play`). Both lists page on the server, the grabs
// under `page`/`size` and the events under `events_page`/`events_size`, so each filter goes back to
// page 1 of its own list only.
const filters = {
  category: oneOf(['tv', 'movies']),
  kind: oneOf(['search', 'play', 'arr_push'], '', { resetsPage: 'events_page' }),
}

function kept(retentionSecs: number) {
  if (retentionSecs === 0) return 'Kept with no time limit.'
  const days = Math.round(retentionSecs / 86_400)
  if (days >= 1) return `Kept for ${days} ${days === 1 ? 'day' : 'days'}, across restarts.`
  const hours = Math.max(1, Math.round(retentionSecs / 3600))
  return `Kept for ${hours} ${hours === 1 ? 'hour' : 'hours'}, across restarts.`
}

/** Past the last page (a stale link, or rows pruned since): go to the last page. */
function useClampPage(
  query: { data?: { total: number }; isPlaceholderData: boolean },
  list: { page: number; size: number; setPage: (page: number) => void },
) {
  const pages = Math.max(1, Math.ceil((query.data?.total ?? 0) / list.size))
  const { data, isPlaceholderData } = query
  const { page, setPage } = list
  useEffect(() => {
    if (data && !isPlaceholderData && page > pages) setPage(pages)
  }, [data, isPlaceholderData, page, pages, setPage])
}

export function ActivityPage() {
  const { category, kind, set } = useFilters(filters)
  const list = useListParams()
  const paging = { limit: list.size, offset: (list.page - 1) * list.size }
  const activity = useQuery({
    queryKey: ['activity', category, paging],
    queryFn: () => api.activity(category || undefined, paging),
    refetchInterval: 15_000,
    placeholderData: keepPreviousData,
  })
  useClampPage(activity, list)
  const eventList = useListParams('events_')
  const eventPaging = { limit: eventList.size, offset: (eventList.page - 1) * eventList.size }
  const events = useQuery({
    queryKey: ['activity', 'events', kind, eventPaging],
    queryFn: () => api.activityEvents(kind || undefined, eventPaging),
    refetchInterval: 15_000,
    placeholderData: keepPreviousData,
  })
  useClampPage(events, eventList)
  const queryClient = useQueryClient()
  const pushReleased = useMutation({
    mutationFn: api.pushReleased,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['activity'] }),
  })

  return (
    <div>
      <div className="sm:flex sm:items-end">
        <div className="sm:flex-auto">
          <h1 className="text-base font-semibold text-ink">Activity</h1>
          <p className="mt-2 text-sm text-muted">
            What Sonarr, Radarr and Jellyfin asked for (grabs, indexer searches, play starts) and the releases Tubarr pushed to the arrs.
          </p>
        </div>
        <div className="mt-4 grid grid-cols-2 items-end gap-4 sm:mt-0 sm:ml-16 sm:flex">
          <button
            type="button"
            onClick={() => pushReleased.mutate()}
            disabled={pushReleased.isPending}
            className={secondaryButton}
            title="Push every episode and film Tubarr has a release for, if Sonarr or Radarr still wants it"
          >
            Push released
          </button>
          <Select
            id="filter-category"
            label="Category"
            value={category}
            onChange={(v) => set('category', v as typeof category)}
            options={[
              { value: '', label: 'All' },
              { value: 'tv', label: 'TV' },
              { value: 'movies', label: 'Movies' },
            ]}
          />
        </div>
      </div>

      {pushReleased.isSuccess && (
        <p role="status" className="mt-4 text-sm text-muted">
          Queued {pushReleased.data.queued} for a push, {pushReleased.data.waiting} waiting. Each shows below as it goes
          out.
        </p>
      )}
      {pushReleased.isError && (
        <p role="alert" className="mt-4 text-sm text-brand-text">
          {pushReleased.error.message}
        </p>
      )}
      {activity.isPending && <p className="mt-8 text-sm text-muted">Loading...</p>}
      {activity.isError && (
        <p role="alert" className="mt-8 text-sm text-brand-text">
          {activity.error.message}
        </p>
      )}

      {activity.data && (
        <>
          <h2 className="mt-10 text-sm font-semibold text-ink">Grabs</h2>
          {activity.data.total === 0 ? (
            <div className="mt-4">
              <EmptyState title="No grabs yet" text="A grab shows here once Sonarr or Radarr sends a release to Tubarr." />
            </div>
          ) : (
            <Table
              footer={
                <Pager
                  page={list.page}
                  size={list.size}
                  total={activity.data.total}
                  onPage={list.setPage}
                  onSize={list.setSize}
                />
              }
              head={
                <>
                  <Th first>Release</Th>
                  <Th hideBelow="sm">Category</Th>
                  <Th hideBelow="sm">When</Th>
                  <Th last hideBelow="sm">
                    Status
                  </Th>
                </>
              }
            >
              {activity.data.grabs.map((g) => {
                const status =
                  g.status === 'completed' ? <Badge tone="green">completed</Badge> : <Badge tone="red">failed</Badge>
                return (
                  <tr key={g.nzo_id}>
                    <Td first wrap stackBelow="sm">
                      <div className="font-medium text-ink wrap-break-word">{g.release_name}</div>
                      <div className="mt-1 text-muted wrap-break-word">{g.storage_path}</div>
                      <Stacked
                        below="sm"
                        items={[
                          { label: 'Status', value: status },
                          { label: 'Category', value: g.category },
                          { label: 'When', value: when(g.created_at) },
                        ]}
                      />
                    </Td>
                    <Td hideBelow="sm">{g.category}</Td>
                    <Td hideBelow="sm">{when(g.created_at)}</Td>
                    <Td last hideBelow="sm">
                      {status}
                    </Td>
                  </tr>
                )
              })}
            </Table>
          )}

          <div className="mt-12 sm:flex sm:items-end">
            <div className="sm:flex-auto">
              <h2 className="text-sm font-semibold text-ink">Searches, pushes and plays</h2>
              {events.data && <p className="mt-1 text-sm text-muted">{kept(events.data.retention_secs)}</p>}
            </div>
            <div className="mt-4 sm:mt-0 sm:ml-16">
              <Select
                id="filter-kind"
                label="Kind"
                value={kind}
                onChange={(v) => set('kind', v as typeof kind)}
                options={[
                  { value: '', label: 'All' },
                  { value: 'search', label: 'Searches' },
                  { value: 'play', label: 'Plays' },
                  { value: 'arr_push', label: 'Arr pushes' },
                ]}
              />
            </div>
          </div>
          {events.isError && (
            <p role="alert" className="mt-4 text-sm text-brand-text">
              {events.error.message}
            </p>
          )}
          {events.data && events.data.total === 0 ? (
            <div className="mt-4">
              <EmptyState title="Nothing yet" text="Searches and play starts show here as they happen." />
            </div>
          ) : events.data ? (
            <Table
              footer={
                <Pager
                  id="events-pager-size"
                  page={eventList.page}
                  size={eventList.size}
                  total={events.data.total}
                  onPage={eventList.setPage}
                  onSize={eventList.setSize}
                />
              }
              head={
                <>
                  <Th first>Kind</Th>
                  <Th hideBelow="sm">Request</Th>
                  <Th hideBelow="sm">Result</Th>
                  <Th last hideBelow="sm">
                    When
                  </Th>
                </>
              }
            >
              {events.data.items.map((e, i) => {
                const d = eventDetail(e)
                return (
                  <tr key={`${e.at}-${i}`}>
                    <Td first stackBelow="sm">
                      <div className="flex items-center justify-between gap-x-3 sm:block">
                        <Badge tone={eventTone(e)}>{e.kind}</Badge>
                        <span className="font-normal whitespace-nowrap text-muted sm:hidden">{when(e.at)}</span>
                      </div>
                      <dl className="mt-2 space-y-1 font-normal text-muted sm:hidden">
                        <dt className="sr-only">Request</dt>
                        <dd className="text-ink wrap-break-word">{d.what}</dd>
                        <dt className="sr-only">Result</dt>
                        <dd className="wrap-break-word">{d.result}</dd>
                      </dl>
                    </Td>
                    <Td wrap hideBelow="sm">
                      {d.what}
                    </Td>
                    <Td wrap hideBelow="sm">
                      {d.result}
                    </Td>
                    <Td last hideBelow="sm">
                      {when(e.at)}
                    </Td>
                  </tr>
                )
              })}
            </Table>
          ) : null}
        </>
      )}
    </div>
  )
}
