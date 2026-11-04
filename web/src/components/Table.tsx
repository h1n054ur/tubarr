// Ported from Tailwind Plus application-ui/lists/tables/simple-in-card (with-border for the frame),
// split into parts so each page lists its own columns. The card holds the header row and every row,
// and scrolls sideways inside its own border on narrow screens. A footer (a pager) sits inside the card
// under the table and stays put while the table scrolls.
//
// Phones: the stacked-columns pattern from Tailwind Plus application-ui/lists/tables/
// with-stacked-columns-on-mobile. A secondary column passes `hideBelow` to its Th and every Td so it
// only shows from that breakpoint up; the first Td passes `stackBelow` (narrowest breakpoint any
// column hides at) so it takes the spare width and wraps, and repeats the hidden values in a
// <Stacked below=...> list that shows only under the breakpoint. Keep the action column without
// `hideBelow` so Edit stays on screen. From the largest breakpoint used up, the table is unchanged.
import type { ReactNode } from 'react'

export type Breakpoint = 'sm' | 'md' | 'lg' | 'xl'

// Literal class names so Tailwind picks them up.
const showFrom: Record<Breakpoint, string> = {
  sm: 'hidden sm:table-cell',
  md: 'hidden md:table-cell',
  lg: 'hidden lg:table-cell',
  xl: 'hidden xl:table-cell',
}
const hideFrom: Record<Breakpoint, string> = { sm: 'sm:hidden', md: 'md:hidden', lg: 'lg:hidden', xl: 'xl:hidden' }
const stackCell: Record<Breakpoint, string> = {
  sm: 'w-full max-w-0 sm:w-auto sm:max-w-none',
  md: 'w-full max-w-0 md:w-auto md:max-w-none',
  lg: 'w-full max-w-0 lg:w-auto lg:max-w-none',
  xl: 'w-full max-w-0 xl:w-auto xl:max-w-none',
}
const stackNowrap: Record<Breakpoint, string> = {
  sm: 'sm:whitespace-nowrap',
  md: 'md:whitespace-nowrap',
  lg: 'lg:whitespace-nowrap',
  xl: 'xl:whitespace-nowrap',
}

export function Table({ head, children, footer }: { head: ReactNode; children: ReactNode; footer?: ReactNode }) {
  const table = (
    <table className="relative min-w-full divide-y divide-line">
      <thead className="bg-surface-2">
        <tr>{head}</tr>
      </thead>
      <tbody className="divide-y divide-line">{children}</tbody>
    </table>
  )
  if (!footer) {
    return <div className="mt-8 overflow-x-auto rounded-lg border border-line bg-surface shadow-sm">{table}</div>
  }
  return (
    <div className="mt-8 overflow-hidden rounded-lg border border-line bg-surface shadow-sm">
      <div className="overflow-x-auto">{table}</div>
      {footer}
    </div>
  )
}

// First and last cells carry the card's inner padding so text never touches its border.
const firstPad = 'pr-3 pl-4 sm:pl-6'
const lastPad = 'pr-4 pl-3 sm:pr-6'
const pad = (first?: boolean, last?: boolean) => (first ? firstPad : last ? lastPad : 'px-3')

export function Th({
  children,
  first,
  last,
  hideBelow,
}: {
  children?: ReactNode
  first?: boolean
  last?: boolean
  hideBelow?: Breakpoint
}) {
  const vis = hideBelow ? ` ${showFrom[hideBelow]}` : ''
  return (
    <th scope="col" className={`${pad(first, last)} py-3.5 text-left text-sm font-semibold text-ink${vis}`}>
      {children}
    </th>
  )
}

export function Td({
  children,
  first,
  last,
  wrap,
  hideBelow,
  stackBelow,
}: {
  children?: ReactNode
  first?: boolean
  last?: boolean
  wrap?: boolean
  hideBelow?: Breakpoint
  stackBelow?: Breakpoint
}) {
  const tone = first ? '' : 'text-muted'
  const space = wrap ? '' : stackBelow ? stackNowrap[stackBelow] : 'whitespace-nowrap'
  const extra = [hideBelow && showFrom[hideBelow], stackBelow && stackCell[stackBelow]].filter(Boolean).join(' ')
  return (
    <td className={`${pad(first, last)} py-4 text-sm ${tone} ${space}${extra ? ` ${extra}` : ''}`}>{children}</td>
  )
}

/** The hidden columns repeated inside the first cell, on one wrapping line, only under `below`. An item
 * whose column shows from a narrower breakpoint passes its own `below` so it is not shown twice. Each
 * item keeps its column name for screen readers. */
export function Stacked({
  below,
  items,
}: {
  below: Breakpoint
  items: { label: string; value: ReactNode; below?: Breakpoint }[]
}) {
  return (
    <dl className={`mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 font-normal text-muted ${hideFrom[below]}`}>
      {items.map((item) => (
        <div key={item.label} className={`flex items-center${item.below ? ` ${hideFrom[item.below]}` : ''}`}>
          <dt className="sr-only">{item.label}</dt>
          <dd>{item.value}</dd>
        </div>
      ))}
    </dl>
  )
}
