import { useState, type SubmitEvent } from 'react'
import { auth, type Me } from '../api'
import { Field } from '../components/Field'
import { AuthLayout } from './AuthLayout'

interface LoginScreenProps {
  site: string | null
  onSignedIn: (me: Me) => void
}

export function LoginScreen({ site, onSignedIn }: LoginScreenProps) {
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const submit = async (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault()
    setBusy(true)
    setError(null)
    try {
      onSignedIn(await auth.login(email, password))
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
      setBusy(false)
    }
  }

  return (
    <AuthLayout title="Sign in" site={site}>
      <form className="auth-form" onSubmit={submit} noValidate>
        <Field
          id="email"
          label="Email"
          type="email"
          name="email"
          autoComplete="username"
          autoCapitalize="none"
          spellCheck={false}
          required
          invalid={Boolean(error)}
          value={email}
          onChange={(event) => {
            setEmail(event.target.value)
            setError(null)
          }}
        />
        {/* The answer does not say which of the two is wrong, so the message
            sits on the password and both fields turn red. */}
        <Field
          id="password"
          label="Password"
          type="password"
          name="password"
          autoComplete="current-password"
          required
          error={error}
          value={password}
          onChange={(event) => {
            setPassword(event.target.value)
            setError(null)
          }}
        />
        <button className="auth-submit" type="submit" disabled={busy || !email || !password}>
          Sign in
        </button>
      </form>
      <details className="auth-help">
        <summary>Forgotten password?</summary>
        <p>
          The owner or an administrator can create a new one-time link from the People section.
          The owner gets one on the server with <code>alogeno owner-link</code>.
        </p>
      </details>
    </AuthLayout>
  )
}

/** Shown on a new installation, before the owner's account exists. */
export function NoOwnerScreen({ site }: { site: string | null }) {
  return (
    <AuthLayout title="No owner yet" site={site}>
      <p className="auth-text">
        This installation has no owner. At every start, the server writes a one-time link to
        create the owner&apos;s account in its log. On the server:
      </p>
      <pre className="auth-command">journalctl -u alogeno | grep setup#</pre>
      <p className="auth-text">
        The link works once, for 24 hours. Restarting alogeno prints a new one.
      </p>
    </AuthLayout>
  )
}
