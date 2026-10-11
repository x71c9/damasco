import {
  createContext,
  useContext,
  useEffect,
  useLayoutEffect,
  useReducer,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from 'react'
import Markdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import {
  ApiError,
  agent,
  chat as chatApi,
  type AgentAccount,
  type ChatConversation,
  type ChatEvent,
  type ChatHolder,
  type ChatItem,
  type ChatQuestions,
  type QuestionAnswer,
} from '../api'
import { Icon, Mark, WaitingMark, Working } from './icons'
import './Chat.css'

interface ChatProps {
  /** The signed-in person. */
  me: number
  account: AgentAccount
  /** The assistant was disconnected; the new state of its account. */
  onDisconnected: (account: AgentAccount) => void
  /** A turn ended: the preview has its changes. */
  onTurnEnd?: () => void
}

interface ChatState {
  conversation: ChatConversation | null
  items: ChatItem[]
  /** The turn in progress: when it started, in this browser's clock. */
  turn: { since: number; stopping: boolean; updating: boolean; waiting: boolean } | null
  /** Who uses the chat, and until when in this browser's clock (null during a turn). */
  holder: { id: number; name: string; until: number | null } | null
}

const empty: ChatState = { conversation: null, items: [], turn: null, holder: null }

function holderOf(holder: ChatHolder | null): ChatState['holder'] {
  if (!holder) return null
  return {
    id: holder.id,
    name: holder.name,
    until: holder.remaining_ms === null ? null : Date.now() + holder.remaining_ms,
  }
}

/** Whether the signed-in person can act on the chat: answer, stop, write. */
const CanAct = createContext(true)

function reduce(state: ChatState, event: ChatEvent): ChatState {
  switch (event.type) {
    case 'snapshot':
      return {
        conversation: event.conversation,
        items: event.items,
        turn: event.turn
          ? {
              since: Date.now() - event.turn.elapsed_ms,
              stopping: event.turn.stopping,
              updating: event.turn.updating,
              waiting: event.turn.waiting,
            }
          : null,
        holder: holderOf(event.holder),
      }
    case 'item': {
      const known = state.items.some((item) => item.id === event.item.id)
      return {
        ...state,
        items: known
          ? state.items.map((item) => (item.id === event.item.id ? event.item : item))
          : [...state.items, event.item],
      }
    }
    case 'delta':
      // Only the next piece is applied: a piece already in the snapshot,
      // or one after a gap, is skipped (the complete text follows anyway).
      return {
        ...state,
        items: state.items.map((item) =>
          item.id === event.id && event.seq === item.seq + 1
            ? { ...item, text: item.text + event.text, seq: event.seq }
            : item,
        ),
      }
    case 'turn':
      return {
        ...state,
        conversation: event.conversation,
        turn: event.turn
          ? {
              since: state.turn?.since ?? Date.now() - event.turn.elapsed_ms,
              stopping: event.turn.stopping,
              updating: event.turn.updating,
              waiting: event.turn.waiting,
            }
          : null,
        holder: holderOf(event.holder),
      }
    default:
      return state
  }
}

/** Follows the chat on the server: a snapshot, then every change. */
function useChatEvents(): [ChatState, boolean] {
  const [state, dispatch] = useReducer(reduce, empty)
  const [connected, setConnected] = useState(true)

  useEffect(() => {
    let source: EventSource | null = null
    const open = () => {
      source = new EventSource(chatApi.eventsUrl)
      source.onopen = () => setConnected(true)
      // The browser connects again by itself.
      source.onerror = () => setConnected(false)
      source.onmessage = (message) => {
        const event = JSON.parse(message.data) as ChatEvent
        if (event.type === 'reset') {
          source?.close()
          open()
        } else {
          dispatch(event)
        }
      }
    }
    open()
    return () => source?.close()
  }, [])

  return [state, connected]
}

function elapsed(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000))
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`
}

/** Chat with the assistant. */
export function Chat({ me, account, onDisconnected, onTurnEnd }: ChatProps) {
  const [{ conversation, items, turn, holder }, connected] = useChatEvents()
  // Another person holds the chat: during their turn and for a while after.
  // The hold that ran out, set by a timer when it does.
  const [expired, setExpired] = useState<number | null>(null)
  const heldBy =
    holder && holder.id !== me && (holder.until === null || holder.until !== expired)
      ? holder
      : null
  useEffect(() => {
    const until = holder?.until
    if (until == null) return
    const timer = setTimeout(() => setExpired(until), Math.max(0, until - Date.now()) + 50)
    return () => clearTimeout(timer)
  }, [holder])
  const canAct = !heldBy
  const label = turn?.updating
    ? 'Updating the preview'
    : turn?.stopping
      ? 'Stopping'
      : turn?.waiting
        ? 'Waiting for your answer'
        : 'Working'

  // When a turn ends, the preview shows what changed.
  const running = Boolean(turn)
  const wasRunning = useRef(running)
  const turnEnded = useRef(onTurnEnd)
  useEffect(() => {
    turnEnded.current = onTurnEnd
  })
  useEffect(() => {
    if (wasRunning.current && !running) turnEnded.current?.()
    wasRunning.current = running
  }, [running])
  const [draft, setDraft] = useState('')
  const [textSize, setTextSize] = useTextSize()
  const [problem, setProblem] = useState<string | null>(null)
  const [sending, setSending] = useState(false)
  const [now, setNow] = useState(() => Date.now())
  const body = useRef<HTMLDivElement>(null)
  const atBottom = useRef(true)

  // The clock of the turn in progress.
  useEffect(() => {
    if (!turn) return
    const timer = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [turn])

  // Stays at the end of the conversation while it grows, unless the
  // person scrolled up to read. Only the person's own scrolling (wheel,
  // touch, keys, scroll bar) leaves the end: the chat growing, or its own
  // scrolling, never does.
  const userScrolled = useRef(0)
  useLayoutEffect(() => {
    const element = body.current
    if (!element) return
    const follow = () => {
      if (atBottom.current) element.scrollTop = element.scrollHeight
    }
    follow()
    const observer = new ResizeObserver(follow)
    if (element.firstElementChild) observer.observe(element.firstElementChild)
    return () => observer.disconnect()
  }, [items])
  const markUserScroll = () => {
    userScrolled.current = Date.now()
  }

  const send = async (event?: { preventDefault: () => void }) => {
    event?.preventDefault()
    const text = draft.trim()
    if (!text || turn || sending) return
    setSending(true)
    setProblem(null)
    try {
      await chatApi.send(text)
      setDraft('')
      atBottom.current = true
    } catch (err) {
      setProblem(err instanceof ApiError ? err.message : String(err))
    }
    setSending(false)
  }

  const stop = () => {
    chatApi.stop().catch((err) => setProblem(String(err)))
  }

  const takeOver = async () => {
    setProblem(null)
    try {
      await chatApi.takeOver()
    } catch (err) {
      setProblem(err instanceof ApiError ? err.message : String(err))
    }
  }

  const startNew = async () => {
    setProblem(null)
    try {
      await chatApi.newConversation()
    } catch (err) {
      setProblem(err instanceof ApiError ? err.message : String(err))
    }
  }

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing) send(event)
    // Up in an empty field brings back the last message, to send it again
    // or change it.
    if (event.key === 'ArrowUp' && draft === '') {
      const last = items.findLast((item) => item.kind === 'person')
      if (last) {
        event.preventDefault()
        setDraft(last.text)
      }
    }
  }

  // Esc stops the assistant wherever the focus is in the panel.
  const stoppable = Boolean(canAct && turn && !turn.stopping && !turn.updating)
  useEffect(() => {
    if (!stoppable) return
    const onEscape = (event: globalThis.KeyboardEvent) => {
      if (event.key === 'Escape' && !event.defaultPrevented) {
        chatApi.stop().catch((err) => setProblem(String(err)))
      }
    }
    document.addEventListener('keydown', onEscape)
    return () => document.removeEventListener('keydown', onEscape)
  }, [stoppable])

  return (
    <section className="chat" aria-label="Chat with the assistant" data-size={textSize}>
      <header className="chat-header">
        <button className="icon-button" aria-label="Previous conversations" title="Previous conversations">
          <Icon name="history" size={18} />
        </button>
        <span className="composer-spacer" />
        <button
          className="icon-button"
          aria-label="New conversation"
          title="New conversation"
          onClick={startNew}
          disabled={Boolean(turn) || !conversation || !canAct}
        >
          <Icon name="newChat" size={18} />
        </button>
        <ChatSettings textSize={textSize} onTextSize={setTextSize} />
      </header>

      <Mark size={300} className="chat-watermark" />

      <div
        className="chat-body"
        ref={body}
        onWheel={markUserScroll}
        onTouchMove={markUserScroll}
        onPointerDown={markUserScroll}
        onKeyDown={markUserScroll}
        onScroll={(event) => {
          const element = event.currentTarget
          const near = element.scrollHeight - element.scrollTop - element.clientHeight < 40
          if (near) atBottom.current = true
          else if (Date.now() - userScrolled.current < 600) atBottom.current = false
        }}
      >
        {items.length === 0 ? (
          <div className="chat-empty">
            <p className="chat-empty-title">Describe the site to build</p>
            <p className="chat-empty-text">What it is for, who it is for, which pages it needs.</p>
          </div>
        ) : (
          <CanAct.Provider value={canAct}>
            <ol className="chat-messages">{render(items)}</ol>
          </CanAct.Provider>
        )}
      </div>

      <div className="chat-status" role="status">
        {!connected ? (
          <span>Connecting again…</span>
        ) : turn ? (
          <>
            {turn.waiting && !turn.stopping ? <WaitingMark size={16} /> : <Working label={label} />}
            <span>{label}</span>
            <span className="chat-status-time">{elapsed(now - turn.since)}</span>
            <span className="composer-spacer" />
            {canAct && !turn.stopping && !turn.updating && (
              <span className="chat-status-hint">
                <kbd>Esc</kbd> to stop
              </span>
            )}
          </>
        ) : problem ? (
          <span className="chat-status-error" title={problem}>
            {problem.charAt(0).toUpperCase() + problem.slice(1)}
          </span>
        ) : null}
      </div>

      {heldBy ? (
        <div className="chat-held" role="status">
          <span>
            <strong>{heldBy.name}</strong> is using the assistant.
          </span>
          {!turn && (
            <button type="button" className="button" onClick={takeOver}>
              Use it now
            </button>
          )}
        </div>
      ) : (
      <form className="composer" onSubmit={send}>
        <label htmlFor="composer-input" className="visually-hidden">
          Message to the assistant
        </label>
        <textarea
          id="composer-input"
          rows={2}
          placeholder={conversation ? 'What would you like to change?' : 'Describe the site to build'}
          value={draft}
          onChange={(event) => {
            setDraft(event.target.value)
            setProblem(null)
          }}
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
          {turn && !turn.updating ? (
            <button
              type="button"
              className="composer-send"
              aria-label="Stop the assistant"
              title="Stop (Esc)"
              onClick={stop}
              disabled={turn.stopping}
            >
              <svg aria-hidden="true" width="12" height="12" viewBox="0 0 12 12">
                <rect width="12" height="12" rx="2.5" fill="currentColor" />
              </svg>
            </button>
          ) : (
            <button
              type="submit"
              className="composer-send"
              aria-label="Send"
              disabled={!draft.trim() || sending || Boolean(turn)}
            >
              <Icon name="send" size={17} strokeWidth={1.75} />
            </button>
          )}
        </div>
      </form>
      )}
    </section>
  )
}

/** The items of the conversation, with consecutive actions in one list. */
function render(items: ChatItem[]): ReactNode[] {
  const out: ReactNode[] = []
  let actions: ChatItem[] = []
  const flush = () => {
    if (actions.length === 0) return
    out.push(
      <li key={`actions-${actions[0].id}`} className="chat-actions">
        <ul aria-label="What the assistant did">
          {actions.map((action) => (
            <Action key={action.id} item={action} />
          ))}
        </ul>
      </li>,
    )
    actions = []
  }
  for (const item of items) {
    if (item.kind === 'activity') {
      actions.push(item)
      continue
    }
    // Items with nothing to show yet do not split a list of actions.
    const content = itemContent(item)
    if (content) {
      flush()
      out.push(
        <li key={item.id} className={`chat-item chat-item-${item.kind}`}>
          {content}
        </li>,
      )
    }
  }
  flush()
  return out
}

function itemContent(item: ChatItem): ReactNode {
  switch (item.kind) {
    case 'person':
      return <div className="chat-message-person">{item.text}</div>
    case 'answer':
      return item.text ? <Answer text={item.text} /> : null
    case 'thinking':
      return item.text ? (
        <details className="chat-thinking">
          <summary>
            {item.seq > 0 ? 'Thinking' : 'Thought'}
            <Icon name="chevronRight" size={13} strokeWidth={2} />
          </summary>
          <p>{item.text}</p>
        </details>
      ) : null
    case 'permission':
      return <PermissionCard item={item} />
    case 'question':
      return <QuestionCard item={item} />
    case 'notice':
    case 'error':
      return (
        <div className={`chat-note chat-note-${item.kind}`} role={item.kind === 'error' ? 'alert' : undefined}>
          <p>{item.text.charAt(0).toUpperCase() + item.text.slice(1)}</p>
          {item.detail && <p className="chat-note-detail">{item.detail}</p>}
        </div>
      )
    default:
      return null
  }
}

function Answer({ text }: { text: string }) {
  return (
    <div className="chat-answer">
      <Markdown
        remarkPlugins={[remarkGfm]}
        components={{
          a: ({ node: _node, ...props }) => <a {...props} target="_blank" rel="noreferrer" />,
          table: ({ node: _node, ...props }) => (
            <div className="chat-table">
              <table {...props} />
            </div>
          ),
        }}
      >
        {text}
      </Markdown>
    </div>
  )
}

function Action({ item }: { item: ChatItem }) {
  const icon =
    item.state === 'running' ? (
      <Working size={15} />
    ) : item.state === 'failed' ? (
      <span className="chat-action-icon chat-action-failed" aria-label="Failed" role="img">
        <Icon name="circleX" size={15} strokeWidth={1.75} />
      </span>
    ) : item.state === 'stopped' ? (
      <span className="chat-action-icon" aria-label="Stopped" role="img">
        <Icon name="circleMinus" size={15} strokeWidth={1.75} />
      </span>
    ) : (
      <span className="chat-action-icon" aria-label="Done" role="img">
        <Icon name="circleCheck" size={15} strokeWidth={1.75} />
      </span>
    )
  return (
    <li className="chat-action">
      {icon}
      <span className="chat-action-text">
        <span className="chat-action-label">{item.text}</span>
        {item.detail && (
          <span className="chat-action-detail" title={item.detail}>
            {item.detail}
          </span>
        )}
      </span>
    </li>
  )
}

/** The assistant in use, with the way to disconnect its account. */
function AssistantMenu({
  account,
  onDisconnected,
}: Pick<ChatProps, 'account' | 'onDisconnected'>) {
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

type TextSize = 'small' | 'medium' | 'large'
const textSizes: { value: TextSize; label: string; sample: number }[] = [
  { value: 'small', label: 'Small', sample: 12 },
  { value: 'medium', label: 'Medium', sample: 15 },
  { value: 'large', label: 'Large', sample: 18 },
]
const TEXT_SIZE_KEY = 'axioma.chat.text-size'

/** The size of the chat's text, remembered by this browser. */
function useTextSize(): [TextSize, (size: TextSize) => void] {
  const [size, setSize] = useState<TextSize>(() => {
    try {
      const saved = localStorage.getItem(TEXT_SIZE_KEY)
      return textSizes.some((s) => s.value === saved) ? (saved as TextSize) : 'medium'
    } catch {
      return 'medium'
    }
  })
  const change = (next: TextSize) => {
    setSize(next)
    try {
      localStorage.setItem(TEXT_SIZE_KEY, next)
    } catch {
      // Not remembered: the choice lasts until the page is closed.
    }
  }
  return [size, change]
}

/** Settings of the chat, for this browser. */
function ChatSettings({
  textSize,
  onTextSize,
}: {
  textSize: TextSize
  onTextSize: (size: TextSize) => void
}) {
  const [open, setOpen] = useState(false)
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

  return (
    <div className="assistant" ref={root}>
      <button
        className="icon-button"
        aria-label="Chat settings"
        title="Chat settings"
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        <Icon name="settings" size={18} />
      </button>
      {open && (
        <div className="assistant-menu chat-settings" role="dialog" aria-label="Chat settings">
          <span className="chat-settings-label" id="chat-text-size">
            Text size
          </span>
          <div className="chat-sizes" role="radiogroup" aria-labelledby="chat-text-size">
            {textSizes.map((size) => (
              <button
                key={size.value}
                type="button"
                role="radio"
                aria-checked={textSize === size.value}
                aria-label={size.label}
                title={size.label}
                className="chat-size"
                onClick={() => onTextSize(size.value)}
              >
                <span style={{ fontSize: size.sample }}>A</span>
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  )
}

/** A permission request: waiting for a person, or how it was answered. */
function PermissionCard({ item }: { item: ChatItem }) {
  const [busy, setBusy] = useState(false)
  const canAct = useContext(CanAct)
  const [details, setDetails] = useState(false)
  const [problem, setProblem] = useState<string | null>(null)

  if (item.state !== 'waiting') {
    const allowed = item.state === 'allowed'
    return (
      <div className="chat-decision">
        <span className="chat-decision-title">{item.text}</span>
        <span aria-hidden="true">·</span>
        <span className={allowed ? 'chat-decision-yes' : undefined}>
          {allowed ? 'Allowed' : 'Not allowed'}
          {item.author ? ` by ${item.author}` : ''}
        </span>
      </div>
    )
  }

  const answer = async (allow: boolean) => {
    setBusy(true)
    setProblem(null)
    try {
      await chatApi.answerPermission(item.id, allow)
    } catch (err) {
      setProblem(err instanceof ApiError ? err.message : String(err))
      setBusy(false)
    }
  }

  return (
    <section className="chat-request" aria-label="The assistant asks permission">
      <div className="chat-request-head">
        <WaitingMark />
        <span className="chat-request-title">{item.text}</span>
      </div>
      <div className="chat-request-actions">
        <button type="button" className="chat-request-yes" onClick={() => answer(true)} disabled={busy || !canAct}>
          Allow
        </button>
        <button type="button" className="chat-request-no" onClick={() => answer(false)} disabled={busy || !canAct}>
          Don't allow
        </button>
      </div>
      {item.detail && (
        <>
          <button
            type="button"
            className="chat-request-more"
            aria-expanded={details}
            onClick={() => setDetails((open) => !open)}
          >
            {details ? 'Hide the details' : 'Show the details'}
            <Icon name="chevronDown" size={13} strokeWidth={2} />
          </button>
          {details && <pre className="chat-request-detail">{item.detail}</pre>}
        </>
      )}
      {problem && (
        <p className="chat-request-error" role="alert">
          {problem.charAt(0).toUpperCase() + problem.slice(1)}
        </p>
      )}
    </section>
  )
}

function questionsOf(item: ChatItem): ChatQuestions | null {
  try {
    const parsed = JSON.parse(item.detail ?? '') as ChatQuestions
    return Array.isArray(parsed.questions) && parsed.questions.length > 0 ? parsed : null
  } catch {
    return null
  }
}

/** Questions of the assistant, one at a time, then how they were answered. */
function QuestionCard({ item }: { item: ChatItem }) {
  const parsed = questionsOf(item)
  const canAct = useContext(CanAct)
  const count = parsed?.questions.length ?? 0
  const [step, setStep] = useState(0)
  const [answers, setAnswers] = useState<QuestionAnswer[]>(() =>
    Array.from({ length: count }, () => ({ choices: [], other: '' })),
  )
  const [writing, setWriting] = useState(false)
  const [busy, setBusy] = useState(false)
  const [problem, setProblem] = useState<string | null>(null)

  if (!parsed) return null

  if (item.state !== 'waiting') {
    return (
      <div className="chat-answers">
        {parsed.questions.map((question, index) => (
          <p key={index} className="chat-answers-line">
            <span className="chat-answers-question">{question.question}</span>
            <span className="chat-answers-answer">
              {parsed.answers?.[index] ?? 'Not answered'}
            </span>
          </p>
        ))}
        {item.author && <p className="chat-answers-by">Answered by {item.author}</p>}
      </div>
    )
  }

  const question = parsed.questions[step]
  const current = answers[step]
  const last = step === count - 1

  const submit = async (all: QuestionAnswer[]) => {
    setBusy(true)
    setProblem(null)
    try {
      await chatApi.answerQuestions(
        item.id,
        all.map((answer) => ({
          choices: answer.choices,
          other: answer.other?.trim() ? answer.other.trim() : undefined,
        })),
      )
    } catch (err) {
      setProblem(err instanceof ApiError ? err.message : String(err))
      setBusy(false)
    }
  }

  const update = (answer: QuestionAnswer) => {
    const next = answers.map((existing, index) => (index === step ? answer : existing))
    setAnswers(next)
    return next
  }

  // An answer is complete: the next question, or the answers go.
  const advance = (next: QuestionAnswer[]) => {
    setWriting(false)
    if (last) submit(next)
    else setStep(step + 1)
  }

  const choose = (label: string) => {
    if (question.multiple) {
      const choices = current.choices.includes(label)
        ? current.choices.filter((choice) => choice !== label)
        : [...current.choices, label]
      update({ ...current, choices })
    } else {
      advance(update({ choices: [label], other: '' }))
    }
  }

  const answered = current.choices.length > 0 || Boolean(current.other?.trim())

  return (
    <section className="chat-request" aria-label="The assistant asks">
      <div className="chat-request-head">
        <WaitingMark />
        <span className="chat-request-title">{question.question}</span>
        {count > 1 && (
          <span className="chat-request-step">
            {step + 1} of {count}
          </span>
        )}
      </div>

      <div
        className="chat-options"
        role={question.multiple ? 'group' : undefined}
        aria-label={question.multiple ? 'Choose one or more' : undefined}
      >
        {question.options.map((option) => {
          const chosen = current.choices.includes(option.label)
          return (
            <button
              key={option.label}
              type="button"
              className="chat-option"
              aria-pressed={question.multiple ? chosen : undefined}
              onClick={() => choose(option.label)}
              disabled={busy || !canAct}
            >
              {question.multiple && (
                <span className="chat-option-box" aria-hidden="true">
                  {chosen && <Icon name="check" size={13} strokeWidth={2.25} />}
                </span>
              )}
              <span className="chat-option-text">
                <span>{option.label}</span>
                {option.description && (
                  <span className="chat-option-detail">{option.description}</span>
                )}
              </span>
            </button>
          )
        })}
      </div>

      {writing ? (
        <form
          className="chat-other"
          onSubmit={(event) => {
            event.preventDefault()
            if (answered) advance(answers)
          }}
        >
          <label htmlFor={`other-${item.id}`} className="visually-hidden">
            Your answer
          </label>
          <input
            id={`other-${item.id}`}
            autoFocus
            placeholder="Write your answer"
            value={current.other ?? ''}
            onChange={(event) =>
              update({
                choices: question.multiple ? current.choices : [],
                other: event.target.value,
              })
            }
            disabled={busy}
          />
          <button type="submit" className="chat-request-yes" disabled={busy || !answered}>
            {last ? 'Send' : 'Next'}
          </button>
        </form>
      ) : (
        <div className="chat-request-foot">
          {step > 0 && (
            <button type="button" className="chat-request-more" onClick={() => setStep(step - 1)} disabled={busy}>
              Back
            </button>
          )}
          <button type="button" className="chat-request-more" onClick={() => setWriting(true)} disabled={busy || !canAct}>
            Answer in my own words
          </button>
          {question.multiple && (
            <button
              type="button"
              className="chat-request-yes chat-request-done"
              onClick={() => advance(answers)}
              disabled={busy || !answered}
            >
              {last ? 'Done' : 'Next'}
            </button>
          )}
        </div>
      )}

      {problem && (
        <p className="chat-request-error" role="alert">
          {problem.charAt(0).toUpperCase() + problem.slice(1)}
        </p>
      )}
    </section>
  )
}
