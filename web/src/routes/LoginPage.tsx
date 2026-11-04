// The login page: a username and password form, an SSO button, or both, as the server offers.
import { useState, type FormEvent } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Alert } from '../components/Alert'
import { AuthCard, wideButton } from '../components/AuthCard'
import { Field, inputClass } from '../components/fields'
import { AUTH_KEY, authApi, loginErrorMessage, ssoErrors, type AuthState } from '../lib/auth'

// The secondary social button from the same Tailwind Plus sign-in block.
const ssoButton =
  'flex w-full items-center justify-center gap-3 rounded-md bg-ink/10 px-3 py-2 text-sm/6 font-semibold text-ink shadow-xs ring-1 ring-line ring-inset hover:bg-ink/20'

export function LoginPage({ state, next, error }: { state: AuthState; next: string; error: string | null }) {
  const queryClient = useQueryClient()
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const login = useMutation({
    mutationFn: () => authApi.login(username.trim(), password),
    onSuccess: ({ user }) => queryClient.setQueryData<AuthState>(AUTH_KEY, (s) => s && { ...s, user, via: 'session' }),
    onError: () => setPassword(''),
  })
  const { password: forms, oidc } = state.methods
  const ssoMessage = error ? (ssoErrors[error] ?? ssoErrors.failed) : null

  function submit(e: FormEvent) {
    e.preventDefault()
    login.mutate()
  }

  return (
    <AuthCard title="Sign in to Tubarr">
      <div className="space-y-6">
        {ssoMessage && !login.isError && <Alert title="SSO sign-in failed">{ssoMessage}</Alert>}
        {login.isError && <Alert title="Sign-in failed">{loginErrorMessage(login.error)}</Alert>}
        {!forms && !oidc && <Alert title="No sign-in method is on">Set [auth] method in the Tubarr config.</Alert>}

        {forms && (
          <form onSubmit={submit} className="space-y-6">
            <Field id="login-username" label="Username">
              <input
                id="login-username"
                required
                autoComplete="username"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                className={inputClass}
              />
            </Field>
            <Field id="login-password" label="Password">
              <input
                id="login-password"
                type="password"
                required
                autoComplete="current-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                className={inputClass}
              />
            </Field>
            <button type="submit" disabled={login.isPending} className={wideButton}>
              {login.isPending ? 'Signing in...' : 'Sign in'}
            </button>
          </form>
        )}

        {forms && oidc && (
          <div className="flex items-center gap-x-6">
            <div className="w-full flex-1 border-t border-line" />
            <p className="text-sm/6 font-medium text-nowrap text-muted">Or</p>
            <div className="w-full flex-1 border-t border-line" />
          </div>
        )}

        {oidc && (
          <a href={`/auth/oidc/start?next=${encodeURIComponent(next)}`} className={ssoButton}>
            {state.oidc_button_label ?? 'Sign in with SSO'}
          </a>
        )}
      </div>
    </AuthCard>
  )
}
