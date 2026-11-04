import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { User } from '../lib/api'
import { admin, authSummary } from '../test-data'
import { mockApi, renderPage, type Call } from '../test-util'
import { UsersSection } from './UsersSection'

afterEach(() => vi.unstubAllGlobals())

const cells = (row: HTMLElement) => within(row).getAllByRole('cell')

const sso: User = {
  ...admin,
  id: 2,
  username: 'arshia',
  oidc_issuer: authSummary.oidc_issuer,
  oidc_subject: 'abc123',
  has_password: false,
}

describe('UsersSection', () => {
  it('lists users, keeps the last admin and shows the sign-in summary', async () => {
    mockApi({ 'GET /admin/api/users': [admin] })
    renderPage(<UsersSection auth={{ ...authSummary, local_bypass: true }} />)
    const row = (await screen.findByText('hani')).closest('tr') as HTMLElement
    expect(cells(row)[1]).toHaveTextContent('Password')
    expect(within(row).getByRole('button', { name: 'Delete hani' })).toBeDisabled()
    expect(screen.getByText('Username and password, and SSO')).toBeInTheDocument()
    expect(screen.getByText(authSummary.oidc_issuer as string)).toBeInTheDocument()
    expect(screen.getByText('admins')).toBeInTheDocument()
    expect(screen.getByText(/^on: loopback/)).toBeInTheDocument()
  })

  it('adds a local user and an SSO user', async () => {
    const calls = mockApi({
      'GET /admin/api/users': [admin],
      'POST /admin/api/users': (call: Call) => ({ ...admin, id: 3, ...(call.body as object) }),
    })
    renderPage(<UsersSection auth={authSummary} />)
    await screen.findByText('hani')

    await userEvent.type(screen.getByLabelText('Username'), 'dan')
    await userEvent.type(screen.getByLabelText('Password'), 'a long password')
    await userEvent.click(screen.getByRole('button', { name: 'Add user' }))
    expect(await screen.findByText('Added dan as admin.')).toBeInTheDocument()

    await userEvent.type(screen.getByLabelText('Username'), 'arshia')
    await userEvent.type(screen.getByLabelText('SSO subject'), 'abc123')
    await userEvent.click(screen.getByRole('button', { name: 'Add user' }))
    expect(await screen.findByText('Added arshia as admin.')).toBeInTheDocument()

    const posts = calls.filter((c) => c.method === 'POST')
    expect(posts.map((c) => c.body)).toEqual([
      { username: 'dan', password: 'a long password' },
      { username: 'arshia', oidc_subject: 'abc123' },
    ])
    expect(posts.every((c) => c.headers['X-Requested-With'] === 'tubarr')).toBe(true)
  })

  it('hides the SSO subject when SSO is off and wants a long enough password', async () => {
    mockApi({ 'GET /admin/api/users': [admin] })
    renderPage(<UsersSection auth={{ ...authSummary, oidc: false, oidc_issuer: null, allowed_groups: null }} />)
    await screen.findByText('hani')
    expect(screen.queryByLabelText('SSO subject')).not.toBeInTheDocument()
    expect(screen.queryByText('SSO issuer')).not.toBeInTheDocument()
    await userEvent.type(screen.getByLabelText('Password'), 'short')
    expect(screen.getByRole('button', { name: 'Add user' })).toBeDisabled()
    expect(screen.getByText('At least 8 characters.')).toBeInTheDocument()
  })

  it('deletes a user after a confirm', async () => {
    let users = [admin, sso]
    const calls = mockApi({
      'GET /admin/api/users': () => users,
      'DELETE /admin/api/users/2': () => {
        users = [admin]
        return null
      },
    })
    renderPage(<UsersSection auth={authSummary} />)
    const row = (await screen.findByText('arshia')).closest('tr') as HTMLElement
    expect(cells(row)[1]).toHaveTextContent('SSO')
    expect(screen.getByRole('button', { name: 'Delete hani' })).toBeEnabled()
    await userEvent.click(within(row).getByRole('button', { name: 'Delete arshia' }))
    await userEvent.click(within(row).getByRole('button', { name: 'Yes, delete arshia' }))
    await waitFor(() => expect(screen.queryByText('arshia')).not.toBeInTheDocument())
    expect(calls.some((c) => c.method === 'DELETE' && c.headers['X-Requested-With'] === 'tubarr')).toBe(true)
    expect(screen.getByRole('button', { name: 'Delete hani' })).toBeDisabled()
  })

  it('says whether a user signs in with a password, SSO or both', async () => {
    const both: User = { ...sso, id: 3, username: 'dan', has_password: true }
    mockApi({ 'GET /admin/api/users': [admin, sso, both] })
    renderPage(<UsersSection auth={authSummary} />)
    const row = async (name: string) => (await screen.findByText(name)).closest('tr') as HTMLElement
    expect(cells(await row('hani'))[1]).toHaveTextContent(/^Password$/)
    expect(cells(await row('arshia'))[1]).toHaveTextContent(/^SSO$/)
    expect(cells(await row('dan'))[1]).toHaveTextContent('Password and SSO')
  })

  it('repeats role, sign-in method and last sign-in under the username for phones', async () => {
    mockApi({ 'GET /admin/api/users': [admin, { ...sso, role: 'viewer', disabled: true }] })
    renderPage(<UsersSection auth={authSummary} />)
    const summary = async (name: string) => {
      const first = within(cells((await screen.findByText(name)).closest('tr') as HTMLElement)[0])
      expect(first.getAllByRole('term').map((t) => t.textContent)).toEqual(['Role', 'Signs in with', 'Last sign-in'])
      return first.getAllByRole('definition').map((d) => d.textContent)
    }
    expect(await summary('hani')).toEqual(['admin', 'Password', expect.stringMatching(/^Last sign-in /)])
    expect(await summary('arshia')).toEqual(['viewerdisabled', 'SSO', expect.stringMatching(/^Last sign-in /)])
    expect(screen.getByRole('columnheader', { name: 'Role' })).toHaveClass('hidden', 'sm:table-cell')
    expect(screen.getByRole('columnheader', { name: 'Last sign-in' })).toHaveClass('hidden', 'sm:table-cell')
    expect(screen.getByRole('columnheader', { name: 'Delete' })).not.toHaveClass('hidden')
  })
})
