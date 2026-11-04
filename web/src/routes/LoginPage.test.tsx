import { cleanup, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { signedOut } from '../test-data'
import { errorResponse, mockApi, renderPage } from '../test-util'
import { LoginPage } from './LoginPage'

afterEach(() => vi.unstubAllGlobals())

async function signIn(password: string) {
  await userEvent.type(screen.getByLabelText('Username'), 'hani')
  await userEvent.type(screen.getByLabelText('Password'), password)
  await userEvent.click(screen.getByRole('button', { name: 'Sign in' }))
}

describe('LoginPage', () => {
  it('says so when the password is wrong and clears it', async () => {
    mockApi({ 'POST /auth/login': errorResponse(401, 'wrong username or password') })
    renderPage(<LoginPage state={signedOut} next="/" error={null} />)
    await signIn('nope')
    expect(await screen.findByRole('alert')).toHaveTextContent('Wrong username or password.')
    expect(screen.getByLabelText('Password')).toHaveValue('')
  })

  it('shows the wait from Retry-After when throttled', async () => {
    mockApi({
      'POST /auth/login': errorResponse(429, 'too many failed sign-ins, try again in 42 s', { 'Retry-After': '42' }),
    })
    renderPage(<LoginPage state={signedOut} next="/" error={null} />)
    await signIn('again')
    expect(await screen.findByRole('alert')).toHaveTextContent('Too many failed sign-ins. Try again in 42 seconds.')
  })

  it('offers only the SSO button when forms are off', () => {
    const state = {
      ...signedOut,
      methods: { password: false, oidc: true },
      oidc_button_label: 'Sign in with Authentik',
    }
    renderPage(<LoginPage state={state} next="/health" error={null} />)
    const sso = screen.getByRole('link', { name: 'Sign in with Authentik' })
    expect(sso).toHaveAttribute('href', '/auth/oidc/start?next=%2Fhealth')
    expect(screen.queryByLabelText('Username')).not.toBeInTheDocument()
  })

  it('offers both with forms and SSO, and explains a failed SSO return', () => {
    const state = { ...signedOut, methods: { password: true, oidc: true }, oidc_button_label: 'Sign in with SSO' }
    renderPage(<LoginPage state={state} next="/" error="not_allowed" />)
    expect(screen.getByLabelText('Username')).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'Sign in with SSO' })).toBeInTheDocument()
    expect(screen.getByRole('alert')).toHaveTextContent('not allowed into Tubarr')
  })

  it('has a message for every SSO error code', () => {
    for (const [code, text] of [
      ['expired', 'took too long'],
      ['failed', 'did not work'],
      ['something-new', 'did not work'],
    ]) {
      renderPage(<LoginPage state={signedOut} next="/" error={code} />)
      expect(screen.getByRole('alert')).toHaveTextContent(text)
      cleanup()
    }
  })
})
