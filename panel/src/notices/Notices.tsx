import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { Button } from '../components/Button'
import { Icon, Mark, Working } from '../components/icons'
import { NoticesContext, type Notice, type NoticeAction, type NoticeKind } from './context'
import './Notices.css'

/** Notifications shown at once; older ones wait. */
const SHOWN = 3
/** How long a notification about a result stays. */
const LIFETIME = 6000

/** The mark of a notice. */
function NoticeMark({ kind }: { kind: NoticeKind }) {
  if (kind === 'working') return <Working size={24} label="Under way" />
  if (kind === 'person') return <Icon name="user" size={24} strokeWidth={1.75} />
  return <Mark size={24} className={kind === 'problem' ? 'notice-mark-outline' : undefined} />
}

function ActionLink({ action, className, after }: { action: NoticeAction; className: string; after?: () => void }) {
  if (action.href) {
    return (
      <a className={className} href={action.href} target="_blank" rel="noreferrer" onClick={after}>
        {action.label}
      </a>
    )
  }
  return (
    <button
      type="button"
      className={className}
      onClick={() => {
        action.run?.()
        after?.()
      }}
    >
      {action.label}
    </button>
  )
}

/** One notification or the global notice: opposite colours to the theme. */
function Toast({ notice, onClose, wide = false }: { notice: Notice; onClose?: () => void; wide?: boolean }) {
  return (
    <div className={`toast${wide ? ' toast-wide' : ''}`} role={notice.kind === 'problem' ? 'alert' : 'status'}>
      <span className="toast-mark">
        <NoticeMark kind={notice.kind} />
      </span>
      <span className="toast-text">
        <span className="toast-title">{notice.title}</span>
        {notice.text && <span className="toast-detail">{notice.text}</span>}
        {notice.action && <ActionLink action={notice.action} className="toast-action" after={onClose} />}
      </span>
      {onClose && (
        <button type="button" className="toast-close" aria-label="Close" onClick={onClose}>
          <Icon name="close" size={16} strokeWidth={2} />
        </button>
      )}
    </div>
  )
}

interface NoticesProviderProps {
  /** A notice about the whole panel, at the top: stays until it is solved. */
  banner: Notice | null
  openChat: () => void
  children: ReactNode
}

/** The notifications of the panel and the notice about the whole panel,
 * together at the top, centred. */
export function NoticesProvider({ banner, openChat, children }: NoticesProviderProps) {
  const [toasts, setToasts] = useState<{ id: number; notice: Notice }[]>([])
  const next = useRef(1)
  const timers = useRef(new Map<number, number>())

  const dismiss = useCallback((id: number) => {
    window.clearTimeout(timers.current.get(id))
    timers.current.delete(id)
    setToasts((current) => current.filter((toast) => toast.id !== id))
  }, [])
  // Results close by themselves; errors and requests wait to be closed.
  const schedule = useCallback(
    (id: number, notice: Notice) => {
      window.clearTimeout(timers.current.get(id))
      if (notice.sticky || notice.kind === 'problem' || notice.kind === 'working') return
      timers.current.set(id, window.setTimeout(() => dismiss(id), LIFETIME))
    },
    [dismiss],
  )
  const notify = useCallback(
    (notice: Notice) => {
      const id = next.current++
      setToasts((current) => [...current, { id, notice }])
      schedule(id, notice)
      return id
    },
    [schedule],
  )
  const update = useCallback(
    (id: number, notice: Notice) => {
      setToasts((current) => {
        const found = current.some((toast) => toast.id === id)
        return found
          ? current.map((toast) => (toast.id === id ? { id, notice } : toast))
          : [...current, { id, notice }]
      })
      schedule(id, notice)
    },
    [schedule],
  )
  useEffect(() => {
    const all = timers.current
    return () => all.forEach((timer) => window.clearTimeout(timer))
  }, [])

  const value = useMemo(() => ({ notify, update, dismiss, openChat }), [notify, update, dismiss, openChat])
  return (
    <NoticesContext.Provider value={value}>
      {children}
      {/* All at the top, centred: the notice about the whole panel first,
          then the notifications, newest last. */}
      <div className="notices" aria-live="polite">
        {banner && <Toast notice={banner} wide />}
        {toasts.slice(-SHOWN).map((toast) => (
          <Toast key={toast.id} notice={toast.notice} onClose={() => dismiss(toast.id)} />
        ))}
      </div>
    </NoticesContext.Provider>
  )
}

/** A notice at the top of a section, in the theme's colours: it stays as
 * long as the situation lasts. */
export function PageNotice({ notice }: { notice: Notice }) {
  return (
    <div className="page-notice" role={notice.kind === 'problem' ? 'alert' : 'status'}>
      <span className="page-notice-mark">
        <NoticeMark kind={notice.kind} />
      </span>
      <span className="page-notice-text">
        <span className="page-notice-title">{notice.title}</span>
        {notice.text && <span className="page-notice-detail">{notice.text}</span>}
      </span>
      {notice.action &&
        (notice.action.href ? (
          <a className="button button-secondary page-notice-action" href={notice.action.href} target="_blank" rel="noreferrer">
            {notice.action.label}
          </a>
        ) : (
          <Button className="page-notice-action" onClick={notice.action.run}>
            {notice.action.label}
          </Button>
        ))}
    </div>
  )
}
