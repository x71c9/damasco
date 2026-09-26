import { useEffect, useRef, useState, type KeyboardEvent } from 'react'
import { agent, type AgentAccount } from '../api'
import { Icon, Mark } from './icons'
import './Chat.css'

interface Message {
  id: number
  from: 'person' | 'notice'
  text: string
}

/** Chat with the assistant. The assistant is not connected yet: messages
 * stay in the page and a notice explains it. */
interface ChatProps {
  account: AgentAccount
  /** The assistant was disconnected; the new state of its account. */
  onDisconnected: (account: AgentAccount) => void
}

export function Chat({ account, onDisconnected }: ChatProps) {
  const [messages, setMessages] = useState<Message[]>([])
  const [draft, setDraft] = useState('')

  const send = (event?: { preventDefault: () => void }) => {
    event?.preventDefault()
    const text = draft.trim()
    if (!text) return
    setMessages((current) => [
      ...current,
      { id: current.length, from: 'person', text },
      {
        id: current.length + 1,
        from: 'notice',
        text: 'The assistant is not connected yet. Messages are not sent anywhere in this draft.',
      },
    ])
    setDraft('')
  }

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey) send(event)
  }

  return (
    <section className="chat" aria-label="Chat with the assistant">
      <header className="chat-header">
        <button className="icon-button" aria-label="Previous conversations" title="Previous conversations">
          <Icon name="history" size={18} />
        </button>
        <span className="chat-title">New conversation</span>
        <button className="icon-button" aria-label="New conversation" title="New conversation">
          <Icon name="newChat" size={18} />
        </button>
      </header>

      <Mark size={300} className="chat-watermark" />

      <div className="chat-body">
        {messages.length === 0 ? (
          <div className="chat-empty">
            <p className="chat-empty-title">Describe the site to build</p>
            <p className="chat-empty-text">What it is for, who it is for, which pages it needs.</p>
          </div>
        ) : (
          <ol className="chat-messages">
            {messages.map((message) => (
              <li key={message.id} className={`chat-message chat-message-${message.from}`}>
                {message.text}
              </li>
            ))}
          </ol>
        )}
      </div>

      <form className="composer" onSubmit={send}>
        <label htmlFor="composer-input" className="visually-hidden">
          Message to the assistant
        </label>
        <textarea
          id="composer-input"
          rows={2}
          placeholder="What would you like to change?"
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={onKeyDown}
        />
        <div className="composer-bar">
          <button type="button" className="icon-button" aria-label="Attach files" title="Attach files">
            <Icon name="plus" size={17} />
          </button>
          <button type="button" className="composer-pill" aria-label="Mode: Auto. Change mode">
            <Icon name="bolt" size={14} />
            Auto
            <Icon name="chevronDown" size={12} strokeWidth={2} />
          </button>
          <span className="composer-spacer" />
          <AssistantMenu account={account} onDisconnected={onDisconnected} />
          <button type="submit" className="composer-send" aria-label="Send" disabled={!draft.trim()}>
            <Icon name="send" size={17} strokeWidth={1.75} />
          </button>
        </div>
      </form>
    </section>
  )
}

/** The assistant in use, with the way to disconnect its account. */
function AssistantMenu({ account, onDisconnected }: ChatProps) {
  const [open, setOpen] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const root = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    const close = (event: MouseEvent | globalThis.KeyboardEvent) => {
      if (event instanceof globalThis.KeyboardEvent ? event.key === 'Escape' : !root.current?.contains(event.target as Node)) {
        setOpen(false)
      }
    }
    document.addEventListener('mousedown', close)
    document.addEventListener('keydown', close)
    return () => {
      document.removeEventListener('mousedown', close)
      document.removeEventListener('keydown', close)
    }
  }, [open])

  const disconnect = async () => {
    setBusy(true)
    setError(null)
    try {
      await agent.logout()
      onDisconnected(await agent.account())
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
      setBusy(false)
    }
  }

  return (
    <div className="assistant" ref={root}>
      <button
        type="button"
        className="composer-assistant"
        aria-label="Assistant: Claude"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        Claude
        <Icon name="chevronDown" size={12} strokeWidth={2} />
      </button>
      {open && (
        <div className="assistant-menu" role="menu" aria-label="Assistant">
          <div className="assistant-who">
            <span className="assistant-name">Claude</span>
            <span className="assistant-meta">
              {account.method === 'claude.ai' ? 'Claude subscription' : 'Signed in'}
            </span>
          </div>
          <button
            type="button"
            className="assistant-item assistant-item-danger"
            role="menuitem"
            onClick={disconnect}
            disabled={busy}
          >
            Disconnect Claude
          </button>
          {error && (
            <p className="assistant-error" role="alert">
              {error}
            </p>
          )}
        </div>
      )}
    </div>
  )
}
