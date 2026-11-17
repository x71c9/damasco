import { useCallback, useEffect, useState } from 'react'
import { agent, type AgentAccount, type AllowedHost} from '../api'
import { ApiKeyForm } from '../components/ApiKeyForm'
import { Button } from '../components/Button'
import { ConnectAssistant } from '../components/ConnectAssistant'
import { Dialog, DialogActions } from '../components/Dialog'
import { useNotices } from '../notices/context'
import { sentence } from '../text'
import { textOf } from '../assistants'
import { connectedWith, usesKey } from './account'
import '../components/Page.css'
import './Assistant.css'

const day = new Intl.DateTimeFormat(undefined, { day: 'numeric', month: 'short', year: 'numeric' })

type Asking = 'disconnect' | 'subscription' | null

/** The assistant: which one is connected and how, how to change that, and
 * the sites it may reach. For those who use the chat. */
export function AssistantPage({
  account,
  onAccount,
}: {
  account: AgentAccount | null
  onAccount: (account: AgentAccount) => void
}) {
  const { notify } = useNotices()
  const [asking, setAsking] = useState<Asking>(null)
  const [busy, setBusy] = useState(false)
  const [hosts, setHosts] = useState<AllowedHost[] | null>(null)
  const [configured, setConfigured] = useState<string[]>([])

  const loadHosts = useCallback(() => {
    agent.hosts().then(
      (result) => {
        setHosts(result.hosts)
        setConfigured(result.configured)
      },
      () => setHosts([]),
    )
  }, [])
  useEffect(() => {
    loadHosts()
  }, [loadHosts])

  const apiKey = account ? usesKey(account) : false
  const text = textOf(account?.provider)
  // Disconnecting, also before signing in with a subscription: the key goes.
  const disconnect = async (next: Asking) => {
    setAsking(null)
    setBusy(true)
    try {
      await agent.logout(account?.provider)
      onAccount(await agent.account())
      notify({
        kind: 'done',
        title: next === 'subscription' ? 'API key removed' : `${text.name} disconnected`,
        text:
          next === 'subscription'
            ? `Choose ${text.name}, then Subscription, to sign in with a ${text.name} account.`
            : undefined,
      })
    } catch (err) {
      notify({
        kind: 'problem',
        title: 'Couldn’t disconnect',
        text: err instanceof Error ? sentence(err.message) : String(err),
      })
    }
    setBusy(false)
  }

  const removeHost = async (host: string) => {
    try {
      await agent.removeHost(host)
      notify({ kind: 'done', title: `${host} removed`, text: 'The assistant asks again before reaching it.' })
    } catch (err) {
      notify({
        kind: 'problem',
        title: `Couldn’t remove ${host}`,
        text: err instanceof Error ? sentence(err.message) : String(err),
      })
    }
    loadHosts()
  }

  return (
    <section className="page-frame" aria-labelledby="assistant-title">
      <div className="page-card">
        <header className="page-head">
          <div>
            <h1 id="assistant-title">Assistant</h1>
            <p>The AI that builds and changes the site, and the sites it may reach.</p>
          </div>
        </header>

        {account === null ? (
          <p className="assistant-note">Reading the assistant’s account…</p>
        ) : !account.signed_in ? (
          <div className="assistant-box assistant-connect">
            <ConnectAssistant account={account} onConnected={onAccount} />
          </div>
        ) : (
          <>
            <section className="assistant-box assistant-account" aria-label="Connected assistant">
              <span className="connect-badge assistant-badge">{text.badge}</span>
              <span className="assistant-account-text">
                <span className="assistant-account-name">{text.name}</span>
                <span className="assistant-note">{connectedWith(account)}</span>
                {account.expires_at && (
                  <span className="assistant-note">
                    Signed in until {day.format(new Date(account.expires_at))}
                  </span>
                )}
              </span>
              <span className="assistant-state">Connected</span>
            </section>

            <section className="assistant-box" aria-labelledby="assistant-key-title">
              <h2 id="assistant-key-title">{apiKey ? 'Replace the API key' : 'Use an API key instead'}</h2>
              <p className="assistant-note">
                {apiKey
                  ? `For a new key, or when the old one was revoked. The new key is checked first: if ${text.company} refuses it, the current one stays.`
                  : `With an API key the answers are paid by use, on the account of the key, instead of the subscription. The key is checked first: if ${text.company} refuses it, the subscription stays.`}{' '}
                Spending is in the{' '}
                <a href={text.usageUrl} target="_blank" rel="noreferrer">
                  {text.keyConsole.label}
                </a>
                .
              </p>
              <ApiKeyForm
                provider={text.id}
                submitLabel={apiKey ? 'Replace' : 'Switch to the key'}
                onBusy={setBusy}
                onSaved={(next) => {
                  onAccount(next)
                  notify({
                    kind: 'done',
                    title: apiKey ? 'API key replaced' : 'Connected with the API key',
                    text: next.key_ending ? `The key ending in ${next.key_ending} is in use.` : undefined,
                  })
                }}
              />
            </section>
          </>
        )}

        <section className="assistant-box" aria-labelledby="assistant-hosts-title">
          <h2 id="assistant-hosts-title">Sites it may reach</h2>
          <p className="assistant-note">
            {text.company} and the npm registry, always
            {configured.length > 0 && <>, and {configured.join(', ')} from the installation’s configuration</>}.
            For any other site the assistant asks in the chat first; the sites allowed there are
            listed here, and removing one makes it ask again.
          </p>
          {hosts && hosts.length === 0 && <p className="assistant-empty">No other site allowed yet.</p>}
          {hosts && hosts.length > 0 && (
            <ul className="assistant-hosts">
              {hosts.map((item) => (
                <li key={item.host}>
                  <span className="assistant-host-text">
                    <span className="assistant-host">{item.host}</span>
                    <span className="assistant-note">
                      Allowed{item.by_name ? ` by ${item.by_name}` : ''} on {day.format(new Date(item.at))}
                    </span>
                  </span>
                  <Button className="pages-delete" variant="quiet" small onClick={() => removeHost(item.host)}>
                    Remove
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </section>


        {account?.signed_in && (
          <section className="assistant-box assistant-danger" aria-labelledby="assistant-disconnect-title">
            <h2 id="assistant-disconnect-title">Disconnect</h2>
            <p className="assistant-note">
              The assistant stops until it is connected again, here or in the chat. The site and the
              conversations stay as they are. To use another assistant, disconnect {text.name}: the
              assistants to choose from appear here and in the chat.
            </p>
            <div className="assistant-actions">
              <Button variant="danger" disabled={busy} onClick={() => setAsking('disconnect')}>
                Disconnect {text.name}
              </Button>
              {apiKey && (
                <Button disabled={busy} onClick={() => setAsking('subscription')}>
                  Use a subscription instead
                </Button>
              )}
            </div>
          </section>
        )}
      </div>

      {asking && (
        <Dialog
          title={asking === 'subscription' ? 'Use a subscription instead?' : `Disconnect ${text.name}?`}
          onClose={() => setAsking(null)}
        >
          <p className="dialog-text">
            {asking === 'subscription'
              ? `The API key is removed from this server, and the assistant stops until someone signs in with a ${text.plans} account, here or in the chat.`
              : apiKey
                ? 'The API key is removed from this server, and the assistant stops until it is connected again.'
                : `The ${text.name} account is signed out on this server, and the assistant stops until it is connected again.`}
          </p>
          <DialogActions>
            <Button onClick={() => setAsking(null)} autoFocus>
              Keep it
            </Button>
            <Button variant="danger" onClick={() => disconnect(asking)}>
              {asking === 'subscription' ? 'Remove the key' : 'Disconnect'}
            </Button>
          </DialogActions>
        </Dialog>
      )}
    </section>
  )
}
