import { useRef, useState, type ChangeEvent } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { api, mappingExportUrl, type ImportReport, type MappingExport } from '../lib/api'
import { primaryButton, secondaryButton } from './fields'

/** One line for a report: what it creates, updates, leaves alone and skips. */
function summary(r: ImportReport) {
  const [n, c, u, k] = [r.creates.length, r.updates.length, r.unchanged.length, r.conflicts.length]
  const mappings = n === 1 ? 'mapping' : 'mappings'
  return r.dry_run
    ? `Would add ${n} ${mappings}, update ${c}, leave ${u} as they are and skip ${k} with a conflict.`
    : `Added ${n} ${mappings}, updated ${c}, left ${u} as they were and skipped ${k} with a conflict.`
}

function ReportView({ report }: { report: ImportReport }) {
  const titles = (items: { title: string }[]) => items.map((i) => i.title).join(', ')
  return (
    <div role="status" className="text-sm text-muted">
      <p className="text-ink">{summary(report)}</p>
      {report.creates.length > 0 && <p className="mt-2">New: {titles(report.creates)}</p>}
      {report.updates.length > 0 && <p className="mt-1">Updated: {titles(report.updates)}</p>}
      {report.conflicts.length > 0 && (
        <ul className="mt-2 list-disc space-y-1 pl-5">
          {report.conflicts.map((c, i) => (
            <li key={i}>
              <span className="text-ink">{c.title || 'A mapping without a title'}</span>: {c.reason}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

/** Export every mapping with its aliases and pins as one file, and import such a file after a dry
 * run shows what it would change. */
export function MappingTransfer() {
  const queryClient = useQueryClient()
  const input = useRef<HTMLInputElement>(null)
  const [file, setFile] = useState<{ name: string; data: MappingExport } | null>(null)
  const [readError, setReadError] = useState<string | null>(null)
  const check = useMutation({ mutationFn: (data: MappingExport) => api.importMappings(data, true) })
  const apply = useMutation({
    mutationFn: (data: MappingExport) => api.importMappings(data, false),
    onSuccess: () => {
      setFile(null)
      check.reset()
      queryClient.invalidateQueries({ queryKey: ['mappings'] })
    },
  })

  async function choose(e: ChangeEvent<HTMLInputElement>) {
    const picked = e.target.files?.[0]
    e.target.value = ''
    if (!picked) return
    setReadError(null)
    apply.reset()
    let data: MappingExport
    try {
      data = JSON.parse(await picked.text())
    } catch {
      setFile(null)
      check.reset()
      setReadError(`${picked.name} is not a Tubarr mapping export.`)
      return
    }
    setFile({ name: picked.name, data })
    check.mutate(data)
  }

  const error = readError ?? (check.isError ? check.error.message : apply.isError ? apply.error.message : null)
  const canApply = file !== null && check.data !== undefined && check.data.creates.length + check.data.updates.length > 0

  return (
    // Framed like AddMapping: heading in the surface-2 header, actions in a footer row.
    <section className="mt-10 overflow-hidden rounded-lg border border-line bg-surface shadow-sm">
      <div className="bg-surface-2 px-4 py-5 sm:px-6">
        <h2 className="text-base/7 font-semibold text-ink">Export and import mappings</h2>
        <p className="mt-1 max-w-2xl text-sm/6 text-muted">
          One file with every mapping, its rules, aliases and pins, to move them to another Tubarr or keep a copy. An
          import matches mappings by TVDB or TMDB id and shows what it would change before anything is written.
        </p>
      </div>
      <div className="space-y-4 border-t border-line px-4 py-6 sm:px-6">
        {file && (
          <p className="text-sm text-muted">
            Checked <span className="text-ink">{file.name}</span>.
          </p>
        )}
        {check.isPending && <p className="text-sm text-muted">Checking the file...</p>}
        {apply.data ? <ReportView report={apply.data} /> : check.data && <ReportView report={check.data} />}
        {error && (
          <p role="alert" className="text-sm text-brand-text">
            {error}
          </p>
        )}
        {!file && !apply.data && !check.isPending && !error && (
          <p className="text-sm text-muted">No file chosen.</p>
        )}
      </div>
      <div className="flex flex-wrap items-center justify-end gap-x-4 gap-y-2 border-t border-line px-4 py-4 sm:px-6">
        <a href={mappingExportUrl} download="tubarr-mappings.json" className={secondaryButton}>
          Download export
        </a>
        <input
          ref={input}
          type="file"
          accept="application/json,.json"
          aria-label="Import file"
          onChange={choose}
          className="hidden"
        />
        <button type="button" onClick={() => input.current?.click()} className={secondaryButton}>
          Choose file to import
        </button>
        <button
          type="button"
          onClick={() => file && apply.mutate(file.data)}
          disabled={!canApply || apply.isPending}
          className={primaryButton}
        >
          {apply.isPending ? 'Importing...' : 'Apply import'}
        </button>
      </div>
    </section>
  )
}
