// The app shell, ported from Tailwind Plus application-ui/application-shells/sidebar/dark-sidebar.
// On large screens the sidebar column stretches with the page (so its background reaches the bottom of
// long pages) while its contents stay in view with sticky positioning.
import { useState, type ComponentType, type SVGProps } from 'react'
import { Dialog, DialogBackdrop, DialogPanel, TransitionChild } from '@headlessui/react'
import {
  Bars3Icon,
  BoltIcon,
  Cog6ToothIcon,
  DocumentTextIcon,
  FilmIcon,
  HeartIcon,
  QuestionMarkCircleIcon,
  XMarkIcon,
} from '@heroicons/react/24/outline'
import { Link, Outlet, useRouterState } from '@tanstack/react-router'
import { Logo } from './Logo'
import { UserMenu } from './UserMenu'

type Icon = ComponentType<SVGProps<SVGSVGElement>>

export const navigation: { name: string; to: string; icon: Icon }[] = [
  { name: 'Shows and films', to: '/', icon: FilmIcon },
  { name: 'Unmatched', to: '/unmatched', icon: QuestionMarkCircleIcon },
  { name: 'Drafts', to: '/drafts', icon: DocumentTextIcon },
  { name: 'Activity', to: '/activity', icon: BoltIcon },
  { name: 'Health', to: '/health', icon: HeartIcon },
  { name: 'Settings', to: '/settings', icon: Cog6ToothIcon },
]

function Nav({ onNavigate }: { onNavigate?: () => void }) {
  return (
    <nav className="flex flex-1 flex-col">
      <ul role="list" className="-mx-2 space-y-1">
        {navigation.map((item) => (
          <li key={item.name}>
            <Link
              to={item.to}
              onClick={onNavigate}
              activeOptions={{ exact: true }}
              activeProps={{ className: 'bg-ink/10 text-ink' }}
              inactiveProps={{ className: 'text-muted hover:bg-ink/10 hover:text-ink' }}
              className="group flex gap-x-3 rounded-md p-2 text-sm/6 font-semibold"
            >
              <item.icon aria-hidden="true" className="size-6 shrink-0" />
              {item.name}
            </Link>
          </li>
        ))}
      </ul>
    </nav>
  )
}

function Brand() {
  return (
    <div className="flex h-16 shrink-0 items-center">
      <Logo className="h-8 w-auto" />
    </div>
  )
}

export function Layout() {
  const [sidebarOpen, setSidebarOpen] = useState(false)
  const path = useRouterState({ select: (s) => s.location.pathname })
  const current = navigation.find((n) => n.to === path)?.name ?? 'Tubarr'

  return (
    <div className="min-h-screen lg:flex">
      <Dialog open={sidebarOpen} onClose={setSidebarOpen} className="relative z-50 lg:hidden">
        <DialogBackdrop
          transition
          className="fixed inset-0 bg-canvas/80 transition-opacity duration-300 ease-linear data-closed:opacity-0"
        />
        <div className="fixed inset-0 flex">
          <DialogPanel
            transition
            className="relative mr-16 flex w-full max-w-xs flex-1 transform transition duration-300 ease-in-out data-closed:-translate-x-full"
          >
            <TransitionChild>
              <div className="absolute top-0 left-full flex w-16 justify-center pt-5 duration-300 ease-in-out data-closed:opacity-0">
                <button type="button" onClick={() => setSidebarOpen(false)} className="-m-2.5 p-2.5">
                  <span className="sr-only">Close sidebar</span>
                  <XMarkIcon aria-hidden="true" className="size-6 text-ink" />
                </button>
              </div>
            </TransitionChild>
            <div className="flex grow flex-col gap-y-5 overflow-y-auto bg-surface px-6 pb-2 ring-1 ring-line">
              <Brand />
              <Nav onNavigate={() => setSidebarOpen(false)} />
              <UserMenu />
            </div>
          </DialogPanel>
        </div>
      </Dialog>

      <div id="sidebar" className="hidden border-r border-line bg-surface lg:flex lg:w-72 lg:shrink-0 lg:flex-col">
        <div className="sticky top-0 flex h-screen flex-col gap-y-5 overflow-y-auto px-6">
          <Brand />
          <Nav />
          <UserMenu />
        </div>
      </div>

      <div className="sticky top-0 z-40 flex items-center gap-x-6 bg-surface px-4 py-4 shadow-xs sm:px-6 lg:hidden">
        <button type="button" onClick={() => setSidebarOpen(true)} className="-m-2.5 p-2.5 text-muted lg:hidden">
          <span className="sr-only">Open sidebar</span>
          <Bars3Icon aria-hidden="true" className="size-6" />
        </button>
        <div className="flex-1 text-sm/6 font-semibold text-ink">{current}</div>
      </div>

      <main className="py-10 lg:min-w-0 lg:flex-1">
        <div className="px-4 sm:px-6 lg:px-8">
          <Outlet />
        </div>
      </main>
    </div>
  )
}
