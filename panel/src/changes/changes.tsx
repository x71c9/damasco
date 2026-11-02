import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react'
import { changes as api, type Changes, type PendingItem } from '../api'
import { Context, labels, madeBy, useChanges, usePending } from './context'
import './Changes.css'

/** What is not online yet, shared by the header, the sections and the
 * editor. It is read again each time `version` changes (a new build) and
 * after each save. */
export function ChangesProvider({ version, enabled, children }: { version: string; enabled: boolean; children: ReactNode }) {
  const [changes, setChanges] = useState<Changes | null>(null)
  // Only the newest answer counts when two reads overlap.
  const asked = useRef(0)
  const refresh = useCallback(() => {
    if (!enabled) return
    const ask = ++asked.current
    api.list().then(
      (result) => {
        if (ask === asked.current) setChanges(result)
      },
      () => {},
    )
  }, [enabled])
  useEffect(() => refresh(), [refresh, version])
  return <Context.Provider value={{ changes, refresh }}>{children}</Context.Provider>
}

/** The mark next to an item not online yet: "Changed · not online yet",
 * with who and when on hover and for screen readers. */
export function PendingMark({ item, compact = false }: { item: PendingItem; compact?: boolean }) {
  const { changes } = useChanges()
  const state = item.status === 'deleted' ? 'still online' : 'not online yet'
  const who = madeBy(item, changes?.people)
  return (
    <span className="pending-mark" title={`${labels[item.status]} ${who}`}>
      <span className="pending-dot" aria-hidden="true" />
      {compact ? labels[item.status] : `${labels[item.status]} · ${state}`}
      <span className="visually-hidden">{`, ${who}`}</span>
    </span>
  )
}

/** Whether a page or picture is online as it is here: "Changed by Anna
 * today at 14:02 · not online yet", or "Online". */
export function PendingState({ kind, id }: { kind: 'pages' | 'media'; id: string }) {
  const { changes } = useChanges()
  const item = usePending(kind, id)
  if (!changes) return null
  if (!item) {
    return <span className="pending-state">{changes.pending.published ? 'Online' : 'Not published yet'}</span>
  }
  const what = item.status === 'created' ? 'Created' : 'Changed'
  return (
    <span className="pending-state">
      <span className="pending-dot" aria-hidden="true" />
      {what} {madeBy(item, changes.people)} · not online yet
    </span>
  )
}
