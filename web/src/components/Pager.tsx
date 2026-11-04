// Ported from Tailwind Plus application-ui/navigation/pagination/card-footer-with-page-buttons,
// recoloured with the Tubarr tokens, with buttons in place of links and a page size choice. The
// caller pages the rows (in the browser or on the server); this only shows where it is.
import { ChevronLeftIcon, ChevronRightIcon } from '@heroicons/react/20/solid'
import { pageSizes, type PageSize } from '../lib/urlState'

export type PageItem = number | 'gap'

/** First, last, the current page and its neighbours, with a gap where pages are left out. A gap
 * that would hide a single page shows that page instead. */
export function pageItems(page: number, pages: number): PageItem[] {
  const shown = new Set([1, pages, page - 1, page, page + 1])
  const items: PageItem[] = []
  for (let p = 1; p <= pages; p++) {
    if (shown.has(p)) items.push(p)
    else if (shown.has(p - 1) && shown.has(p + 1)) items.push(p)
    else if (items[items.length - 1] !== 'gap') items.push('gap')
  }
  return items
}

const cell = 'relative inline-flex items-center px-4 py-2 text-sm font-semibold focus:z-20'
const plain = `${cell} text-ink inset-ring inset-ring-line hover:bg-surface-2 focus:outline-offset-0`
const current = `${cell} z-10 bg-brand text-on-brand focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand`
const arrow =
  'relative inline-flex items-center px-2 py-2 text-muted inset-ring inset-ring-line hover:bg-surface-2 focus:z-20 focus:outline-offset-0 disabled:opacity-50 disabled:hover:bg-transparent'
const mobileButton =
  'relative inline-flex items-center rounded-md bg-surface px-4 py-2 text-sm font-medium text-ink ring-1 ring-line ring-inset hover:bg-surface-2 disabled:opacity-50'

/** Hidden when everything fits the smallest page; with more rows than that the size choice stays,
 * even when the chosen size shows them all on one page. A second pager on the page passes its own
 * `id` for the size choice. */
export function Pager({
  page,
  size,
  total,
  onPage,
  onSize,
  id = 'pager-size',
}: {
  page: number
  size: PageSize
  total: number
  onPage: (page: number) => void
  onSize: (size: PageSize) => void
  id?: string
}) {
  if (total <= pageSizes[0]) return null
  const pages = Math.max(1, Math.ceil(total / size))
  const at = Math.min(Math.max(1, page), pages)
  const from = (at - 1) * size + 1
  const to = Math.min(at * size, total)

  return (
    <div className="flex flex-wrap items-center justify-between gap-3 border-t border-line bg-surface px-4 py-3 sm:px-6">
      <p className="text-sm text-muted">
        Showing <span className="font-medium text-ink">{from}</span> to{' '}
        <span className="font-medium text-ink">{to}</span> of <span className="font-medium text-ink">{total}</span>
      </p>
      <div className="flex flex-wrap items-center gap-x-4 gap-y-3">
        <div className="flex items-center gap-x-2">
          <label htmlFor={id} className="text-sm whitespace-nowrap text-muted">
            Per page
          </label>
          <select
            id={id}
            value={size}
            onChange={(e) => onSize(Number(e.target.value) as PageSize)}
            className="rounded-md bg-surface py-1.5 pr-8 pl-3 text-sm text-ink outline-1 -outline-offset-1 outline-ink/20 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-brand"
          >
            {pageSizes.map((s) => (
              <option key={s} value={s}>
                {s}
              </option>
            ))}
          </select>
        </div>
        {pages > 1 && (
          <>
            <div className="flex gap-x-3 sm:hidden">
              <button type="button" className={mobileButton} disabled={at === 1} onClick={() => onPage(at - 1)}>
                Previous
              </button>
              <button type="button" className={mobileButton} disabled={at === pages} onClick={() => onPage(at + 1)}>
                Next
              </button>
            </div>
            <nav aria-label="Pagination" className="isolate hidden -space-x-px rounded-md shadow-xs sm:inline-flex">
              <button
                type="button"
                className={`${arrow} rounded-l-md`}
                disabled={at === 1}
                onClick={() => onPage(at - 1)}
              >
                <span className="sr-only">Previous page</span>
                <ChevronLeftIcon aria-hidden="true" className="size-5" />
              </button>
              {pageItems(at, pages).map((item, i) =>
                item === 'gap' ? (
                  <span key={`gap-${i}`} className={`${cell} text-muted inset-ring inset-ring-line`}>
                    ...
                  </span>
                ) : (
                  <button
                    key={item}
                    type="button"
                    aria-label={`Page ${item}`}
                    aria-current={item === at ? 'page' : undefined}
                    className={item === at ? current : plain}
                    onClick={() => item !== at && onPage(item)}
                  >
                    {item}
                  </button>
                ),
              )}
              <button
                type="button"
                className={`${arrow} rounded-r-md`}
                disabled={at === pages}
                onClick={() => onPage(at + 1)}
              >
                <span className="sr-only">Next page</span>
                <ChevronRightIcon aria-hidden="true" className="size-5" />
              </button>
            </nav>
          </>
        )}
      </div>
    </div>
  )
}
