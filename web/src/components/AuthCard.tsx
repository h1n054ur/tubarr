// The frame of the setup and login pages, ported from Tailwind Plus
// application-ui/forms/sign-in-forms/simple-card with the Tubarr logo and tokens.
import type { ReactNode } from 'react'
import { Logo } from './Logo'

export function AuthCard({ title, text, children }: { title: string; text?: string; children: ReactNode }) {
  return (
    <div className="flex min-h-full flex-col justify-center py-12 sm:px-6 lg:px-8">
      <div className="sm:mx-auto sm:w-full sm:max-w-md">
        <Logo className="mx-auto h-10 w-auto" />
        <h2 className="mt-6 text-center text-2xl/9 font-bold tracking-tight text-ink">{title}</h2>
        {text && <p className="mt-2 text-center text-sm/6 text-muted">{text}</p>}
      </div>
      <div className="mt-10 sm:mx-auto sm:w-full sm:max-w-[480px]">
        <div className="bg-surface px-6 py-12 shadow-sm ring-1 ring-line sm:rounded-lg sm:px-12">{children}</div>
      </div>
    </div>
  )
}

// The full-width button of the same block.
export const wideButton =
  'flex w-full justify-center rounded-md bg-brand px-3 py-1.5 text-sm/6 font-semibold text-on-brand shadow-xs hover:bg-brand-dark focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand disabled:opacity-50'
