import { createContext, useContext } from 'react'
import type { ChangeStatus, Changes, PendingItem } from '../api'

export interface ChangesValue {
  changes: Changes | null
  /** Reads the list again, after a save. */
  refresh: () => void
}

export const Context = createContext<ChangesValue>({ changes: null, refresh: () => {} })

export function useChanges(): ChangesValue {
  return useContext(Context)
}

/** The pending change of a page, a picture or a component, if any. */
export function usePending(kind: 'pages' | 'media' | 'components', id: string): PendingItem | null {
  const { changes } = useChanges()
  return changes?.pending[kind].find((item) => item.id === id) ?? null
}

const time = new Intl.DateTimeFormat(undefined, { hour: '2-digit', minute: '2-digit' })
const day = new Intl.DateTimeFormat(undefined, { day: 'numeric', month: 'short' })

/** "today at 14:02", "yesterday at 9:10", "15 Nov at 18:30". */
export function when(seconds: number): string {
  const date = new Date(seconds * 1000)
  const today = new Date()
  const yesterday = new Date(today)
  yesterday.setDate(today.getDate() - 1)
  const same = (a: Date, b: Date) => a.toDateString() === b.toDateString()
  const name = same(date, today) ? 'today' : same(date, yesterday) ? 'yesterday' : day.format(date)
  return `${name} at ${time.format(date)}`
}

export const labels: Record<ChangeStatus, string> = {
  created: 'New',
  changed: 'Changed',
  deleted: 'Deleted',
}

/** Who made the change and when: "by Anna today at 14:02", or only when
 * the person is not known (the site's first version). */
export function madeBy(item: PendingItem, people: Record<string, string> | undefined): string {
  const name = item.by === null ? null : (people?.[String(item.by)] ?? null)
  return name ? `by ${name} ${when(item.at)}` : when(item.at)
}

