// Search text, page and page size for a list, and the list's filter selects, kept in the URL
// (`?q=&page=&size=` plus one named parameter per filter) so back, refresh and a shared link keep
// them. Defaults stay out of the URL and every other parameter is left alone. A second list on the
// same page gives its parameters a prefix (`?events_page=2`).
import { useNavigate, useSearch } from '@tanstack/react-router'
import { useCallback } from 'react'

export const pageSizes = [25, 50, 100] as const
export type PageSize = (typeof pageSizes)[number]
export const defaultPageSize: PageSize = 25

export interface ListParams {
  q: string
  page: number
  size: PageSize
}

type Search = Record<string, unknown>

/** Reads the list parameters from a parsed search object. The router parses values as JSON, so a
 * query like `?q=452001` arrives as a number and is turned back into text here. */
export function readListParams(search: Search, prefix = ''): ListParams {
  const raw = search[`${prefix}q`]
  const q = raw === undefined || raw === null ? '' : String(raw)
  const page = Number(search[`${prefix}page`])
  const size = Number(search[`${prefix}size`])
  return {
    q,
    page: Number.isInteger(page) && page >= 1 ? page : 1,
    size: (pageSizes as readonly number[]).includes(size) ? (size as PageSize) : defaultPageSize,
  }
}

/** The next search object: `patch` applied over `prev`, defaults removed, other keys kept. */
export function writeListParams(prev: Search, patch: Partial<ListParams>, prefix = ''): Search {
  const next: Search = { ...prev }
  for (const [name, value] of Object.entries(patch)) next[`${prefix}${name}`] = value
  if (!next[`${prefix}q`]) delete next[`${prefix}q`]
  if (next[`${prefix}page`] === 1) delete next[`${prefix}page`]
  if (next[`${prefix}size`] === defaultPageSize) delete next[`${prefix}size`]
  return next
}

/** List parameters from the URL, under `prefix` for a second list on the page. A new search or page
 * size goes back to page 1. Typing replaces the history entry so back skips the keystrokes; a page or
 * size change adds one. */
export function useListParams(prefix = '') {
  const params = readListParams(useSearch({ strict: false }) as Search, prefix)
  const navigate = useNavigate()

  const update = useCallback(
    (patch: Partial<ListParams>, replace: boolean) =>
      navigate({ to: '.', search: (prev: Search) => writeListParams(prev, patch, prefix), replace }),
    [navigate, prefix],
  )
  const setQ = useCallback((q: string) => update({ q, page: 1 }, true), [update])
  const setPage = useCallback((page: number) => update({ page }, false), [update])
  const setSize = useCallback((size: PageSize) => update({ size, page: 1 }, false), [update])

  return { ...params, setQ, setPage, setSize }
}

/** One filter select kept in the URL: the values it allows (or a check for open sets such as ids)
 * and the value it has when the parameter is missing or not allowed. */
export interface FilterParam<V extends string = string> {
  values: readonly V[] | ((value: string) => boolean)
  default: V
  /** Whether a change goes back to page 1. Off for a filter that does not narrow the paged list; the
   * name of another page parameter (`events_page`) for a filter that narrows a second list. */
  resetsPage?: boolean | string
}

export type FilterSpec = Record<string, FilterParam>

/** A filter limited to `values`, with `''` (All) as its default unless another is given. */
export function oneOf<const V extends string, D extends string = ''>(
  values: readonly V[],
  fallback?: D,
  options: { resetsPage?: boolean | string } = {},
): FilterParam<V | D> {
  return { values: values as readonly (V | D)[], default: (fallback ?? '') as D, ...options }
}
export type FilterValues<S extends FilterSpec> = { [K in keyof S]: S[K]['default'] }

function allowed(param: FilterParam, value: string) {
  return typeof param.values === 'function' ? param.values(value) : param.values.includes(value)
}

/** Reads each filter from a parsed search object, falling back to its default when missing or not
 * allowed. Values the router parsed as numbers are read as text again. */
export function readFilters<S extends FilterSpec>(search: Search, spec: S): FilterValues<S> {
  const out: Record<string, string> = {}
  for (const [name, param] of Object.entries(spec)) {
    const raw = search[name]
    const value = raw === undefined || raw === null ? '' : String(raw)
    out[name] = value !== '' && allowed(param, value) ? value : param.default
  }
  return out as FilterValues<S>
}

/** The next search object with `patch` applied: defaults removed, a whole number written as a number
 * so the router does not quote it, page 1 again when a changed filter narrows the list. */
export function writeFilters<S extends FilterSpec>(prev: Search, spec: S, patch: Partial<FilterValues<S>>): Search {
  const next = { ...prev }
  for (const [name, value] of Object.entries(patch) as [string, string | undefined][]) {
    const param = spec[name]
    if (value === undefined || !param) continue
    if (value === param.default || value === '' || !allowed(param, value)) delete next[name]
    else next[name] = /^\d+$/.test(value) ? Number(value) : value
    if (typeof param.resetsPage === 'string') delete next[param.resetsPage]
    else if (param.resetsPage !== false) delete next.page
  }
  return next
}

/** The filters in `spec` from the URL, and a setter per filter name. A change replaces the history
 * entry, like typing a search. */
export function useFilters<S extends FilterSpec>(spec: S) {
  const values = readFilters(useSearch({ strict: false }) as Search, spec)
  const navigate = useNavigate()
  const set = useCallback(
    <K extends keyof S & string>(name: K, value: S[K]['default']) =>
      navigate({
        to: '.',
        search: (prev: Search) => writeFilters(prev, spec, { [name]: value } as Partial<FilterValues<S>>),
        replace: true,
      }),
    [navigate, spec],
  )
  return { ...values, set }
}
