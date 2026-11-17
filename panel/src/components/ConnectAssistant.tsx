import { useCallback, useEffect, useState, type SubmitEvent } from 'react'
import { ApiError, agent, type AgentAccount } from '../api'
import { textOf, type ProviderId } from '../assistants'
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
  /** An assistant already chosen, as in Settings. */
  initial?: ProviderId
  /** Leaves the steps, where there is somewhere else to go. */
  onCancel?: () => void
}

/** How long a sign-in code stays valid. */
const CODE_LIFE_MS = 15 * 60 * 1000

/** A sign-in with a code to enter on the vendor's site. */
interface Device {
  url: string
  code: string
  until: number
}

/** Shown in place of the chat until an assistant's account is connected,
 * and in Settings to connect another one. */
export function ConnectAssistant({ account, onConnected, initial, onCancel }: ConnectAssistantProps) {
  const [chosen, setChosen] = useState<ProviderId | null>(initial ?? null)
  const [url, setUrl] = useState<string | null>(null)
  const [device, setDevice] = useState<Device | null>(null)
  const [opened, setOpened] = useState(false)
  const [code, setCode] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  // The API key's form, instead of the subscription's sign-in.
  const [keyForm, setKeyForm] = useState(false)
  const text = textOf(chosen)
  const offered = account?.assistants ?? []
  const available = (id: ProviderId) =>
    offered.length === 0 ? id === 'claude' : offered.some((a) => a.provider === id)

  // Connected: the assistant becomes the one in use, then the chat opens.
  const active = account?.active
  const connected = useCallback(
    async (provider: ProviderId) => {
      try {
        if (active && active !== provider) await agent.use(provider)
        onConnected(await agent.account())
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err))
        setBusy(false)
      }
    },
    [active, onConnected],
  )

  const start = async () => {
    if (!chosen) return
    setBusy(true)
    setError(null)
    try {
      const started = await agent.login(chosen)
      if (started.connected) {
        // Still signed in from before: it is ready to use.
        await connected(chosen)
        return
      }
      if (started.code) {
        setDevice({ url: started.url, code: started.code, until: Date.now() + CODE_LIFE_MS })
      } else {
        setUrl(started.url)
      }
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
      await agent.code(code.trim())
      await connected('claude')
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err))
      setBusy(false)
    }
  }

  // Back to the assistant's ways to connect.
  const choose = () => {
    setUrl(null)
    setDevice(null)
    setKeyForm(false)
    setError(null)
  }

  const another = () => {
    choose()
    if (onCancel && initial) onCancel()
    else setChosen(null)
  }

  if (account?.problem && !chosen) {
    return (
      <section className="connect" aria-labelledby="connect-title">
        <Mark size={44} />
        <h2 id="connect-title">Connect an assistant</h2>
        <p className="connect-text connect-wait">{account.problem}. Try again in a minute.</p>
      </section>
    )
  }

  if (chosen && keyForm) {
    return (
      <section className="connect" aria-labelledby="connect-title">
        <Mark size={44} />
        <h2 id="connect-title">Connect {text.name}</h2>
        <p className="connect-text">
          The assistant uses an {text.company} API key: its answers are paid by use, on the key's
          account.
        </p>
        <ApiKeyForm
          provider={chosen}
          submitLabel="Connect"
          onSaved={() => connected(chosen)}
          onBusy={setBusy}
        />
        {error && (
          <p className="connect-error" role="alert">
            {sentence(error)}
          </p>
        )}
        <button type="button" className="connect-restart" onClick={choose} disabled={busy}>
          Choose another way
        </button>
      </section>
    )
  }

  if (chosen && device) {
    return (
      <DeviceSteps
        name={text.name}
        site={text.site}
        device={device}
        onConnected={() => connected(chosen)}
        onAgain={start}
        onBack={choose}
        busy={busy}
        error={error}
      />
    )
  }

  if (chosen && url === null) {
    return (
      <section className="connect" aria-labelledby="connect-title">
        <Mark size={44} />
        <h2 id="connect-title">Connect {text.name}</h2>
        <p className="connect-text">
          {text.name} works with a subscription of {text.plans}, or with an API key paid by use.
        </p>
        <div className="connect-options">
          <button className="connect-option" onClick={start} disabled={busy}>
            <span className="connect-badge">{text.badge}</span>
            <span className="connect-option-text">
              <span className="connect-option-name">Subscription</span>
              <span className="connect-option-detail">{text.subscription}</span>
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
            <span className="connect-badge">{text.badge}</span>
            <span className="connect-option-text">
              <span className="connect-option-name">API key</span>
              <span className="connect-option-detail">
                From the {text.keyConsole.label}, paid by use
              </span>
            </span>
          </button>
        </div>
        {error && (
          <p className="connect-error" role="alert">
            {sentence(error)}
          </p>
        )}
        <button type="button" className="connect-restart" onClick={another} disabled={busy}>
          Choose another assistant
        </button>
      </section>
    )
  }

  if (!chosen) {
    const choices: ProviderId[] = ['claude', 'codex']
    return (
      <section className="connect" aria-labelledby="connect-title">
        <Mark size={44} />
        <h2 id="connect-title">Connect an assistant</h2>
        <p className="connect-text">
          The assistant builds and changes the site. It works with an account you already have:
          choose the one you pay for. It can be changed later.
        </p>
        <div className="connect-options">
          {choices.filter(available).map((id) => {
            const item = textOf(id)
            return (
              <button key={id} className="connect-option" onClick={() => setChosen(id)}>
                <span className="connect-badge">{item.badge}</span>
                <span className="connect-option-text">
                  <span className="connect-option-name">{item.name}</span>
                  <span className="connect-option-detail">
                    {item.company}, subscription or API key
                  </span>
                </span>
              </button>
            )
          })}
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
      <h2 id="connect-title">Connect {text.name}</h2>
      <p className="connect-text">
        The assistant uses the subscription of the {text.name} account that signs in.
      </p>

      <ol className="steps">
        <li className={`step ${opened ? 'step-done' : 'step-current'}`}>
          <span className="step-marker" aria-hidden="true">
            {opened ? <Icon name="check" size={14} strokeWidth={2.25} /> : '1'}
          </span>
          <div className="step-body">
            <h3 className="step-title">Sign in to {text.name}</h3>
            <p className="step-text">
              Opens {text.site} in a new tab. Allow access, then copy the code shown at the end.
            </p>
            <a
              className={opened ? 'step-again' : buttonClass('primary')}
              href={url ?? undefined}
              target="_blank"
              rel="noreferrer"
              onClick={() => setOpened(true)}
            >
              {opened ? `Open ${text.site} again` : `Open ${text.site}`}
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
                placeholder={`Code from ${text.site}`}
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

/** The sign-in with a code to enter on the vendor's site: the code, where
 * to enter it, and the wait until the vendor confirms. */
function DeviceSteps({
  name,
  site,
  device,
  onConnected,
  onAgain,
  onBack,
  busy,
  error,
}: {
  name: string
  site: string
  device: Device
  onConnected: () => void
  onAgain: () => void
  onBack: () => void
  busy: boolean
  error: string | null
}) {
  const [now, setNow] = useState(() => Date.now())
  const [copied, setCopied] = useState(false)
  const [problem, setProblem] = useState<string | null>(null)
  const left = Math.max(0, device.until - now)
  const expired = left === 0

  // The vendor confirms on its own site: the panel asks every few seconds,
  // and at once when the page comes back to the front (from a phone's
  // other app, for example).
  useEffect(() => {
    if (expired) return
    let done = false
    const check = async () => {
      try {
        const status = await agent.account('codex')
        if (done) return
        if (status.signed_in) {
          done = true
          onConnected()
        } else if (!status.signing_in && status.problem) {
          setProblem(status.problem)
        }
      } catch {
        // The next check tries again.
      }
    }
    const timer = window.setInterval(check, 3000)
    const tick = window.setInterval(() => setNow(Date.now()), 1000)
    const visible = () => {
      if (document.visibilityState === 'visible') void check()
    }
    document.addEventListener('visibilitychange', visible)
    return () => {
      done = true
      window.clearInterval(timer)
      window.clearInterval(tick)
      document.removeEventListener('visibilitychange', visible)
    }
  }, [device, expired, onConnected])

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(device.code)
      setCopied(true)
      window.setTimeout(() => setCopied(false), 2000)
    } catch {
      setCopied(false)
    }
  }

  const minutes = Math.floor(left / 60000)
  const seconds = Math.floor((left % 60000) / 1000)
  const shown = error ?? problem

  return (
    <section className="connect" aria-labelledby="connect-title">
      <Mark size={44} />
      <h2 id="connect-title">Connect {name}</h2>
      <p className="connect-text">
        Sign in on {site} and enter this code there. Sign-in with a code must be turned on in the
        security settings of the ChatGPT account.
      </p>

      <div className="connect-device">
        <code className="connect-device-code" aria-label="Sign-in code">
          {device.code}
        </code>
        <Button small variant="quiet" onClick={copy}>
          {copied ? 'Copied' : 'Copy'}
        </Button>
      </div>

      <a className={buttonClass('primary')} href={device.url} target="_blank" rel="noreferrer">
        Open {site}
        <Icon name="external" size={15} strokeWidth={1.75} />
      </a>

      <p className="connect-hint">Enter the code only on {site}. Nobody from this site will ask for it.</p>

      {expired ? (
        <p className="connect-text connect-wait" role="status">
          The code expired.
        </p>
      ) : (
        <p className="connect-text connect-wait" role="status" aria-live="polite">
          Waiting for you to approve on {site}… The code is valid for {minutes}:
          {String(seconds).padStart(2, '0')}.
        </p>
      )}
      {shown && (
        <p className="connect-error" role="alert">
          {sentence(shown)}
        </p>
      )}

      <div className="connect-actions">
        <Button small onClick={onAgain} disabled={busy}>
          Get a new code
        </Button>
        <Button small onClick={onBack} disabled={busy}>
          Choose another way
        </Button>
      </div>
    </section>
  )
}
