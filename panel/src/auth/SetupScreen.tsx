import { useEffect, useState, type SubmitEvent } from 'react'
import { ApiError, auth, type LinkPurpose, type Me } from '../api'
import { Field } from '../components/Field'
import { AuthLayout } from './AuthLayout'

const PASSWORD_MIN = 10

type FieldName = 'name' | 'email' | 'password'
type Errors = Partial<Record<FieldName, string>>

interface SetupScreenProps {
  token: string
  site: string | null
  onSignedIn: (me: Me) => void
}

/** Checks done in the browser before sending, with the same rules as the
 * server, so that most mistakes show without a round trip. */
function check(purpose: LinkPurpose, name: string, email: string, password: string): Errors {
  const errors: Errors = {}
  if (purpose === 'setup') {
    if (!name.trim()) errors.name = 'Required'
    if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim())) errors.email = 'Not a valid email address'
  }
  if (password.length < PASSWORD_MIN) errors.password = `At least ${PASSWORD_MIN} characters`
  return errors
}

/** Opened from a one-time link (/setup#token): creates the account or sets
 * a new password, then signs in. */
export function SetupScreen({ token, site, onSignedIn }: SetupScreenProps) {
  const [purpose, setPurpose] = useState<LinkPurpose | 'invalid' | null>(null)
  const [name, setName] = useState('')
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [errors, setErrors] = useState<Errors>({})
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    auth.link(token).then(
      (link) => setPurpose(link.purpose),
      () => setPurpose('invalid'),
    )
  }, [token])

  const edit = (field: FieldName, set: (value: string) => void) => (value: string) => {
    set(value)
    setErrors((current) => {
      const next = { ...current }
      delete next[field]
      return next
    })
  }

  const submit = async (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (purpose !== 'setup' && purpose !== 'reset') return
    const found = check(purpose, name, email, password)
    setErrors(found)
    if (Object.keys(found).length > 0) return
    setBusy(true)
    try {
      const me =
        purpose === 'setup'
          ? await auth.setup(token, password, email, name)
          : await auth.setup(token, password)
      onSignedIn(me)
    } catch (err) {
      if (err instanceof ApiError && err.status === 410) {
        setPurpose('invalid')
      } else if (err instanceof ApiError && err.field) {
        setErrors({ [err.field]: err.message })
      } else {
        setErrors({ password: err instanceof Error ? err.message : String(err) })
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
          time. A new one comes from the person who sent it.
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
        {setup && (
          <>
            <Field
              id="name"
              label="Name"
              note="Shown to the other people"
              name="name"
              autoComplete="name"
              maxLength={80}
              error={errors.name}
              value={name}
              onChange={(event) => edit('name', setName)(event.target.value)}
            />
            <Field
              id="email"
              label="Email"
              note="Used to sign in"
              type="email"
              name="email"
              autoComplete="username"
              autoCapitalize="none"
              spellCheck={false}
              error={errors.email}
              value={email}
              onChange={(event) => edit('email', setEmail)(event.target.value)}
            />
          </>
        )}
        <Field
          id="password"
          label="Password"
          note={`${PASSWORD_MIN}+ characters`}
          type="password"
          name="new-password"
          autoComplete="new-password"
          error={errors.password}
          value={password}
          onChange={(event) => edit('password', setPassword)(event.target.value)}
        />
        <button className="auth-submit" type="submit" disabled={busy}>
          {setup ? 'Create the account' : 'Set the password'}
        </button>
      </form>
    </AuthLayout>
  )
}
