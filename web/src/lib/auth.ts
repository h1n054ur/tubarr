// Sign-in state from /auth/state. The session cookie is HttpOnly, so this call is the only way the
// UI learns who is signed in. A 401 from the admin API marks the browser signed out, which brings
// back the login page.
import { MutationCache, QueryCache, QueryClient } from '@tanstack/react-query'
import { ApiError, send, type User } from './api'

export interface AuthState {
  setup_required: boolean
  methods: { password: boolean; oidc: boolean }
  oidc_button_label: string | null
  user: User | null
  via: 'session' | 'api_key' | 'local_bypass' | null
}

/** Matches the server's limits in crates/tubarr/src/auth/password.rs. */
export const MIN_PASSWORD = 8
export const MAX_PASSWORD = 256

export const AUTH_KEY = ['auth', 'state'] as const

export const authApi = {
  state() {
    return send<AuthState>('GET', '/auth/state')
  },
  setup(username: string, password: string, setup_code: string) {
    return send<{ user: User }>('POST', '/auth/setup', { username, password, setup_code })
  },
  login(username: string, password: string) {
    return send<{ user: User }>('POST', '/auth/login', { username, password })
  },
  logout() {
    return send<null>('POST', '/auth/logout')
  },
  changePassword(current_password: string, new_password: string) {
    return send<null>('POST', '/auth/password', { current_password, new_password })
  },
}

export function isSignedIn(state: AuthState): boolean {
  return state.user !== null || state.via !== null
}

/** Only paths on this site, like the server's own check: never `//host` or a full URL. */
export function safeNext(next: string | null | undefined): string {
  if (!next || !next.startsWith('/') || next.startsWith('//') || next.includes('\\')) return '/'
  if (next === '/login' || next.startsWith('/login?')) return '/'
  return next
}

/** The query cache's error hook: a 401 from the admin API means the session is gone. */
export function signOutOn401(client: QueryClient) {
  return (error: unknown) => {
    const admin = error instanceof ApiError && (error.url.startsWith('/admin/api') || error.url === '/api/health')
    if (admin && error.status === 401) {
      client.setQueryData<AuthState>(AUTH_KEY, (s) => s && { ...s, user: null, via: null })
    }
  }
}

/** The app's query client, wired so a 401 from the admin API signs the browser out. */
export function createQueryClient(): QueryClient {
  const client: QueryClient = new QueryClient({
    queryCache: new QueryCache({ onError: (e) => signOutOn401(client)(e) }),
    mutationCache: new MutationCache({ onError: (e) => signOutOn401(client)(e) }),
    defaultOptions: { queries: { retry: false } },
  })
  return client
}

/** What a sign-in failure means for the person at the form. */
export function loginErrorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 401) return 'Wrong username or password.'
    if (error.status === 429) {
      const wait = error.retryAfter
      return wait
        ? `Too many failed sign-ins. Try again in ${wait} ${wait === 1 ? 'second' : 'seconds'}.`
        : 'Too many failed sign-ins. Try again in a minute.'
    }
    return error.message
  }
  return 'Tubarr did not answer. Check that it is running.'
}

/** The `?error=` codes the SSO callback sends people back with. */
export const ssoErrors: Record<string, string> = {
  expired: 'The SSO sign-in took too long or was started in another browser. Please try again.',
  not_allowed: 'Your SSO account is not allowed into Tubarr. Ask an admin to add you.',
  failed: 'The SSO sign-in did not work. Please try again, or check the Tubarr log.',
}
