import { useState, type KeyboardEvent } from 'react'
import { Icon, Mark } from './icons'
import './Chat.css'

interface Message {
  id: number
  from: 'person' | 'notice'
  text: string
}

const suggestions = [
  'A portfolio for an architecture studio, with projects and contacts',
  'A restaurant page with menu, opening hours and bookings by phone',
  'A personal blog with a short biography',
]

/** Chat with the assistant. The assistant is not connected yet: messages
 * stay in the page and a notice explains it. */
export function Chat() {
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
            <ul className="chat-suggestions">
              {suggestions.map((text) => (
                <li key={text}>
                  <button onClick={() => setDraft(text)}>{text}</button>
                </li>
              ))}
            </ul>
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
          <button type="button" className="composer-assistant" aria-label="Assistant: Claude. Change assistant">
            Claude
            <Icon name="chevronDown" size={12} strokeWidth={2} />
          </button>
          <button type="submit" className="composer-send" aria-label="Send" disabled={!draft.trim()}>
            <Icon name="send" size={17} strokeWidth={1.75} />
          </button>
        </div>
      </form>
    </section>
  )
}
