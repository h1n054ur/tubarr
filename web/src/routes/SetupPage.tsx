// First run: no user exists yet. Whoever has the setup code from Tubarr's log creates the admin
// account, so an exposed instance cannot be claimed by the first visitor.
import { useState, type FormEvent } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Alert } from '../components/Alert'
import { AuthCard, wideButton } from '../components/AuthCard'
import { Field, inputClass } from '../components/fields'
import { PasswordHints, passwordProblems } from '../components/PasswordHints'
import { AUTH_KEY, authApi, type AuthState } from '../lib/auth'

export function SetupPage() {
  const queryClient = useQueryClient()
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [confirm, setConfirm] = useState('')
  const [code, setCode] = useState('')
  const setup = useMutation({
    mutationFn: () => authApi.setup(username.trim(), password, code.trim()),
    onSuccess: ({ user }) =>
      queryClient.setQueryData<AuthState>(AUTH_KEY, (s) => s && { ...s, setup_required: false, user, via: 'session' }),
  })
  const problems = passwordProblems(password, confirm)

  function submit(e: FormEvent) {
    e.preventDefault()
    if (problems.length === 0 && username.trim() && code.trim()) setup.mutate()
  }

  return (
    <AuthCard title="Create the admin account" text="Tubarr has no users yet. This account can add the others later.">
      <form onSubmit={submit} className="space-y-6">
        <Field id="setup-code" label="Setup code" help="Find it in Tubarr's log">
          <input
            id="setup-code"
            required
            autoComplete="off"
            spellCheck={false}
            aria-describedby="setup-code-help"
            value={code}
            onChange={(e) => setCode(e.target.value)}
            className={`${inputClass} font-mono`}
          />
        </Field>
        <Field id="setup-username" label="Username">
          <input
            id="setup-username"
            required
            autoComplete="username"
            maxLength={64}
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            className={inputClass}
          />
        </Field>
        <Field id="setup-password" label="Password">
          <input
            id="setup-password"
            type="password"
            required
            autoComplete="new-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            className={inputClass}
          />
        </Field>
        <Field id="setup-confirm" label="Confirm password">
          <input
            id="setup-confirm"
            type="password"
            required
            autoComplete="new-password"
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
            className={inputClass}
          />
        </Field>
        <PasswordHints password={password} confirm={confirm} />
        {setup.isError && <Alert title="Could not create the account">{setup.error.message}</Alert>}
        <button
          type="submit"
          disabled={setup.isPending || problems.length > 0 || !username.trim() || !code.trim()}
          className={wideButton}
        >
          {setup.isPending ? 'Creating...' : 'Create account and sign in'}
        </button>
      </form>
    </AuthCard>
  )
}
