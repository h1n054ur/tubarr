import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, type Health, type Step, type Tool } from '../lib/api'
import { bytes, seconds, when } from '../lib/format'
import { Badge, type Tone } from '../components/Badge'
import { DescriptionList } from '../components/DescriptionList'
import { secondaryButton } from '../components/fields'
import { Stats } from '../components/Stats'
import { Stacked, Table, Td, Th } from '../components/Table'

const statusTone: Record<Health['status'], Tone> = { ok: 'green', degraded: 'yellow', failing: 'red' }
const stepName: Record<Step, string> = { plain: 'Plain', po_token: 'PO token', cookies: 'Cookies' }

function nextRun(b: Health['backfills'][number]): string {
  if (b.next_run_at === null) return 'Ended'
  return b.next_run_at * 1000 <= Date.now() ? 'Due now' : when(b.next_run_at)
}

type Channel = Health['watcher']['channels'][number]

// A channel's feed and listing state: in their own columns from sm up, under the channel id on phones.
function Feed({ c }: { c: Channel }) {
  return (
    <>
      {when(c.last_feed_at)}
      {c.last_feed_error && (
        <div className="text-brand-text wrap-anywhere">
          {c.last_feed_error} ({c.feed_failures} in a row
          {c.feed_failing_since !== null && `, failing since ${when(c.feed_failing_since)}`})
        </div>
      )}
      {c.feed_fallback && (
        <div className="mt-1">
          <Badge tone="yellow">feed failing, using listing</Badge>
          <div>Last short listing {when(c.last_fallback_at)}</div>
          {c.last_fallback_error && <div className="text-brand-text wrap-anywhere">{c.last_fallback_error}</div>}
        </div>
      )}
    </>
  )
}

function Listing({ c }: { c: Channel }) {
  return (
    <>
      {when(c.last_listing_at)}
      {c.last_listing_error && <div className="text-brand-text wrap-anywhere">{c.last_listing_error}</div>}
    </>
  )
}

function Backfills({ backfills }: { backfills: Health['backfills'] }) {
  return (
    <>
      <h2 className="mt-10 text-base/7 font-semibold text-ink">Channel search backfill</h2>
      <p className="mt-1 text-sm text-muted">
        Back episodes of shows without a playlist, searched on each channel once a day.
      </p>
      {backfills.length === 0 ? (
        <p className="mt-1 text-sm text-muted">No show needs one.</p>
      ) : (
        <Table
          head={
            <>
              <Th first>Show</Th>
              <Th hideBelow="sm">Last run</Th>
              <Th hideBelow="sm">Next run</Th>
              <Th last>Found</Th>
            </>
          }
        >
          {backfills.map((b) => {
            const lastRun = (
              <>
                {when(b.last_run_at)}
                {b.last_error && <div className="text-brand-text wrap-anywhere">{b.last_error}</div>}
              </>
            )
            const next = b.done ? <Badge tone="green">done</Badge> : nextRun(b)
            return (
              <tr key={b.mapping_id}>
                <Td first stackBelow="sm">
                  <span className="font-medium text-ink">{b.title}</span>
                  <Stacked
                    below="sm"
                    items={[
                      { label: 'Last run', value: <div>Last run {lastRun}</div> },
                      { label: 'Next run', value: b.done ? next : `Next run ${next}` },
                    ]}
                  />
                </Td>
                <Td wrap hideBelow="sm">
                  {lastRun}
                </Td>
                <Td hideBelow="sm">{next}</Td>
                <Td last>{b.results_found ?? 'none yet'}</Td>
              </tr>
            )
          })}
        </Table>
      )}
    </>
  )
}

function Binaries({ health }: { health: Health }) {
  const queryClient = useQueryClient()
  const refresh = () => queryClient.invalidateQueries({ queryKey: ['health'] })
  const check = useMutation({ mutationFn: api.checkBinaries, onSuccess: refresh })
  const rollback = useMutation({ mutationFn: (tool: Tool) => api.rollback(tool), onSuccess: refresh })
  // Only the latest action's answer is shown.
  const runCheck = () => {
    rollback.reset()
    check.mutate()
  }
  const runRollback = (tool: Tool) => {
    check.reset()
    rollback.mutate(tool)
  }

  const message = check.isError
    ? check.error.message
    : rollback.isError
      ? rollback.error.message
      : check.data
        ? check.data.length === 0
          ? 'Both binaries have fixed paths in the config, so Tubarr does not update them.'
          : check.data.map((r) => `${r.tool}: ${r.result}`).join('. ')
        : rollback.data
          ? rollback.data.rolled_back
            ? `${rollback.data.tool} is back on the previous version.`
            : `${rollback.data.tool} has no previous version to go back to.`
          : null

  return (
    <DescriptionList
      title="Binaries"
      text="yt-dlp and deno, from GitHub releases. Roll back when a new yt-dlp breaks lookups."
      action={
        <button type="button" onClick={runCheck} disabled={check.isPending} className={secondaryButton}>
          {check.isPending ? 'Checking...' : 'Check now'}
        </button>
      }
      items={[
        ...health.binaries.map((b) => ({
          term: b.tool,
          detail: (
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div>
                <div className="text-ink">{b.installed ?? 'not installed'}</div>
                <div>
                  Latest {b.latest ?? 'unknown'} · checked {when(b.last_check)}
                  {b.last_result ? ` · ${b.last_result}` : ''}
                </div>
              </div>
              {b.can_roll_back && (
                <button
                  type="button"
                  onClick={() => runRollback(b.tool)}
                  disabled={rollback.isPending}
                  className="text-sm font-medium text-brand-text hover:text-brand-dark disabled:opacity-50"
                >
                  Roll back<span className="sr-only"> {b.tool}</span>
                </button>
              )}
            </div>
          ),
        })),
        ...(message ? [{ term: 'Last action', detail: <span role="status">{message}</span> }] : []),
      ]}
    />
  )
}

export function HealthPage() {
  const health = useQuery({ queryKey: ['health'], queryFn: api.health, refetchInterval: 15_000 })

  if (health.isPending) return <p className="text-sm text-muted">Loading...</p>
  if (health.isError)
    return (
      <p role="alert" className="text-sm text-brand-text">
        {health.error.message}
      </p>
    )
  const h = health.data
  const m = h.matcher
  const fallbacks = h.watcher.channels.filter((c) => c.feed_fallback).length

  return (
    <div>
      <div className="flex items-center gap-x-3">
        <h1 className="text-base font-semibold text-ink">Health</h1>
        <span title={h.reasons.join('. ') || undefined}>
          <Badge tone={statusTone[h.status]}>{h.status}</Badge>
        </span>
        <span className="ml-auto text-sm text-muted tabular-nums">
          v{h.version.version}
          {h.version.git_sha && ` (${h.version.git_sha})`}
        </span>
      </div>
      <p className="mt-2 text-sm text-muted">How Tubarr is getting on with YouTube, Sonarr and Radarr.</p>
      {h.reasons.length > 0 && (
        <p className="mt-1 text-sm text-ink">
          {h.status === 'failing' ? 'Failing' : 'Degraded'}: {h.reasons.join(', ')}.
        </p>
      )}

      <div className="mt-8">
        <Stats
          stats={[
            { name: 'Lookup tokens', value: String(Math.floor(h.limiter.available)), unit: `${h.limiter.queued} waiting` },
            { name: 'Canary failures in a row', value: String(h.canary.failures) },
            { name: 'Released', value: String(m.counts.released), unit: `${m.films.counts.released} films` },
            { name: 'Waiting', value: String(m.counts.pending + m.counts.unmatched), unit: `${m.counts.pending} pending` },
          ]}
        />
      </div>

      <Binaries health={h} />

      <DescriptionList
        title="YouTube lookups"
        items={[
          {
            term: 'Rate limiter',
            detail: h.limiter.paused_for_secs
              ? `Paused for ${seconds(h.limiter.paused_for_secs)} after YouTube pushed back`
              : `Next token in ${seconds(h.limiter.next_in_secs)}`,
          },
          {
            term: 'Sticky fallback step',
            detail: h.sticky
              ? `${stepName[h.sticky.step]} for ${seconds(h.sticky.remaining_secs)} more`
              : 'None, lookups start plain',
          },
          {
            term: 'Resolver',
            detail: `${h.resolver.lookups} lookups, ${h.resolver.failures} failed, ${h.resolver.cache_hits} from the cache`,
          },
          {
            term: 'Fallback steps',
            detail: (
              <ul className="space-y-1">
                {h.fallback.map((f) => (
                  <li key={f.step}>
                    {stepName[f.step]}: {f.configured ? `${f.succeeded} of ${f.attempts} worked` : 'not configured'}
                  </li>
                ))}
              </ul>
            ),
          },
        ]}
      />

      <DescriptionList
        title="Canary"
        text={
          h.canary.enabled
            ? `Looks up ${h.canary.video_id} every ${seconds(h.canary.interval_secs)}.`
            : 'Off: no canary video in the config.'
        }
        items={[
          { term: 'Skipped ticks', detail: String(h.canary.skipped) },
          { term: 'Last forced update', detail: when(h.canary.last_forced_update) },
          {
            term: 'History',
            detail:
              h.canary.history.length === 0 ? (
                'No runs yet'
              ) : (
                <ul className="space-y-1">
                  {[...h.canary.history].reverse().map((r) => (
                    <li key={r.at} className="flex flex-wrap items-center gap-x-2">
                      <Badge tone={r.ok ? 'green' : 'red'}>{r.ok ? 'ok' : 'failed'}</Badge>
                      {when(r.at)}
                      {r.step ? ` · ${stepName[r.step]}` : ''}
                      {r.error ? ` · ${r.error}` : ''}
                    </li>
                  ))}
                </ul>
              ),
          },
        ]}
      />

      <DescriptionList
        title="Matcher"
        items={[
          {
            term: 'Last run',
            detail:
              m.last_run_at === null
                ? 'never'
                : `${when(m.last_run_at)} · ${m.matched} mappings matched, ${m.unchanged} unchanged`,
          },
          { term: 'Last error', detail: m.last_error ?? 'None' },
          { term: 'Lookups last run', detail: String(m.lookups) },
          { term: 'Capped by backfill limit', detail: String(m.counts.capped) },
          {
            term: 'Films',
            detail: `${m.films.wanted} wanted, ${m.films.counts.released} released, ${m.films.counts.pending} pending${
              m.films.radarr_error ? ` · Radarr: ${m.films.radarr_error}` : ''
            }`,
          },
          {
            term: 'Skipped mappings',
            detail:
              m.skipped.length === 0 ? 'None' : m.skipped.map(([id, why]) => `mapping ${id}: ${why}`).join('. '),
          },
        ]}
      />

      <DescriptionList
        title="Playback"
        text="Plays running now. A first play reads its video's sample tables; plays past the stream cap get a busy answer."
        items={[
          {
            term: 'First plays',
            detail: `${h.play.builds} of ${h.play.max_builds} reading${
              h.play.builds_waiting > 0 ? `, ${h.play.builds_waiting} waiting` : ''
            }`,
          },
          {
            term: 'Streams',
            detail: h.play.max_streams === 0 ? `${h.play.streams}, no cap` : `${h.play.streams} of ${h.play.max_streams}`,
          },
        ]}
      />

      <DescriptionList
        title="Storage"
        text="What grows with the library's history. Each store is pruned by its retention rule."
        items={[
          { term: 'Unmatched notes', detail: String(h.storage.notes) },
          { term: 'Watched videos', detail: String(h.storage.channel_videos) },
          { term: 'Download history', detail: `${h.storage.grabs} grabs` },
          { term: 'Activity events', detail: `${h.storage.activity_events} searches, pushes and plays` },
          {
            term: 'Sample tables',
            detail: `${h.storage.play_index.files} files, ${bytes(h.storage.play_index.bytes)}`,
          },
        ]}
      />

      {h.jellyfin && (
        <DescriptionList
          title="Jellyfin versions"
          text="Audio only files waiting to be merged into their episodes, or split off them."
          items={[
            { term: 'Queue', detail: String(h.jellyfin.queued) },
            {
              term: 'Last error',
              detail: h.jellyfin.last_error ? `${h.jellyfin.last_error} (${when(h.jellyfin.last_error_at)})` : 'None',
            },
          ]}
        />
      )}

      <h2 className="mt-10 text-base/7 font-semibold text-ink">Watcher</h2>
      {fallbacks > 0 && (
        <p className="mt-1 text-sm text-muted">
          {fallbacks === 1
            ? "1 channel's feed is failing, so its newest uploads are listed instead."
            : `${fallbacks} channels' feeds are failing, so their newest uploads are listed instead.`}
        </p>
      )}
      {!h.watcher.active || h.watcher.channels.length === 0 ? (
        <p className="mt-1 text-sm text-muted">Idle: no mapping names a channel yet.</p>
      ) : (
        <Table
          head={
            <>
              <Th first>Channel</Th>
              <Th hideBelow="sm">Feed</Th>
              <Th hideBelow="sm">Listing</Th>
              <Th last>Videos</Th>
            </>
          }
        >
          {h.watcher.channels.map((c) => (
            <tr key={c.channel_id}>
              <Td first stackBelow="sm">
                <span className="font-medium text-ink wrap-anywhere">{c.channel_id}</span>
                <Stacked
                  below="sm"
                  items={[
                    { label: 'Feed', value: <div>Feed <Feed c={c} /></div> },
                    { label: 'Listing', value: <div>Listing <Listing c={c} /></div> },
                  ]}
                />
              </Td>
              <Td wrap hideBelow="sm">
                <Feed c={c} />
              </Td>
              <Td wrap hideBelow="sm">
                <Listing c={c} />
              </Td>
              <Td last>
                {c.videos_new} new of {c.videos_seen}
              </Td>
            </tr>
          ))}
        </Table>
      )}

      <Backfills backfills={h.backfills} />
    </div>
  )
}
