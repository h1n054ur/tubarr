// Ported from Tailwind Plus application-ui/data-display/description-lists/left-aligned-in-card, framed
// like Table (border, surface-2 header) so lists and tables read as the same kind of card. The heading,
// its note and any action sit in the card's header; every row carries the card's inner padding.
// Long values (paths, URLs, ids) wrap anywhere so they never push the card wider than a phone.
import type { ReactNode } from 'react'

export interface Item {
  term: string
  detail: ReactNode
}

export function DescriptionList({
  title,
  text,
  items,
  action,
  level: Heading = 'h2',
}: {
  title: string
  text?: string
  items: Item[]
  action?: ReactNode
  level?: 'h2' | 'h3'
}) {
  return (
    <section
      className={`${Heading === 'h2' ? 'mt-10' : 'mt-8'} overflow-hidden rounded-lg border border-line bg-surface shadow-sm`}
    >
      <div className="flex items-start justify-between gap-x-4 bg-surface-2 px-4 py-5 sm:px-6">
        <div>
          <Heading className={Heading === 'h2' ? 'text-base/7 font-semibold text-ink' : 'text-sm/6 font-semibold text-ink'}>
            {title}
          </Heading>
          {text && <p className="mt-1 max-w-2xl text-sm/6 text-muted">{text}</p>}
        </div>
        {action}
      </div>
      <div className="border-t border-line">
        <dl className="divide-y divide-line">
          {items.map((item) => (
            <div key={item.term} className="px-4 py-6 sm:grid sm:grid-cols-3 sm:gap-4 sm:px-6">
              <dt className="text-sm/6 font-medium text-ink">{item.term}</dt>
              <dd className="mt-1 min-w-0 text-sm/6 wrap-anywhere whitespace-pre-line text-muted sm:col-span-2 sm:mt-0">{item.detail}</dd>
            </div>
          ))}
        </dl>
      </div>
    </section>
  )
}
