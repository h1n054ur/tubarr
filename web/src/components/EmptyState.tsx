// Ported from Tailwind Plus application-ui/feedback/empty-states/simple. The icon can be swapped, e.g. a
// check circle where an empty list is good news, and an action (a button) can go under the text.
import type { ComponentType, ReactNode, SVGProps } from 'react'

function FolderPlusIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <svg fill="none" stroke="currentColor" viewBox="0 0 24 24" {...props}>
      <path
        d="M9 13h6m-3-3v6m-9 1V7a2 2 0 012-2h6l2 2h6a2 2 0 012 2v8a2 2 0 01-2 2H5a2 2 0 01-2-2z"
        strokeWidth={2}
        vectorEffect="non-scaling-stroke"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  )
}

export function EmptyState({
  title,
  text,
  icon: Icon = FolderPlusIcon,
  action,
}: {
  title: string
  text: string
  icon?: ComponentType<SVGProps<SVGSVGElement>>
  action?: ReactNode
}) {
  return (
    <div className="rounded-lg border-2 border-dashed border-line px-6 py-12 text-center">
      <Icon aria-hidden="true" className="mx-auto size-12 text-muted" />
      <h3 className="mt-2 text-sm font-semibold text-ink">{title}</h3>
      <p className="mt-1 text-sm text-muted">{text}</p>
      {action && <div className="mt-6">{action}</div>}
    </div>
  )
}
