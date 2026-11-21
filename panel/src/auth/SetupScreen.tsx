import { useEffect, useState, type SubmitEvent } from 'react'
import { ApiError, auth, type LinkPurpose, type Me } from '../api'
import { Field } from '../components/Field'
import { AuthLayout } from './AuthLayout'
import { OwnerName } from './LoginScreen'

const PASSWORD_MIN = 10

interface SetupScreenProps {
  token: string
  site: string | null
  onSignedIn: (me: Me) => void
}

/** Opened from a one-time link (/setup#token): sets the owner's first
 * password, or a new one, then signs in. */
export function SetupScreen({ token, site, onSignedIn }: SetupScreenProps) {
  const [purpose, setPurpose] = useState<LinkPurpose | 'invalid' | null>(null)
  const [password, setPassword] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    auth.link(token).then(
      (link) => setPurpose(link.purpose),
      () => setPurpose('invalid'),
    )
  }, [token])

  const submit = async (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (purpose !== 'setup' && purpose !== 'reset') return
    // The same rule as the server, so that a short password shows without
    // a round trip.
    if (password.length < PASSWORD_MIN) {
      setError(`At least ${PASSWORD_MIN} characters`)
      return
    }
    setBusy(true)
    try {
      onSignedIn(await auth.setup(token, password))
    } catch (err) {
      if (err instanceof ApiError && err.status === 410) {
        setPurpose('invalid')
      } else {
        setError(err instanceof Error ? err.message : String(err))
      }
      setBusy(false)
    }
  }

  if (purpose === null) {
    return (
      <AuthLayout title="One moment" site={site}>
        {null}
      </AuthLayout>
    )
  }

  if (purpose === 'invalid') {
    return (
      <AuthLayout title="Link not valid" site={site}>
        <p className="auth-text">
          The link has already been used or has expired: each link works once, for a limited
          time. On the server, <code>damasco owner-link</code> prints a new one.
        </p>
        <a className="auth-submit" href="/">
          Go to sign in
        </a>
      </AuthLayout>
    )
  }

  const setup = purpose === 'setup'
  return (
    <AuthLayout title={setup ? 'Create the account' : 'New password'} site={site}>
      <form className="auth-form" onSubmit={submit} noValidate>
        <OwnerName />
        <Field
          id="password"
          label="Password"
          note={`${PASSWORD_MIN}+ characters`}
          type="password"
          name="new-password"
          autoComplete="new-password"
          error={error}
          value={password}
          onChange={(event) => {
            setPassword(event.target.value)
            setError(null)
          }}
        />
        <button className="auth-submit" type="submit" disabled={busy}>
          {setup ? 'Create the account' : 'Set the password'}
        </button>
      </form>
    </AuthLayout>
  )
}
