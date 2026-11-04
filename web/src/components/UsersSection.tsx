// Who may sign in, and how. The table is Tailwind Plus application-ui/lists/tables/simple-on-dark
// (through Table), the summary a description list. On phones the sign-in method, role and last sign-in
// move under the username (see Stacked in ./Table), so Delete never leaves the screen.
import { useState, type FormEvent } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, type AuthSummary, type NewUser, type User } from '../lib/api'
import { MIN_PASSWORD } from '../lib/auth'
import { duration, when } from '../lib/format'
import { Badge } from './Badge'
import { DescriptionList } from './DescriptionList'
import { Field, inputClass, primaryButton } from './fields'
import { Stacked, Table, Td, Th } from './Table'

function methodText(auth: AuthSummary): string {
  if (auth.password && auth.oidc) return 'Username and password, and SSO'
  if (auth.oidc) return 'SSO only'
  return 'Username and password'
}

export function AuthSummaryList({ auth }: { auth: AuthSummary }) {
  const items = [
    { term: 'Sign-in methods', detail: methodText(auth) },
    {
      term: 'Local network bypass',
      detail: auth.local_bypass ? 'on: loopback and private addresses skip sign-in' : 'off',
    },
    {
      term: 'Sessions',
      detail: `end after ${duration(auth.session_ttl_secs)} idle, ${duration(auth.session_max_age_secs)} at most`,
    },
  ]
  if (auth.oidc) {
    items.push(
      { term: 'SSO issuer', detail: auth.oidc_issuer ?? 'not set' },
      {
        term: 'Allowed groups',
        detail:
          auth.allowed_groups && auth.allowed_groups.length > 0
            ? auth.allowed_groups.join(', ')
            : 'anyone the provider signs in',
      },
      {
        term: 'New SSO users',
        detail: auth.auto_create_users ? 'created on first sign-in' : 'must be added here first',
      },
    )
  }
  items.push({
    term: 'Who may play',
    detail: auth.play_networks_default
      ? 'loopback and private addresses (the default)'
      : auth.play_allowed_networks.join(', '),
  })
  if (auth.trusted_proxies.length > 0) {
    items.push({ term: 'Trusted proxies', detail: auth.trusted_proxies.join(', ') })
  }
  return <DescriptionList title="Sign-in" text="Set in [auth] in the config file." items={items} level="h3" />
}

function signInWith(user: User): string {
  if (user.has_password && user.oidc_subject) return 'Password and SSO'
  return user.oidc_subject ? 'SSO' : 'Password'
}

function UserRow({ user, lastAdmin }: { user: User; lastAdmin: boolean }) {
  const queryClient = useQueryClient()
  const [confirming, setConfirming] = useState(false)
  const remove = useMutation({
    mutationFn: () => api.deleteUser(user.id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['users'] }),
  })
  const role = (
    <span className="inline-flex gap-x-2">
      <Badge tone={user.role === 'admin' ? 'cyan' : 'gray'}>{user.role}</Badge>
      {user.disabled && <Badge tone="red">disabled</Badge>}
    </span>
  )
  return (
    <tr>
      <Td first stackBelow="sm">
        <div className="font-medium text-ink wrap-anywhere">{user.username}</div>
        {user.display_name && <div className="mt-1 text-muted wrap-anywhere">{user.display_name}</div>}
        <Stacked
          below="sm"
          items={[
            { label: 'Role', value: role },
            { label: 'Signs in with', value: signInWith(user) },
            { label: 'Last sign-in', value: `Last sign-in ${when(user.last_login_at)}` },
          ]}
        />
      </Td>
      <Td hideBelow="sm">{signInWith(user)}</Td>
      <Td hideBelow="sm">{role}</Td>
      <Td hideBelow="sm">{when(user.last_login_at)}</Td>
      <td className="py-4 pr-4 pl-3 text-right text-sm font-medium whitespace-nowrap sm:pr-6">
        {remove.isError && (
          <span
            role="alert"
            className="mb-1 block max-w-40 whitespace-normal text-brand-text sm:mr-3 sm:mb-0 sm:inline sm:max-w-none sm:whitespace-nowrap"
          >
            {remove.error.message}
          </span>
        )}
        {confirming ? (
          <span className="inline-flex flex-col items-end gap-y-1 sm:flex-row sm:gap-x-3">
            <button
              type="button"
              onClick={() => remove.mutate()}
              disabled={remove.isPending}
              className="text-brand-text hover:text-brand"
            >
              Yes, delete {user.username}
            </button>
            <button type="button" onClick={() => setConfirming(false)} className="text-muted hover:text-ink">
              Keep
            </button>
          </span>
        ) : (
          <button
            type="button"
            onClick={() => setConfirming(true)}
            disabled={lastAdmin}
            aria-label={`Delete ${user.username}`}
            title={lastAdmin ? 'Tubarr needs at least one admin' : undefined}
            className="text-brand-text hover:text-brand disabled:cursor-not-allowed disabled:opacity-40"
          >
            Delete
          </button>
        )}
      </td>
    </tr>
  )
}

const emptyUser = { username: '', password: '', subject: '' }

function AddUser({ auth }: { auth: AuthSummary | undefined }) {
  const queryClient = useQueryClient()
  const [form, setForm] = useState(emptyUser)
  const forms = auth?.password ?? true
  const oidc = auth?.oidc ?? false
  const create = useMutation({
    mutationFn: (u: NewUser) => api.createUser(u),
    onSuccess: () => {
      setForm(emptyUser)
      queryClient.invalidateQueries({ queryKey: ['users'] })
    },
  })

  function submit(e: FormEvent) {
    e.preventDefault()
    const user: NewUser = { username: form.username.trim() }
    if (form.password) user.password = form.password
    if (form.subject.trim()) user.oidc_subject = form.subject.trim()
    create.mutate(user)
  }

  const set = (k: keyof typeof emptyUser) => (v: string) => setForm({ ...form, [k]: v })
  const short = form.password !== '' && [...form.password].length < MIN_PASSWORD

  return (
    // Framed like the "Add mapping by hand" card: heading in the surface-2 header, the Add button in a footer row.
    <form onSubmit={submit} className="mt-8 overflow-hidden rounded-lg border border-line bg-surface shadow-sm">
      <div className="bg-surface-2 px-4 py-5 sm:px-6">
        <h3 className="text-sm/6 font-semibold text-ink">Add a user</h3>
      </div>
      <div className="grid max-w-2xl grid-cols-1 gap-6 border-t border-line px-4 py-6 sm:grid-cols-2 sm:px-6">
        <Field id="user-username" label="Username">
          <input
            id="user-username"
            required
            autoComplete="off"
            value={form.username}
            onChange={(e) => set('username')(e.target.value)}
            className={inputClass}
          />
        </Field>
        {forms && (
          <Field
            id="user-password"
            label="Password"
            help={short ? `At least ${MIN_PASSWORD} characters.` : oidc ? 'Leave empty for an SSO-only user.' : undefined}
          >
            <input
              id="user-password"
              type="password"
              autoComplete="new-password"
              required={!oidc}
              value={form.password}
              onChange={(e) => set('password')(e.target.value)}
              className={inputClass}
            />
          </Field>
        )}
        {oidc && (
          <div className="sm:col-span-2">
            <Field
              id="user-subject"
              label="SSO subject"
              help={`The user's subject (sub) at ${auth?.oidc_issuer ?? 'the provider'}. Links this account to their SSO sign-in.`}
            >
              <input
                id="user-subject"
                autoComplete="off"
                value={form.subject}
                onChange={(e) => set('subject')(e.target.value)}
                className={inputClass}
              />
            </Field>
          </div>
        )}
      </div>
      <div className="flex items-center justify-end gap-x-4 border-t border-line px-4 py-4 sm:px-6">
        {create.isError && (
          <span role="alert" className="text-sm text-brand-text">
            {create.error.message}
          </span>
        )}
        {create.isSuccess && (
          <span role="status" className="text-sm text-muted">
            Added {create.data.username} as {create.data.role}.
          </span>
        )}
        <button type="submit" disabled={create.isPending || short} className={primaryButton}>
          {create.isPending ? 'Adding...' : 'Add user'}
        </button>
      </div>
    </form>
  )
}

export function UsersSection({ auth }: { auth: AuthSummary | undefined }) {
  const users = useQuery({ queryKey: ['users'], queryFn: api.users })
  const admins = (users.data ?? []).filter((u) => u.role === 'admin' && !u.disabled)

  return (
    <section className="mt-10">
      <h2 className="text-base/7 font-semibold text-ink">Users</h2>
      <p className="mt-1 max-w-2xl text-sm/6 text-muted">
        Who may sign in to this page. New users are admins. The last admin cannot be deleted.
      </p>
      {users.isError && (
        <p role="alert" className="mt-6 text-sm text-brand-text">
          {users.error.message}
        </p>
      )}
      {users.data && (
        <Table
          head={
            <>
              <Th first>Username</Th>
              <Th hideBelow="sm">Signs in with</Th>
              <Th hideBelow="sm">Role</Th>
              <Th hideBelow="sm">Last sign-in</Th>
              <Th last>
                <span className="sr-only">Delete</span>
              </Th>
            </>
          }
        >
          {users.data.map((u) => (
            <UserRow key={u.id} user={u} lastAdmin={u.role === 'admin' && !u.disabled && admins.length <= 1} />
          ))}
        </Table>
      )}
      <AddUser auth={auth} />
      {auth && <AuthSummaryList auth={auth} />}
    </section>
  )
}
