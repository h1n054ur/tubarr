// Simple strength hints under a new password: the two the server enforces, and one nudge.
import { CheckCircleIcon, MinusCircleIcon } from '@heroicons/react/20/solid'
import { MAX_PASSWORD, MIN_PASSWORD } from '../lib/auth'

export function passwordProblems(password: string, confirm: string): string[] {
  const problems: string[] = []
  const length = [...password].length
  if (length < MIN_PASSWORD) problems.push(`The password needs at least ${MIN_PASSWORD} characters.`)
  if (length > MAX_PASSWORD) problems.push(`The password can have at most ${MAX_PASSWORD} characters.`)
  if (password !== confirm) problems.push('The two passwords do not match.')
  return problems
}

function Hint({ met, children }: { met: boolean; children: string }) {
  const Icon = met ? CheckCircleIcon : MinusCircleIcon
  return (
    <li className={`flex items-center gap-x-2 ${met ? 'text-ink' : 'text-muted'}`}>
      <Icon aria-hidden="true" className={`size-4 shrink-0 ${met ? 'text-accent' : 'text-muted'}`} />
      <span>
        {children}
        <span className="sr-only">{met ? ' (done)' : ' (not yet)'}</span>
      </span>
    </li>
  )
}

export function PasswordHints({ password, confirm }: { password: string; confirm: string }) {
  const length = [...password].length
  return (
    <ul aria-label="Password hints" className="space-y-1 text-sm">
      <Hint met={length >= MIN_PASSWORD}>{`At least ${MIN_PASSWORD} characters`}</Hint>
      <Hint met={password !== '' && password === confirm}>Both passwords match</Hint>
      <Hint met={length >= 16}>Stronger: 16 or more characters, a few words work well</Hint>
    </ul>
  )
}
