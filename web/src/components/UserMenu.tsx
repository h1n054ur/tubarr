// The signed-in user at the foot of the sidebar, as in Tailwind Plus
// application-ui/application-shells/sidebar/dark-sidebar, with the menu from
// application-ui/elements/dropdowns/simple opening upwards.
import { useState } from 'react'
import { Menu, MenuButton, MenuItem, MenuItems } from '@headlessui/react'
import { ChevronUpIcon } from '@heroicons/react/20/solid'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useRouter } from '@tanstack/react-router'
import { AUTH_KEY, authApi, type AuthState } from '../lib/auth'
import { PasswordDialog } from './PasswordDialog'

const item = 'block w-full px-4 py-2 text-left text-sm text-ink data-focus:bg-ink/10 data-focus:outline-hidden'

const bypassText: Record<string, string> = {
  local_bypass: 'Signed in by local network',
  api_key: 'Signed in with the API key',
}

export function UserMenu() {
  const queryClient = useQueryClient()
  const router = useRouter()
  const auth = useQuery({ queryKey: AUTH_KEY, queryFn: authApi.state, staleTime: Infinity })
  const [changing, setChanging] = useState(false)
  const logout = useMutation({
    mutationFn: authApi.logout,
    onSuccess: () => {
      queryClient.removeQueries({ predicate: (q) => q.queryKey[0] !== AUTH_KEY[0] })
      queryClient.setQueryData<AuthState>(AUTH_KEY, (s) => s && { ...s, user: null, via: null })
      router.history.replace('/login')
    },
  })
  const state = auth.data
  if (!state) return null
  const user = state.user

  if (!user) {
    return (
      <div className="-mx-6 mt-auto border-t border-line px-6 py-3 text-sm/6 text-muted">
        {bypassText[state.via ?? ''] ?? ''}
      </div>
    )
  }

  const name = user.display_name ?? user.username
  return (
    <div className="-mx-6 mt-auto border-t border-line">
      <Menu as="div" className="relative">
        <MenuButton className="flex w-full items-center gap-x-4 px-6 py-3 text-sm/6 font-semibold text-ink hover:bg-ink/10">
          <span
            aria-hidden="true"
            className="flex size-8 shrink-0 items-center justify-center rounded-full bg-brand text-sm font-semibold text-on-brand"
          >
            {name.charAt(0).toUpperCase()}
          </span>
          <span className="sr-only">Account menu for </span>
          <span className="flex-1 truncate text-left">{name}</span>
          <ChevronUpIcon aria-hidden="true" className="size-5 text-muted" />
        </MenuButton>
        <MenuItems
          transition
          anchor="top start"
          className="z-50 w-56 origin-bottom-left rounded-md bg-surface py-1 shadow-lg ring-1 ring-line transition [--anchor-gap:4px] focus:outline-hidden data-closed:scale-95 data-closed:transform data-closed:opacity-0 data-enter:duration-100 data-enter:ease-out data-leave:duration-75 data-leave:ease-in"
        >
          <MenuItem>
            <button type="button" onClick={() => setChanging(true)} className={item}>
              Change password
            </button>
          </MenuItem>
          <MenuItem>
            <button type="button" onClick={() => logout.mutate()} className={item}>
              Sign out
            </button>
          </MenuItem>
        </MenuItems>
      </Menu>
      {logout.isError && (
        <p role="alert" className="px-6 pb-3 text-sm text-brand-text">
          {logout.error.message}
        </p>
      )}
      <PasswordDialog open={changing} onClose={() => setChanging(false)} />
    </div>
  )
}
