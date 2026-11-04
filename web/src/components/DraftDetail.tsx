// One stored draft: the check, the drafted fields and the block to copy into TheTVDB's or TMDB's add
// form. Drafts are not edited here: Draft again makes a fresh one from the same request (more
// episodes may be out by now) and replaces this one.
import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeftIcon } from '@heroicons/react/20/solid'
import { api, type FilmDraftRequest, type ShowDraftRequest, type StoredDraft } from '../lib/api'
import { dateTime } from '../lib/date'
import { Badge } from './Badge'
import { DescriptionList, type Item } from './DescriptionList'
import { verdictLabel, verdictTone } from './DraftDialog'
import { primaryButton, secondaryButton } from './fields'

type Fields = Record<string, unknown>

function value(v: unknown): string | null {
  if (v === null || v === undefined || v === '') return null
  if (typeof v === 'boolean') return v ? 'Yes' : 'No'
  return String(v)
}

/** The drafted fields worth a glance, in the site's form order. Empty ones are left out. */
export function draftItems(draft: StoredDraft): Item[] {
  const d = draft.draft as Fields
  const rows: [string, unknown][] =
    draft.kind === 'show'
      ? [
          ['Series name', d.series_name],
          ['Original title', d.original_title],
          ['Network', d.network],
          ['Country', d.country],
          ['Language', d.original_language],
          ['First aired', d.first_aired],
          ['Status', d.status],
          ['Numbering', d.numbering],
          ['Episodes', Array.isArray(d.episodes) ? d.episodes.length : null],
          ['Uploads left out', Array.isArray(d.skipped) && d.skipped.length > 0 ? d.skipped.length : null],
          ['YouTube channel', d.youtube_channel],
        ]
      : [
          ['Title', d.title],
          ['Original title', d.original_title],
          ['Year', d.year === null || d.year === undefined ? null : `${d.year}${d.year_from_upload ? ' (upload year)' : ''}`],
          ['Runtime', d.runtime_mins === null || d.runtime_mins === undefined ? null : `${d.runtime_mins} min`],
          ['Language', d.language],
          ['Country', d.country],
          ['Network', d.network],
          ['TV film', d.tv_film],
          ['Part', d.part],
          ['YouTube', d.youtube_url],
        ]
  return rows.flatMap(([term, v]) => {
    const detail = value(v)
    return detail === null ? [] : [{ term, detail }]
  })
}

function redraft(draft: StoredDraft): Promise<StoredDraft> {
  return draft.kind === 'show'
    ? api.draftShow(draft.request as ShowDraftRequest)
    : api.draftFilm(draft.request as FilmDraftRequest)
}

export function DraftDetail({ id, onBack, onOpen }: { id: number; onBack: () => void; onOpen: (id: number) => void }) {
  const queryClient = useQueryClient()
  const [confirming, setConfirming] = useState(false)
  const [copied, setCopied] = useState(false)
  const draft = useQuery({ queryKey: ['draft', id], queryFn: () => api.draft(id) })

  const again = useMutation({
    mutationFn: async (old: StoredDraft) => {
      const fresh = await redraft(old)
      await api.deleteDraft(old.id)
      return fresh
    },
    onSuccess: (fresh) => {
      queryClient.invalidateQueries({ queryKey: ['drafts'] })
      onOpen(fresh.id)
    },
  })
  const remove = useMutation({
    mutationFn: () => api.deleteDraft(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['drafts'] })
      onBack()
    },
  })

  async function copy(text: string) {
    await navigator.clipboard.writeText(text)
    setCopied(true)
  }

  const back = (
    <button type="button" onClick={onBack} className="inline-flex items-center gap-x-1 text-sm text-muted hover:text-ink">
      <ArrowLeftIcon aria-hidden="true" className="size-4" />
      All drafts
    </button>
  )
  if (draft.isPending) return <div>{back}<p className="mt-8 text-sm text-muted">Loading...</p></div>
  if (draft.isError)
    return (
      <div>
        {back}
        <p role="alert" className="mt-8 text-sm text-brand-text">
          {draft.error.message}
        </p>
      </div>
    )

  const d = draft.data
  const check = d.draft.check
  const site = d.kind === 'show' ? 'TheTVDB' : 'TMDB'
  const error = again.error?.message ?? remove.error?.message
  return (
    <div>
      {back}
      <div className="mt-4 lg:flex lg:items-end lg:gap-x-8">
        <div className="lg:min-w-0 lg:flex-auto">
          <h1 className="text-base font-semibold text-ink wrap-anywhere">{d.title}</h1>
          <div className="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-muted">
            <Badge tone="gray">{d.kind}</Badge>
            <Badge tone={verdictTone[check.verdict]}>{verdictLabel[check.verdict]}</Badge>
            <span>Drafted {dateTime(d.created_at)}</span>
          </div>
        </div>
        <div className="mt-4 flex flex-wrap items-center gap-3 lg:mt-0 lg:shrink-0">
          {confirming ? (
            <>
              <button
                type="button"
                onClick={() => remove.mutate()}
                disabled={remove.isPending}
                className="text-sm font-medium text-brand-text hover:text-brand"
              >
                Yes, delete this draft
              </button>
              <button type="button" onClick={() => setConfirming(false)} className="text-sm font-medium text-muted hover:text-ink">
                Keep
              </button>
            </>
          ) : (
            <button type="button" onClick={() => setConfirming(true)} className={secondaryButton}>
              Delete
            </button>
          )}
          <button type="button" onClick={() => again.mutate(d)} disabled={again.isPending} className={primaryButton}>
            {again.isPending ? 'Drafting...' : 'Draft again'}
          </button>
        </div>
      </div>
      {error && (
        <p role="alert" className="mt-4 text-sm text-brand-text">
          {error}
        </p>
      )}

      <DescriptionList
        title="Drafted fields"
        text={`Enter these on ${site} by hand. Tubarr never signs in there or sends anything.`}
        items={draftItems(d)}
      />

      <section className="mt-10 overflow-hidden rounded-lg border border-line bg-surface shadow-sm">
        <div className="flex items-start justify-between gap-x-4 bg-surface-2 px-4 py-5 sm:px-6">
          <div>
            <h2 className="text-base/7 font-semibold text-ink">Text to copy</h2>
            <p className="mt-1 max-w-2xl text-sm/6 text-muted">
              The check, then every field and episode in the order of {site}'s add form.
            </p>
          </div>
          <button type="button" onClick={() => copy(d.text)} className={secondaryButton}>
            {copied ? 'Copied' : 'Copy'}
          </button>
        </div>
        <pre className="overflow-x-auto border-t border-line px-4 py-4 text-xs/5 whitespace-pre-wrap text-ink sm:px-6">
          {d.text}
        </pre>
      </section>
    </div>
  )
}
