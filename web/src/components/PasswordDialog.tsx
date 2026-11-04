// Change your own password, ported from Tailwind Plus
// application-ui/overlays/modal-dialogs/simple-with-gray-footer.
import { useState, type FormEvent } from 'react'
import { Dialog, DialogBackdrop, DialogPanel, DialogTitle } from '@headlessui/react'
import { useMutation } from '@tanstack/react-query'
import { ApiError } from '../lib/api'
import { authApi } from '../lib/auth'
import { Field, inputClass, primaryButton, secondaryButton } from './fields'
import { PasswordHints, passwordProblems } from './PasswordHints'

function errorText(error: unknown): string {
  if (error instanceof ApiError && error.status === 401) return 'The current password is wrong.'
  if (error instanceof ApiError && error.status === 429)
    return `Too many wrong passwords. Try again in ${error.retryAfter ?? 60} seconds.`
  return error instanceof Error ? error.message : 'Could not change the password.'
}

function PasswordForm({ onClose }: { onClose: () => void }) {
  const [current, setCurrent] = useState('')
  const [password, setPassword] = useState('')
  const [confirm, setConfirm] = useState('')
  const change = useMutation({ mutationFn: () => authApi.changePassword(current, password) })
  const problems = passwordProblems(password, confirm)

  function submit(e: FormEvent) {
    e.preventDefault()
    if (problems.length === 0) change.mutate()
  }

  return (
    <form onSubmit={submit}>
      <div className="px-4 pt-5 pb-4 sm:p-6 sm:pb-4">
        <DialogTitle as="h3" className="text-base font-semibold text-ink">
          Change password
        </DialogTitle>
        {change.isSuccess ? (
          <p role="status" className="mt-2 text-sm text-muted">
            Password changed. Your other sessions are signed out.
          </p>
        ) : (
          <div className="mt-4 space-y-4">
            <Field id="pw-current" label="Current password">
              <input
                id="pw-current"
                type="password"
                required
                autoComplete="current-password"
                value={current}
                onChange={(e) => setCurrent(e.target.value)}
                className={inputClass}
              />
            </Field>
            <Field id="pw-new" label="New password">
              <input
                id="pw-new"
                type="password"
                required
                autoComplete="new-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                className={inputClass}
              />
            </Field>
            <Field id="pw-confirm" label="Confirm new password">
              <input
                id="pw-confirm"
                type="password"
                required
                autoComplete="new-password"
                value={confirm}
                onChange={(e) => setConfirm(e.target.value)}
                className={inputClass}
              />
            </Field>
            <PasswordHints password={password} confirm={confirm} />
            {change.isError && (
              <p role="alert" className="text-sm text-brand-text">
                {errorText(change.error)}
              </p>
            )}
          </div>
        )}
      </div>
      <div className="gap-x-3 bg-surface-2 px-4 py-3 sm:flex sm:flex-row-reverse sm:px-6">
        {!change.isSuccess && (
          <button type="submit" disabled={change.isPending || problems.length > 0} className={primaryButton}>
            {change.isPending ? 'Saving...' : 'Change password'}
          </button>
        )}
        <button type="button" onClick={onClose} className={`mt-3 sm:mt-0 ${secondaryButton}`}>
          {change.isSuccess ? 'Close' : 'Cancel'}
        </button>
      </div>
    </form>
  )
}

export function PasswordDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  return (
    <Dialog open={open} onClose={onClose} className="relative z-50">
      <DialogBackdrop transition className="fixed inset-0 bg-canvas/75 transition-opacity data-closed:opacity-0" />
      <div className="fixed inset-0 z-10 w-screen overflow-y-auto">
        <div className="flex min-h-full items-end justify-center p-4 text-center sm:items-center sm:p-0">
          <DialogPanel
            transition
            className="relative transform overflow-hidden rounded-lg bg-surface text-left shadow-xl ring-1 ring-line transition-all data-closed:translate-y-4 data-closed:opacity-0 sm:my-8 sm:w-full sm:max-w-lg"
          >
            {open && <PasswordForm onClose={onClose} />}
          </DialogPanel>
        </div>
      </div>
    </Dialog>
  )
}
