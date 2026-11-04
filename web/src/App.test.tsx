import { QueryClientProvider } from '@tanstack/react-query'
import { createMemoryHistory } from '@tanstack/react-router'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { App } from './App'
import { createQueryClient } from './lib/auth'
import { admin, signedIn, signedOut } from './test-data'
import { errorResponse, mockApi } from './test-util'

afterEach(() => vi.unstubAllGlobals())

function start(path: string) {
  const history = createMemoryHistory({ initialEntries: [path] })
  render(
    <QueryClientProvider client={createQueryClient()}>
      <App history={history} />
    </QueryClientProvider>,
  )
  return history
}

const settings = {
  public_url: 'http://tubarr:8480/',
  sonarr: null,
  radarr: null,
  arr: { tag: 'youtube', sync_interval_secs: 900 },
  films: { channels: [], min_duration_secs: 4800, similarity: 0.9 },
}

describe('App bootstrap', () => {
  it('shows setup when no user exists and enters the app after it', async () => {
    const calls = mockApi({
      'GET /auth/state': { ...signedOut, setup_required: true },
      'POST /auth/setup': { user: admin },
      'GET /admin/api/mappings': [],
      'GET /admin/api/settings': settings,
    })
    start('/')
    expect(await screen.findByText("Find it in Tubarr's log")).toBeInTheDocument()
    await userEvent.type(screen.getByLabelText('Username'), 'hani')
    await userEvent.type(screen.getByLabelText('Password'), 'short')
    await userEvent.type(screen.getByLabelText('Confirm password'), 'short')
    const create = screen.getByRole('button', { name: 'Create account and sign in' })
    expect(create).toBeDisabled()
    expect(screen.getByText(/At least 8 characters/)).toHaveTextContent('(not yet)')

    await userEvent.type(screen.getByLabelText('Password'), ' and long')
    expect(create).toBeDisabled()
    await userEvent.type(screen.getByLabelText('Confirm password'), ' and long')
    expect(screen.getByText(/Both passwords match/)).toHaveTextContent('(done)')
    expect(create).toBeDisabled()
    await userEvent.type(screen.getByLabelText('Setup code'), ' abcd-efgh ')
    await userEvent.click(create)

    expect(await screen.findByText('No mappings')).toBeInTheDocument()
    const setup = calls.find((c) => c.url === '/auth/setup')
    expect(setup?.body).toEqual({ username: 'hani', password: 'short and long', setup_code: 'abcd-efgh' })
    expect(setup?.headers['X-Requested-With']).toBe('tubarr')
    expect(screen.getAllByText('hani').length).toBeGreaterThan(0)
  })

  it('says so when the setup code is wrong', async () => {
    mockApi({
      'GET /auth/state': { ...signedOut, setup_required: true },
      'POST /auth/setup': errorResponse(403, "wrong or missing setup code, find it in Tubarr's log"),
    })
    start('/')
    await userEvent.type(await screen.findByLabelText('Setup code'), 'WRONG')
    await userEvent.type(screen.getByLabelText('Username'), 'hani')
    await userEvent.type(screen.getByLabelText('Password'), 'short and long')
    await userEvent.type(screen.getByLabelText('Confirm password'), 'short and long')
    await userEvent.click(screen.getByRole('button', { name: 'Create account and sign in' }))
    expect(await screen.findByText(/wrong or missing setup code/)).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Create the admin account' })).toBeInTheDocument()
  })

  it('sends a signed-out visitor to the login page and back to where they were', async () => {
    const calls = mockApi({
      'GET /auth/state': signedOut,
      'POST /auth/login': { user: admin },
      'GET /admin/api/settings': settings,
      'GET /admin/api/mappings?kind=film': [],
      'GET /admin/api/users': [admin],
    })
    const history = start('/settings')
    expect(await screen.findByRole('heading', { name: 'Sign in to Tubarr' })).toBeInTheDocument()
    expect(history.location.pathname).toBe('/login')
    expect(history.location.search).toBe('?next=%2Fsettings')
    expect(screen.queryByRole('link', { name: /SSO/ })).not.toBeInTheDocument()

    await userEvent.type(screen.getByLabelText('Username'), 'hani')
    await userEvent.type(screen.getByLabelText('Password'), 'correct horse')
    await userEvent.click(screen.getByRole('button', { name: 'Sign in' }))

    expect(await screen.findByRole('heading', { name: 'Settings' })).toBeInTheDocument()
    expect(history.location.pathname).toBe('/settings')
    expect(calls.find((c) => c.url === '/auth/login')?.headers['X-Requested-With']).toBe('tubarr')
  })

  it('goes straight to the app when signed in, also from /login', async () => {
    mockApi({ 'GET /auth/state': signedIn, 'GET /admin/api/mappings': [] })
    const history = start('/login?next=%2F')
    expect(await screen.findByText('No mappings')).toBeInTheDocument()
    expect(history.location.pathname).toBe('/')
  })

  it('treats local bypass as signed in, without a sign-out menu', async () => {
    mockApi({ 'GET /auth/state': { ...signedOut, via: 'local_bypass' }, 'GET /admin/api/mappings': [] })
    start('/')
    expect(await screen.findByText('No mappings')).toBeInTheDocument()
    expect(screen.getAllByText('Signed in by local network').length).toBeGreaterThan(0)
    expect(screen.queryByRole('button', { name: /Account menu/ })).not.toBeInTheDocument()
  })

  it('sends the user to /login when the admin API answers 401', async () => {
    mockApi({
      'GET /auth/state': signedIn,
      'GET /admin/api/mappings': errorResponse(401, 'not signed in and no valid API key'),
    })
    const history = start('/')
    expect(await screen.findByRole('heading', { name: 'Sign in to Tubarr' })).toBeInTheDocument()
    expect(history.location.pathname).toBe('/login')
  })

  it('signs out from the user menu', async () => {
    const calls = mockApi({ 'GET /auth/state': signedIn, 'GET /admin/api/mappings': [], 'POST /auth/logout': null })
    const history = start('/')
    await userEvent.click(await screen.findByRole('button', { name: /Account menu for/ }))
    await userEvent.click(await screen.findByRole('menuitem', { name: 'Sign out' }))
    expect(await screen.findByRole('heading', { name: 'Sign in to Tubarr' })).toBeInTheDocument()
    expect(history.location.href).toBe('/login')
    expect(calls.find((c) => c.url === '/auth/logout')?.headers['X-Requested-With']).toBe('tubarr')
  })

  it('changes the password from the user menu', async () => {
    const calls = mockApi({
      'GET /auth/state': signedIn,
      'GET /admin/api/mappings': [],
      'POST /auth/password': null,
    })
    start('/')
    await userEvent.click(await screen.findByRole('button', { name: /Account menu for/ }))
    await userEvent.click(await screen.findByRole('menuitem', { name: 'Change password' }))
    await userEvent.type(await screen.findByLabelText('Current password'), 'old password')
    await userEvent.type(screen.getByLabelText('New password'), 'a new long password')
    await userEvent.type(screen.getByLabelText('Confirm new password'), 'a new long password')
    await userEvent.click(screen.getByRole('button', { name: 'Change password' }))
    expect(await screen.findByText(/Password changed/)).toBeInTheDocument()
    await waitFor(() =>
      expect(calls.find((c) => c.url === '/auth/password')?.body).toEqual({
        current_password: 'old password',
        new_password: 'a new long password',
      }),
    )
  })
})
