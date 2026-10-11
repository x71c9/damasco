import { useState, type SubmitEvent } from 'react'
import { ApiError, agent, type AgentAccount } from '../api'
import { sentence } from '../text'
import { Icon, Mark } from './icons'
import { Button } from './Button'
import { buttonClass } from './buttonClass'
import './Field.css'
import './ConnectAssistant.css'

interface ConnectAssistantProps {
  account: AgentAccount | null
  onConnected: (account: AgentAccount) => void
}

/** Shown in place of the chat until the assistant's account is connected. */
export function ConnectAssistant({ account, onConnected }: ConnectAssistantProps) {
  const [url, setUrl] = useState<string | null>(null)
  const [opened, setOpened] = useState(false)
  const [code, setCode] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

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

  if (account?.problem) {
    return (
      <section className="connect" aria-labelledby="connect-title">
        <Mark size={44} />
        <h2 id="connect-title">Connect an assistant</h2>
        <p className="connect-text connect-wait">{account.problem}. Try again in a minute.</p>
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
          <button className="connect-option" onClick={start} disabled={busy}>
            <span className="connect-badge">C</span>
            <span className="connect-option-text">
              <span className="connect-option-name">Claude</span>
              <span className="connect-option-detail">Anthropic account, Pro or Max</span>
            </span>
          </button>
          <button className="connect-option" disabled>
            <span className="connect-badge">G</span>
            <span className="connect-option-text">
              <span className="connect-option-name">GPT</span>
              <span className="connect-option-detail">OpenAI account, later</span>
            </span>
          </button>
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

      <button type="button" className="connect-restart" onClick={start} disabled={busy}>
        Get a new sign-in link
      </button>
    </section>
  )
}
