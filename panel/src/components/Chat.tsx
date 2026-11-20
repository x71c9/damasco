import {
  createContext,
  lazy,
  Suspense,
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
  terminal as terminalApi,
  type AgentAccount,
  type AssistantModel,
  type ConversationSummary,
  type ChatConversation,
  type ChatEvent,
  type ChatHolder,
  type ChatProblem,
  type ChatItem,
  type ChatQuestions,
  type QuestionAnswer,
} from '../api'
import { Dialog, DialogActions } from './Dialog'
import { Icon, Mark, WaitingMark, Working } from './icons'
import { Button } from './Button'
import { goTo } from '../navigation'
import { connectedWith } from '../settings/account'
import { textOf } from '../assistants'
import { sentence } from '../text'
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
  terminal: { id: number; name: string } | null
  problem: ChatProblem | null
  /** The mode of the assistant's CLI for the next message. */
  mode: string | null
  /** The model for the next message; the CLI's default if null. */
  model: string | null
}

const empty: ChatState = {
  conversation: null,
  items: [],
  turn: null,
  holder: null,
  terminal: null,
  problem: null,
  mode: null,
  model: null,
}

function holderOf(holder: ChatHolder | null): ChatState['holder'] {
  if (!holder) return null
  return {
    id: holder.id,
    name: holder.name,
    until: holder.remaining_ms === null ? null : Date.now() + holder.remaining_ms,
  }
}

// The terminal and xterm.js load only when the terminal opens.
const TerminalView = lazy(() =>
  import('./TerminalView').then((module) => ({ default: module.TerminalView })),
)

/** Whether the signed-in person can act on the chat: answer, stop, write. */
const CanAct = createContext(true)

/** Going back to before a message, and undoing it; null while the
 * assistant or the terminal is at work. */
const Versions = createContext<{
  goBack: (item: ChatItem) => void
  undo: (item: ChatItem) => void
} | null>(null)

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
        terminal: event.terminal,
        problem: event.problem,
        mode: event.mode,
        model: event.model,
      }
    case 'mode':
      return { ...state, mode: event.mode }
    case 'model':
      return { ...state, model: event.model }
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
        terminal: event.terminal,
        problem: event.problem,
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
      source = new EventSource(chatApi.eventsUrl())
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
  const [{ conversation, items, turn, holder, terminal, problem: trouble, mode, model }, connected] =
    useChatEvents()
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
  const canAct = !heldBy && !terminal
  // Why the controls of the chat wait, told on each of them.
  const waitReason = turn
    ? 'Available when the assistant has finished'
    : heldBy
      ? `${heldBy.name} is using the assistant`
      : terminal
        ? 'Available when the terminal is closed'
        : undefined
  // The terminal runs the assistant's own CLI: not every assistant has it.
  const withTerminal =
    account.assistants?.find((a) => a.provider === account.provider)?.capabilities.terminal ?? true
  // The plan used up: the chat waits until the time it is available again.
  const [limitOver, setLimitOver] = useState<number | null>(null)
  const limitUntil = trouble?.kind === 'limit' && trouble.until ? trouble.until * 1000 : null
  useEffect(() => {
    if (limitUntil === null) return
    const timer = setTimeout(
      () => setLimitOver(limitUntil),
      Math.max(0, limitUntil - Date.now()) + 500,
    )
    return () => clearTimeout(timer)
  }, [limitUntil])
  const waitingLimit = limitUntil !== null && limitOver !== limitUntil
  const signedOut = trouble?.kind === 'login'

  // The terminal takes the place of the conversation, on the same session.
  const [view, setView] = useState<'chat' | 'terminal'>('chat')
  // The list of past conversations, in place of the current one.
  const [history, setHistory] = useState(false)
  const [closing, setClosing] = useState(false)
  const [confirmStop, setConfirmStop] = useState(false)
  const lastOutput = useRef(0)
  const mine = terminal?.id === me
  // An open terminal of this person shows in place of the chat, also after
  // a reload, until it is being closed.
  const shownView = view === 'terminal' || (mine && !closing) ? 'terminal' : 'chat'
  const closeTerminal = async () => {
    setConfirmStop(false)
    setClosing(true)
    try {
      await terminalApi.close()
    } catch (err) {
      setProblem(err instanceof ApiError ? err.message : String(err))
    }
    setView('chat')
  }
  const toggleTerminal = (at: number) => {
    if (shownView === 'chat') {
      setProblem(null)
      setClosing(false)
      setView('terminal')
    } else if (at - lastOutput.current < 3000) {
      // The assistant is still writing in the terminal.
      setConfirmStop(true)
    } else {
      closeTerminal()
    }
  }
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
  // The message box grows with its text, up to half the chat's height;
  // beyond that it scrolls.
  const input = useRef<HTMLTextAreaElement>(null)
  const fitInput = () => {
    const element = input.current
    if (!element) return
    const chat = element.closest('.chat')
    const limit = chat ? chat.clientHeight / 2 : Infinity
    element.style.height = 'auto'
    const needed = element.scrollHeight
    element.style.height = `${Math.min(needed, limit)}px`
    element.style.overflowY = needed > limit ? 'auto' : 'hidden'
  }
  useLayoutEffect(fitInput)
  useEffect(() => {
    const element = input.current
    const chat = element?.closest('.chat')
    if (!element || !chat) return
    // A narrower or shorter chat changes the lines and the limit. The
    // chat's own size does not follow the box, so this never loops.
    const observer = new ResizeObserver(fitInput)
    observer.observe(chat)
    return () => observer.disconnect()
  })
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
  // Back from the terminal, the conversation opens again at its end.
  useLayoutEffect(() => {
    if (shownView === 'chat') atBottom.current = true
  }, [shownView])
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
  }, [items, shownView])
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

  const retry = async () => {
    setProblem(null)
    try {
      await chatApi.retry()
      atBottom.current = true
    } catch (err) {
      setProblem(err instanceof ApiError ? err.message : String(err))
    }
  }

  // Signing in again: the account is let go, and the panel shows how to
  // connect it.
  const reconnect = async () => {
    setProblem(null)
    try {
      await agent.logout()
    } catch {
      // Already signed out: the state below says so.
    }
    onDisconnected(await agent.account())
  }

  // Going back to before a message: confirmed first; the message comes
  // back in the box.
  const [backTo, setBackTo] = useState<ChatItem | null>(null)
  const goBack = async () => {
    if (!backTo) return
    setProblem(null)
    try {
      const { text } = await chatApi.goBack(backTo.id)
      setDraft(text)
      document.getElementById('composer-input')?.focus()
    } catch (err) {
      setProblem(err instanceof ApiError ? err.message : String(err))
    }
    setBackTo(null)
  }
  const versions = {
    goBack: (item: ChatItem) => setBackTo(item),
    undo: (item: ChatItem) => {
      setProblem(null)
      chatApi.undoBack(item.id).catch((err) =>
        setProblem(err instanceof ApiError ? err.message : String(err)),
      )
    },
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
        <button
          type="button"
          className="icon-button"
          aria-label={history ? 'Back to the conversation' : 'Past conversations'}
          title={history ? 'Back to the conversation' : (waitReason ?? 'Past conversations')}
          aria-pressed={history}
          onClick={() => setHistory((value) => !value)}
          disabled={!history && (Boolean(turn) || !canAct || shownView === 'terminal')}
        >
          <Icon name="history" size={18} />
        </button>
        <Button
          variant="quiet"
          small
          icon="newChat"
          className="chat-new"
          title={waitReason ?? 'New conversation'}
          onClick={startNew}
          disabled={Boolean(turn) || !conversation || !canAct}
        >
          New
        </Button>
        <span className="composer-spacer" />
        <button
          className="icon-button"
          aria-label={
            shownView === 'terminal'
              ? 'Back to the chat'
              : withTerminal
                ? 'Open the terminal'
                : `The terminal is not available with ${textOf(account.provider).name} yet`
          }
          title={
            shownView === 'terminal'
              ? 'Back to the chat'
              : !withTerminal
                ? `The terminal is not available with ${textOf(account.provider).name} yet`
                : (waitReason ?? 'Open the terminal')
          }
          aria-pressed={shownView === 'terminal'}
          onClick={(event) => toggleTerminal(event.timeStamp)}
          disabled={shownView === 'chat' && (Boolean(turn) || !canAct || !withTerminal)}
        >
          <Icon name={shownView === 'terminal' ? 'chat' : 'terminal'} size={18} />
        </button>
        <ChatSettings textSize={textSize} onTextSize={setTextSize} />
      </header>

      {backTo && (
        <Dialog title="Go back to before this message?" onClose={() => setBackTo(null)}>
          <p className="dialog-text">
            What the assistant changed on the site after it is undone, as a new version that can
            be undone in turn. Your message goes back in the box so you can change it.
          </p>
          <DialogActions>
            <Button onClick={() => setBackTo(null)}>Cancel</Button>
            <Button variant="primary" onClick={goBack}>
              Go back
            </Button>
          </DialogActions>
        </Dialog>
      )}

      {confirmStop && (
        <Dialog title="Stop the terminal?" onClose={() => setConfirmStop(false)}>
          <p className="dialog-text">
            The assistant is still working in the terminal. Going back to the chat stops it; the
            changes made so far are kept.
          </p>
          <DialogActions>
            <Button onClick={() => setConfirmStop(false)}>
              Keep working
            </Button>
            <Button variant="danger" onClick={closeTerminal}>
              Stop and go back
            </Button>
          </DialogActions>
        </Dialog>
      )}

      {history && shownView === 'chat' ? (
        <ConversationList provider={account.provider} onOpened={() => setHistory(false)} />
      ) : shownView === 'terminal' ? (
        <Suspense fallback={<p className="chat-loading">Opening the terminal…</p>}>
        <TerminalView
          fontSize={terminalFontSizes[textSize]}
          onOutput={(at) => {
            lastOutput.current = at
          }}
          onEnded={(refused) => {
            // Stays closing until the server says the terminal is gone.
            setView('chat')
            setClosing(true)
            if (refused) {
              setProblem('The terminal cannot open now: the assistant is working, or someone else is using it.')
            }
          }}
        />
        </Suspense>
      ) : (
        <>

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
          <Versions.Provider value={canAct && !turn ? versions : null}>
            <ol className="chat-messages">
              {render(items)}
              {trouble && !turn && (
                <li className="chat-item">
                  <ProblemCard
                    name={textOf(account.provider).name}
                    problem={trouble}
                    onRetry={retry}
                    onNew={startNew}
                    onReconnect={reconnect}
                  />
                </li>
              )}
            </ol>
          </Versions.Provider>
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

      {terminal ? (
        <div className="chat-held chat-paused" role="status">
          <span>
            {terminal.id === me ? (
              <>Your terminal is open.</>
            ) : (
              <>
                <strong>{terminal.name}</strong> is using the terminal.
              </>
            )}
          </span>
          {terminal.id === me && (
            <Button onClick={() => {
                setClosing(false)
                setView('terminal')
              }}
            >
              Back to the terminal
            </Button>
          )}
        </div>
      ) : waitingLimit || signedOut ? (
        <div className="chat-held" role="status">
          <span>
            {signedOut
              ? `Reconnect ${textOf(account.provider).name} to write.`
              : `Back at ${backAt(limitUntil ?? 0)}.`}
          </span>
        </div>
      ) : heldBy ? (
        <div className="chat-held" role="status">
          <span>
            <strong>{heldBy.name}</strong> is using the assistant.
          </span>
          {!turn && (
            <Button onClick={takeOver}>
              Use it now
            </Button>
          )}
        </div>
      ) : (
      <form className="composer" onSubmit={send}>
        <label htmlFor="composer-input" className="visually-hidden">
          Message to the assistant
        </label>
        <textarea
          ref={input}
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
          <ModeMenu account={account} mode={mode} disabled={Boolean(turn) || !canAct} reason={waitReason} />
          {/* Another assistant has other models: the menu starts again. */}
          <ModelMenu key={account.provider} model={model} disabled={Boolean(turn) || !canAct} reason={waitReason} />
          <span className="composer-spacer" />
          <AssistantMenu account={account} />
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
        </>
      )}
    </section>
  )
}

/** The items of the conversation, with consecutive actions in one list.
 * The assistant's buttons into the panel close its reply: they come after
 * its last words, before the next message of a person. */
function render(items: ChatItem[]): ReactNode[] {
  const out: ReactNode[] = []
  let actions: ChatItem[] = []
  let links: ChatItem[] = []
  const flushLinks = () => {
    for (const link of links) {
      out.push(
        <li key={link.id} className="chat-item chat-item-link">
          {itemContent(link)}
        </li>,
      )
    }
    links = []
  }
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
    if (item.kind === 'link') {
      links.push(item)
      continue
    }
    if (item.kind === 'person') {
      flush()
      flushLinks()
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
  flushLinks()
  return out
}

function itemContent(item: ChatItem): ReactNode {
  switch (item.kind) {
    case 'person':
      return <PersonMessage item={item} />
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
    case 'link':
      return <PanelLink item={item} />
    case 'notice':
      if (item.back) return <WentBack item={item} />
      return (
        <div className="chat-note chat-note-notice">
          <p>{item.text.charAt(0).toUpperCase() + item.text.slice(1)}</p>
          {item.detail && <p className="chat-note-detail">{item.detail}</p>}
        </div>
      )
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
        <span className="chat-action-label" title={item.text}>
          {item.text}
        </span>
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
/** Which assistant answers and how it is connected; changing it is in
 * Settings. */
function AssistantMenu({ account }: Pick<ChatProps, 'account'>) {
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
        type="button"
        className="composer-assistant"
        aria-label={`Assistant: ${textOf(account.provider).name}`}
        title={textOf(account.provider).name}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        <span className="composer-pill-text">{textOf(account.provider).name}</span>
        <Icon name="chevronDown" size={12} strokeWidth={2} />
      </button>
      {open && (
        <div className="assistant-menu" role="menu" aria-label="Assistant">
          <div className="assistant-who">
            <span className="assistant-name">{textOf(account.provider).name}</span>
            <span className="assistant-meta">{connectedWith(account)}</span>
          </div>
          <button
            type="button"
            className="assistant-item"
            role="menuitem"
            onClick={() => {
              setOpen(false)
              goTo('/settings/assistant')
            }}
          >
            Manage in Settings
          </button>
        </div>
      )}
    </div>
  )
}

const day = new Intl.DateTimeFormat(undefined, { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' })

/** The past conversations, newest first: one of the assistant in use opens
 * again, and the next message continues it. */
function ConversationList({ provider, onOpened }: { provider: string; onOpened: () => void }) {
  const [list, setList] = useState<ConversationSummary[] | null>(null)
  const [problem, setProblem] = useState<string | null>(null)

  useEffect(() => {
    chatApi.conversations().then(
      (result) => setList(result.conversations),
      (err: unknown) => {
        setList([])
        setProblem(err instanceof Error ? sentence(err.message) : String(err))
      },
    )
  }, [])

  const open = async (item: ConversationSummary) => {
    if (item.current) {
      onOpened()
      return
    }
    try {
      await chatApi.openConversation(item.id)
      onOpened()
    } catch (err) {
      setProblem(err instanceof Error ? sentence(err.message) : String(err))
    }
  }

  return (
    <section className="chat-history" aria-label="Past conversations">
      <h2 className="chat-history-title">Past conversations</h2>
      {problem && (
        <p className="chat-history-problem" role="alert">
          {problem}
        </p>
      )}
      {list === null && <p className="chat-history-empty">Reading the conversations…</p>}
      {list?.length === 0 && !problem && <p className="chat-history-empty">No conversations yet.</p>}
      <ul className="chat-history-list">
        {list?.map((item) => {
          const other = item.provider !== provider
          const name = textOf(item.provider).name
          return (
            <li key={item.id}>
              <button
                type="button"
                className="chat-history-item"
                aria-current={item.current || undefined}
                disabled={other}
                title={other ? `With ${name}: connect ${name} to continue it` : undefined}
                onClick={() => open(item)}
              >
                <span className="chat-history-name">{item.title}</span>
                <span className="chat-history-meta">
                  {day.format(new Date(item.updated_at))} · {item.messages}{' '}
                  {item.messages === 1 ? 'message' : 'messages'}
                  {other && ` · with ${name}`}
                </span>
              </button>
            </li>
          )
        })}
      </ul>
    </section>
  )
}

/** The mode of the assistant's CLI for the next messages: each assistant
 * has its own, as its CLI names them. */
function ModeMenu({
  account,
  mode,
  disabled,
  reason,
}: {
  account: AgentAccount
  mode: string | null
  disabled: boolean
  /** Why it waits, when it does. */
  reason?: string
}) {
  const [open, setOpen] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const root = useRef<HTMLDivElement>(null)
  const text = textOf(account.provider)
  const modes =
    account.assistants?.find((a) => a.provider === account.provider)?.modes ?? Object.keys(text.modes)
  const current = mode ?? modes[0]
  const label = (id: string) => text.modes[id]?.label ?? id

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

  const choose = async (next: string) => {
    setOpen(false)
    setError(null)
    try {
      await chatApi.setMode(next)
    } catch (err) {
      setError(err instanceof Error ? sentence(err.message) : String(err))
    }
  }

  return (
    <div className="assistant mode" ref={root}>
      <button
        type="button"
        className="composer-pill composer-pill-mode"
        aria-label={`Mode: ${label(current)}. Change mode`}
        title={
          disabled && reason
            ? reason
            : (error ?? [label(current), text.modes[current]?.detail].filter(Boolean).join(': '))
        }
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={disabled}
        onClick={() => setOpen((value) => !value)}
      >
        <Icon name={text.modes[current]?.icon ?? 'bolt'} size={14} />
        <span className="composer-pill-text">{label(current)}</span>
        <Icon name="chevronDown" size={12} strokeWidth={2} />
      </button>
      {open && (
        <div className="assistant-menu mode-menu" role="menu" aria-label="Mode">
          {modes.map((id) => (
            <button
              key={id}
              type="button"
              className="assistant-item mode-item"
              role="menuitemradio"
              aria-checked={id === current}
              onClick={() => choose(id)}
            >
              <span className="mode-label">
                <Icon name={text.modes[id]?.icon ?? 'bolt'} size={14} />
                {label(id)}
              </span>
              <span className="assistant-meta">{text.modes[id]?.detail}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  )
}

/** Claude's `opus` and `opus[1m]` are one model with two context sizes: the
 * list keeps only one of them (claude.rs), so a chat that chose the other
 * still finds its name. */
function sameModel(a: string, b: string) {
  const bare = (id: string) => id.replace(/\[1m\]$/, '')
  return bare(a) === bare(b)
}

/** The model for the next messages: the models the assistant's CLI offers,
 * or its default. The list is asked for when the menu first opens. */
function ModelMenu({
  model,
  disabled,
  reason,
}: {
  model: string | null
  disabled: boolean
  /** Why it waits, when it does. */
  reason?: string
}) {
  const [open, setOpen] = useState(false)
  const [models, setModels] = useState<AssistantModel[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const root = useRef<HTMLDivElement>(null)

  // The list names the model in use, also before the menu opens.
  useEffect(() => {
    let done = false
    agent.models().then(
      (result) => {
        if (!done) setModels(result.models)
      },
      (err: unknown) => {
        if (done) return
        setModels([])
        setError(err instanceof Error ? sentence(err.message) : String(err))
      },
    )
    return () => {
      done = true
    }
  }, [])

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

  const fallback = models?.find((m) => m.default)
  const name = model
    ? ((models?.find((m) => m.id === model) ?? models?.find((m) => sameModel(m.id, model)))?.name ?? model)
    : (fallback?.name ?? 'Default model')

  const choose = async (next: string | null) => {
    setOpen(false)
    setError(null)
    try {
      await chatApi.setModel(next)
    } catch (err) {
      setError(err instanceof Error ? sentence(err.message) : String(err))
    }
  }

  return (
    <div className="assistant mode composer-model" ref={root}>
      <button
        type="button"
        className="composer-pill"
        aria-label={`Model: ${name}. Change model`}
        title={disabled && reason ? reason : (error ?? name)}
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={disabled}
        onClick={() => setOpen((value) => !value)}
      >
        <span className="composer-pill-text">{name}</span>
        <Icon name="chevronDown" size={12} strokeWidth={2} />
      </button>
      {open && (
        <div className="assistant-menu mode-menu" role="menu" aria-label="Model">
          <button
            type="button"
            className="assistant-item mode-item"
            role="menuitemradio"
            aria-checked={model === null}
            onClick={() => choose(null)}
          >
            <span className="mode-label">Default model</span>
            <span className="assistant-meta">
              {fallback ? `Now ${fallback.name}, as the assistant chooses` : 'As the assistant chooses'}
            </span>
          </button>
          {models === null && <p className="assistant-meta mode-wait">Reading the models…</p>}
          {error && <p className="assistant-meta mode-wait">{error}</p>}
          {models?.map((item) => (
            <button
              key={item.id}
              type="button"
              className="assistant-item mode-item"
              role="menuitemradio"
              aria-checked={model !== null && sameModel(item.id, model)}
              onClick={() => choose(item.id)}
            >
              <span className="mode-label">
                {item.name}
              </span>
              {item.detail && <span className="assistant-meta">{item.detail}</span>}
            </button>
          ))}
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
const TEXT_SIZE_KEY = 'alogeno.chat.text-size'
/** The terminal's text for each size of the chat's text. */
const terminalFontSizes: Record<TextSize, number> = { small: 11.5, medium: 13.5, large: 16 }

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

/** Why the last turn did not finish, and the way on. */
function ProblemCard({
  name,
  problem,
  onRetry,
  onNew,
  onReconnect,
}: {
  /** The assistant's name. */
  name: string
  problem: ChatProblem
  onRetry: () => void
  onNew: () => void
  onReconnect: () => void
}) {
  const canAct = useContext(CanAct)
  const until = problem.until ? backAt(problem.until * 1000) : null
  const [title, text] =
    problem.kind === 'limit'
      ? [
          `The ${name} plan is used up for now`,
          until
            ? `Available again at ${until}. The work so far is saved.`
            : 'Available again later. The work so far is saved.',
        ]
      : problem.kind === 'login'
        ? [`${name} is disconnected`, 'Sign in again to keep working. It takes a minute.']
        : ['Something went wrong', 'The assistant could not finish. Nothing was lost.']
  return (
    <section className="chat-request chat-problem" aria-label={title}>
      <div className="chat-request-head">
        <span className="chat-problem-icon" aria-hidden="true">
          <Icon name={problem.kind === 'limit' ? 'history' : 'circleX'} size={20} strokeWidth={1.75} />
        </span>
        <span className="chat-problem-text">
          <span className="chat-request-title">{title}</span>
          <span className="chat-problem-detail">{text}</span>
        </span>
      </div>
      {problem.kind === 'error' && (
        <div className="chat-request-actions">
          <Button variant="primary" onClick={onRetry} disabled={!canAct}>
            Try again
          </Button>
          <Button onClick={onNew} disabled={!canAct}>
            Start a new conversation
          </Button>
        </div>
      )}
      {problem.kind === 'login' && (
        <div className="chat-request-actions">
          <Button variant="primary" onClick={onReconnect} disabled={!canAct}>
            Reconnect
          </Button>
        </div>
      )}
    </section>
  )
}

/** A message of a person, with the way back to before it. */
function PersonMessage({ item }: { item: ChatItem }) {
  const versions = useContext(Versions)
  return (
    <div className="chat-person">
      <div className="chat-message-person">{item.text}</div>
      {item.back && versions && (
        <button type="button" className="chat-back" onClick={() => versions.goBack(item)}>
          <Icon name="history" size={14} />
          Go back to here
        </button>
      )}
    </div>
  )
}

/** The line of a going back, with the way to undo it. */
const USED_LINKS_KEY = 'alogeno.chat.used-links'

/** The buttons into the panel this browser has already followed. */
function usedLinks(): Set<number> {
  try {
    const saved: unknown = JSON.parse(localStorage.getItem(USED_LINKS_KEY) ?? '[]')
    return new Set(Array.isArray(saved) ? saved.filter((id) => typeof id === 'number') : [])
  } catch {
    return new Set()
  }
}

function markLinkUsed(id: number) {
  try {
    // The newest few hundred are enough to remember.
    const ids = [...usedLinks(), id].slice(-300)
    localStorage.setItem(USED_LINKS_KEY, JSON.stringify(ids))
  } catch {
    // Not remembered: the button stays as it was.
  }
}

/** A button of the assistant that opens a place of the panel: a section,
 * a page and its field, a picture. Once followed it turns quiet, so the
 * next one stands out. */
function PanelLink({ item }: { item: ChatItem }) {
  const [used, setUsed] = useState(() => usedLinks().has(item.id))
  let place: { href?: string; place?: string } = {}
  try {
    place = JSON.parse(item.detail ?? '{}') as typeof place
  } catch {
    // Without a place the button has nowhere to go.
  }
  if (!place.href) return null
  const href = place.href
  return (
    <div className="chat-link">
      <Button
        small
        variant={used ? 'secondary' : 'accent'}
        onClick={() => {
          markLinkUsed(item.id)
          setUsed(true)
          goTo(href)
        }}
      >
        {item.text}
      </Button>
      {place.place && <span className="chat-link-place">{place.place}</span>}
    </div>
  )
}

function WentBack({ item }: { item: ChatItem }) {
  const versions = useContext(Versions)
  return (
    <div className="chat-note chat-note-notice chat-went-back">
      <p>{item.text}</p>
      {versions && (
        <button type="button" className="chat-back" onClick={() => versions.undo(item)}>
          Undo
        </button>
      )}
    </div>
  )
}

/** When a used-up plan is available again: the time today, or the day
 * and the time when it is later. */
function backAt(at: number): string {
  const date = new Date(at)
  const time = date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  if (date.toDateString() === new Date().toDateString()) return time
  return `${date.toLocaleDateString([], { weekday: 'long', day: 'numeric', month: 'short' })}, ${time}`
}
