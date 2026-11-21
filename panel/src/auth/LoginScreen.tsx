import { useState, type SubmitEvent } from 'react'
import { auth, type Me } from '../api'
import { Field } from '../components/Field'
import { AuthLayout } from './AuthLayout'

interface LoginScreenProps {
  site: string | null
  onSignedIn: (me: Me) => void
}

/** A field password managers read as the account's name: the installation
 * has one account, the owner, and no name to type. */
export function OwnerName() {
  return (
    <input
      className="visually-hidden"
      type="text"
      name="username"
      autoComplete="username"
      value="owner"
      readOnly
      tabIndex={-1}
      aria-hidden="true"
    />
  )
}

export function LoginScreen({ site, onSignedIn }: LoginScreenProps) {
  const [password, setPassword] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const submit = async (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault()
    setBusy(true)
    setError(null)
    try {
      onSignedIn(await auth.login(password))
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
      setBusy(false)
    }
  }

  return (
    <AuthLayout title="Sign in" site={site}>
      <form className="auth-form" onSubmit={submit} noValidate>
        <OwnerName />
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
        <button className="auth-submit" type="submit" disabled={busy || !password}>
          Sign in
        </button>
      </form>
      <details className="auth-help">
        <summary>Forgotten password?</summary>
        <p>
          A one-time link sets a new one. On the server, <code>sudo damasco owner-link</code> prints it;
          for an installation with a name, the command is in the installation guide.
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
      <pre className="auth-command">journalctl -u damasco | grep setup#</pre>
      <p className="auth-text">
        The link works once, for 24 hours. Restarting damasco prints a new one.
      </p>
    </AuthLayout>
  )
}
