// Forgiving search for the admin lists, built on Fuse.js. Both the query and every searched value go
// through `normalise`, so case, accents, punctuation and a leading article never stop a match.
import Fuse, { type IFuseOptions } from 'fuse.js'

type Value = string | number | null | undefined

export interface FuzzyKey<T> {
  /** A stable name for the key; Fuse uses it to report matches. */
  name: string
  /** Reads the value or values to search from an item. */
  get: (item: T) => Value | readonly Value[]
  /** Relative importance, 1 when left out. A title at 3 outranks an id at 1. */
  weight?: number
  /** An id: never fuzzy, it matches only an all-digit query equal to it, and such a hit ranks first.
   * So "452001" finds that show and not 452002. */
  exact?: boolean
}

/** Lowercase, strip Latin accents, drop punctuation and a leading "the", "a" or "an". Letters in
 * other scripts (Urdu, Hindi) keep their marks, which carry the vowels there. */
export function normalise(text: string): string {
  return text
    .normalize('NFD')
    .replace(/[̀-ͯ]/g, '')
    .toLowerCase()
    .replace(/['’`]/g, '')
    .replace(/[^\p{L}\p{M}\p{N}]+/gu, ' ')
    .trim()
    .replace(/^(the|a|an) (?=\S)/, '')
}

// ignoreLocation: a name can sit anywhere in a long title ("This Past Weekend w/Theo Von").
// threshold 0.34: one typo in a five letter word still matches ("rogn"), two do not, so short
// queries don't pull in every title sharing a few letters.
const options = {
  ignoreLocation: true,
  threshold: 0.34,
  minMatchCharLength: 2,
  includeScore: true,
  shouldSort: true,
} satisfies IFuseOptions<unknown>

/** The items matching `query`, best match first, with exact id hits (see `FuzzyKey.exact`) ahead of the rest. An empty query returns the items as they are. A
 * query of several words also matches when every word matches somewhere, in any order and in any
 * key ("rogan joe", "theri tamil"); an item then ranks by the mean of its word scores, or by its
 * phrase score when that is better. */
export function fuzzySearch<T>(items: readonly T[], query: string, keys: readonly FuzzyKey<T>[]): T[] {
  const q = normalise(query)
  if (!q) return [...items]
  const values = (k: FuzzyKey<T>, item: T) => {
    const raw = k.get(item)
    const list = Array.isArray(raw) ? raw : [raw]
    return list.filter((v): v is string | number => v !== null && v !== undefined).map((v) => normalise(String(v)))
  }
  const idKeys = keys.filter((k) => k.exact)
  const ids = (pattern: string) =>
    /^\d+$/.test(pattern)
      ? items.flatMap((item, index) => (idKeys.some((k) => values(k, item).includes(pattern)) ? [index] : []))
      : []
  const fuse = new Fuse(items, {
    ...options,
    keys: keys
      .filter((k) => !k.exact)
      .map((k) => ({ name: k.name, weight: k.weight ?? 1, getFn: (item: T) => values(k, item) })),
  })
  const fuzzy = (pattern: string) => new Map(fuse.search(pattern).map((r) => [r.refIndex, r.score ?? 0]))
  // A word of a longer query that is an exact id counts as a perfect match for that word.
  const scores = (pattern: string) => {
    const found = fuzzy(pattern)
    for (const index of ids(pattern)) found.set(index, 0)
    return found
  }

  const exact = ids(q)
  const best = fuzzy(q)
  // Words below Fuse's minimum match length can never match on their own, so they don't count.
  const words = [...new Set(q.split(' '))].filter((w) => w.length >= options.minMatchCharLength)
  if (words.length > 1) {
    const perWord = words.map(scores)
    for (const index of perWord[0].keys()) {
      if (!perWord.every((m) => m.has(index))) continue
      const mean = perWord.reduce((sum, m) => sum + (m.get(index) as number), 0) / perWord.length
      best.set(index, Math.min(best.get(index) ?? mean, mean))
    }
  }
  const ranked = [...best].sort(([ia, a], [ib, b]) => a - b || ia - ib).map(([index]) => index)
  return [...new Set([...exact, ...ranked])].map((index) => items[index])
}
