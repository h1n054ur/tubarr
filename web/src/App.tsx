// Asks /auth/state first: setup when no user exists, the login page when signed out, else the app.
// The login page lives at /login?next=<path>, and signing in goes back to that path.
import { useLayoutEffect, useState, useSyncExternalStore } from 'react'
import { useQuery } from '@tanstack/react-query'
import { RouterProvider, type RouterHistory } from '@tanstack/react-router'
import { Alert } from './components/Alert'
import { AUTH_KEY, authApi, isSignedIn, safeNext } from './lib/auth'
import { makeRouter } from './router'
import { LoginPage } from './routes/LoginPage'
import { SetupPage } from './routes/SetupPage'

function useLocation(history: RouterHistory) {
  return useSyncExternalStore(
    (l) => history.subscribe(l),
    () => history.location,
  )
}

function SignedIn({ history }: { history: RouterHistory }) {
  const [router] = useState(() => makeRouter(history))
  return <RouterProvider router={router} />
}

export function App({ history }: { history: RouterHistory }) {
  const auth = useQuery({ queryKey: AUTH_KEY, queryFn: authApi.state, staleTime: Infinity })
  const location = useLocation(history)
  const params = new URLSearchParams(location.search)
  const onLogin = location.pathname === '/login'
  const signedIn = auth.data ? isSignedIn(auth.data) : null
  const showLogin = auth.data && !auth.data.setup_required && signedIn === false

  // Keep the URL in step with what shows: /login?next=... while signed out, `next` once signed in.
  useLayoutEffect(() => {
    if (showLogin && !onLogin) {
      const next = location.pathname + location.search
      history.replace(next === '/' ? '/login' : `/login?next=${encodeURIComponent(next)}`)
    } else if (signedIn && onLogin) {
      history.replace(safeNext(params.get('next')))
    }
  })

  if (auth.isPending) return null
  if (auth.isError) {
    return (
      <div className="mx-auto max-w-md px-6 py-24">
        <Alert title="Tubarr did not answer">{auth.error.message}</Alert>
      </div>
    )
  }
  if (auth.data.setup_required) return <SetupPage />
  if (showLogin) {
    return onLogin ? (
      <LoginPage state={auth.data} next={safeNext(params.get('next'))} error={params.get('error')} />
    ) : null
  }
  return onLogin ? null : <SignedIn history={history} />
}
