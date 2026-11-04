// Ported from Tailwind Plus application-ui/elements/badges/with-border-on-dark.
import type { ReactNode } from 'react'

const tones = {
  green: 'bg-green-500/10 text-green-400 ring-green-500/20',
  yellow: 'bg-yellow-400/10 text-yellow-500 ring-yellow-400/20',
  red: 'bg-red-400/10 text-red-400 ring-red-400/20',
  gray: 'bg-gray-400/10 text-muted ring-gray-400/20',
  cyan: 'bg-cyan-400/10 text-accent ring-cyan-400/30',
}

export type Tone = keyof typeof tones

export function Badge({ tone, children }: { tone: Tone; children: ReactNode }) {
  return (
    <span
      className={`inline-flex items-center rounded-md px-2 py-1 text-xs font-medium ring-1 ring-inset ${tones[tone]}`}
    >
      {children}
    </span>
  )
}
