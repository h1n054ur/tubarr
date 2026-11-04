// Ported from Tailwind Plus application-ui/feedback/alerts/with-description, recoloured with the
// Tubarr tokens: the brand red for errors, cyan for notices.
import { ExclamationTriangleIcon, InformationCircleIcon } from '@heroicons/react/20/solid'
import type { ReactNode } from 'react'

export function Alert({
  tone = 'error',
  title,
  children,
}: {
  tone?: 'error' | 'info'
  title: string
  children?: ReactNode
}) {
  const Icon = tone === 'error' ? ExclamationTriangleIcon : InformationCircleIcon
  const colour = tone === 'error' ? 'text-brand-text' : 'text-accent'
  const ring = tone === 'error' ? 'bg-brand/10 ring-brand/30' : 'bg-accent/10 ring-accent/30'
  return (
    <div role={tone === 'error' ? 'alert' : 'status'} className={`rounded-md p-4 ring-1 ring-inset ${ring}`}>
      <div className="flex">
        <div className="shrink-0">
          <Icon aria-hidden="true" className={`size-5 ${colour}`} />
        </div>
        <div className="ml-3">
          <h3 className={`text-sm font-medium ${colour}`}>{title}</h3>
          {children && <div className="mt-2 text-sm text-ink">{children}</div>}
        </div>
      </div>
    </div>
  )
}
