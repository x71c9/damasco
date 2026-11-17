import { useState, type SubmitEvent } from 'react'
import { ApiError, agent, type AgentAccount } from '../api'
import { sentence } from '../text'
import { Icon, Mark } from './icons'
import { ApiKeyForm } from './ApiKeyForm'
import { Button } from './Button'
import { buttonClass } from './buttonClass'
import './Field.css'
import './ConnectAssistant.css'

interface ConnectAssistantProps {
  account: AgentAccount | null
  onConnected: (account: AgentAccount) => void
}

/** Assistants that will work here later, shown so the choice is clear. */
const later = [
  { badge: 'G', name: 'GPT', detail: 'OpenAI account, later' },
  { badge: 'X', name: 'Grok', detail: 'xAI account, later' },
  { badge: 'A', name: 'Antigravity', detail: 'Google account, later' },
]

/** Shown in place of the chat until the assistant's account is connected. */
export function ConnectAssistant({ account, onConnected }: ConnectAssistantProps) {
  const [url, setUrl] = useState<string | null>(null)
  const [opened, setOpened] = useState(false)
  const [code, setCode] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  // Claude chosen: its two ways to connect are shown.
  const [claude, setClaude] = useState(false)
  // The API key's form, instead of the subscription's sign-in.
  const [keyForm, setKeyForm] = useState(false)

  const start = async () => {
    setBusy(true)
    setError(null)
    try {
      setUrl((await agent.login()).url)
      setOpened(false)
      setCode('')
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    }
    setBusy(false)
  }

  const finish = async (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!code.trim()) {
      setError('Paste the code shown at the end of the sign-in')
      return
    }
    setBusy(true)
    setError(null)
    try {
      onConnected(await agent.code(code.trim()))
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err))
      setBusy(false)
    }
  }

  // Back to Claude's ways to connect.
  const choose = () => {
    setUrl(null)
    setKeyForm(false)
    setError(null)
  }

  if (account?.problem) {
    return (
      <section className="connect" aria-labelledby="connect-title">
        <Mark size={44} />
        <h2 id="connect-title">Connect an assistant</h2>
        <p className="connect-text connect-wait">{account.problem}. Try again in a minute.</p>
      </section>
    )
  }

  if (keyForm) {
    return (
      <section className="connect" aria-labelledby="connect-title">
        <Mark size={44} />
        <h2 id="connect-title">Connect Claude</h2>
        <p className="connect-text">
          The assistant uses an Anthropic API key: its answers are paid by use, on the key's
          account.
        </p>
        <ApiKeyForm submitLabel="Connect" onSaved={onConnected} onBusy={setBusy} />
        <button type="button" className="connect-restart" onClick={choose} disabled={busy}>
          Choose another way
        </button>
      </section>
    )
  }

  if (url === null && claude) {
    return (
      <section className="connect" aria-labelledby="connect-title">
        <Mark size={44} />
        <h2 id="connect-title">Connect Claude</h2>
        <p className="connect-text">
          Claude works with a subscription of a Claude account, or with an API key paid by use.
        </p>
        <div className="connect-options">
          <button className="connect-option" onClick={start} disabled={busy}>
            <span className="connect-badge">C</span>
            <span className="connect-option-text">
              <span className="connect-option-name">Subscription</span>
              <span className="connect-option-detail">Claude Pro or Max, signing in on claude.ai</span>
            </span>
          </button>
          <button
            className="connect-option"
            onClick={() => {
              setKeyForm(true)
              setError(null)
            }}
            disabled={busy}
          >
            <span className="connect-badge">C</span>
            <span className="connect-option-text">
              <span className="connect-option-name">API key</span>
              <span className="connect-option-detail">From the Anthropic Console, paid by use</span>
            </span>
          </button>
        </div>
        {error && (
          <p className="connect-error" role="alert">
            {sentence(error)}
          </p>
        )}
        <button
          type="button"
          className="connect-restart"
          onClick={() => {
            setClaude(false)
            setError(null)
          }}
          disabled={busy}
        >
          Choose another assistant
        </button>
      </section>
    )
  }

  if (url === null) {
    return (
      <section className="connect" aria-labelledby="connect-title">
        <Mark size={44} />
        <h2 id="connect-title">Connect an assistant</h2>
        <p className="connect-text">
          The assistant builds and changes the site. It works with an existing account, chosen
          here once.
        </p>
        <div className="connect-options">
          <button className="connect-option" onClick={() => setClaude(true)}>
            <span className="connect-badge">C</span>
            <span className="connect-option-text">
              <span className="connect-option-name">Claude</span>
              <span className="connect-option-detail">Anthropic, subscription or API key</span>
            </span>
          </button>
          {later.map(({ badge, name, detail }) => (
            <button key={name} className="connect-option" disabled>
              <span className="connect-badge">{badge}</span>
              <span className="connect-option-text">
                <span className="connect-option-name">{name}</span>
                <span className="connect-option-detail">{detail}</span>
              </span>
            </button>
          ))}
        </div>
        {error && (
          <p className="connect-error" role="alert">
            {sentence(error)}
          </p>
        )}
      </section>
    )
  }

  return (
    <section className="connect" aria-labelledby="connect-title">
      <Mark size={44} />
      <h2 id="connect-title">Connect Claude</h2>
      <p className="connect-text">
        The assistant uses the subscription of the Claude account that signs in.
      </p>

      <ol className="steps">
        <li className={`step ${opened ? 'step-done' : 'step-current'}`}>
          <span className="step-marker" aria-hidden="true">
            {opened ? <Icon name="check" size={14} strokeWidth={2.25} /> : '1'}
          </span>
          <div className="step-body">
            <h3 className="step-title">Sign in to Claude</h3>
            <p className="step-text">
              Opens claude.ai in a new tab. Allow access, then copy the code shown at the end.
            </p>
            <a
              className={opened ? 'step-again' : buttonClass('primary')}
              href={url}
              target="_blank"
              rel="noreferrer"
              onClick={() => setOpened(true)}
            >
              {opened ? 'Open claude.ai again' : 'Open claude.ai'}
              <Icon name="external" size={15} strokeWidth={1.75} />
            </a>
          </div>
        </li>

        <li className={`step ${opened ? 'step-current' : ''}`}>
          <span className="step-marker" aria-hidden="true">
            2
          </span>
          <form className="step-body field" onSubmit={finish} noValidate>
            <div className="field-head">
              <label htmlFor="code" className="step-title">
                Paste the code
              </label>
              {error && (
                <span id="code-message" className="field-error" role="alert" title={sentence(error)}>
                  {sentence(error)}
                </span>
              )}
            </div>
            <div className="step-code">
              <input
                id="code"
                autoComplete="off"
                spellCheck={false}
                placeholder="Code from claude.ai"
                aria-invalid={Boolean(error) || undefined}
                aria-describedby={error ? 'code-message' : undefined}
                value={code}
                onChange={(event) => {
                  setCode(event.target.value)
                  setError(null)
                }}
              />
              <Button variant={opened ? 'primary' : 'secondary'} type="submit" disabled={busy}>
                {busy ? 'Connecting…' : 'Connect'}
              </Button>
            </div>
          </form>
        </li>
      </ol>

      <div className="connect-links">
        <button type="button" className="connect-restart" onClick={start} disabled={busy}>
          Get a new sign-in link
        </button>
        <button type="button" className="connect-restart" onClick={choose} disabled={busy}>
          Choose another way
        </button>
      </div>
    </section>
  )
}
