import { useState, type SubmitEvent } from 'react'
import { ApiError, agent, type AgentAccount } from '../api'
import { textOf, type ProviderId } from '../assistants'
import { sentence } from '../text'
import { Button } from './Button'
import './Field.css'
import './ConnectAssistant.css'

/** An API key of the assistant's vendor, checked with the vendor before it
 * replaces the way the assistant is connected now: a key refused changes
 * nothing. */
export function ApiKeyForm({
  provider,
  submitLabel,
  onSaved,
  onBusy,
}: {
  provider: ProviderId
  submitLabel: string
  onSaved: (account: AgentAccount) => void
  /** Told while the key is being checked. */
  onBusy?: (busy: boolean) => void
}) {
  const text = textOf(provider)
  const [key, setKey] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const working = (value: boolean) => {
    setBusy(value)
    onBusy?.(value)
  }

  const save = async (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!key.trim()) {
      setError('Paste the API key')
      return
    }
    working(true)
    setError(null)
    try {
      const account = await agent.key(key.trim(), provider)
      setKey('')
      working(false)
      onSaved(account)
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err))
      working(false)
    }
  }

  return (
    <form className="connect-key field" onSubmit={save} noValidate>
      <label htmlFor="api-key">API key</label>
      <div className="step-code">
        <input
          id="api-key"
          // Not a password field: the browser's password manager would take
          // Enter for its suggestions instead of sending the form.
          type="text"
          className="connect-secret"
          autoComplete="off"
          data-1p-ignore=""
          data-lpignore="true"
          spellCheck={false}
          placeholder={text.keyPlaceholder}
          aria-invalid={Boolean(error) || undefined}
          aria-describedby={error ? 'key-message' : 'key-help'}
          value={key}
          onChange={(event) => {
            setKey(event.target.value)
            setError(null)
          }}
        />
        <Button variant="primary" type="submit" disabled={busy}>
          {busy ? 'Checking…' : submitLabel}
        </Button>
      </div>
      {error && (
        <p id="key-message" className="connect-error" role="alert">
          {sentence(error)}
        </p>
      )}
      <p id="key-help" className="connect-hint">
        Create one in the{' '}
        <a href={text.keyConsole.url} target="_blank" rel="noreferrer">
          {text.keyConsole.label}
        </a>
        , under API keys. It is checked with {text.company}, then kept on this server for the
        assistant only.
      </p>
    </form>
  )
}
