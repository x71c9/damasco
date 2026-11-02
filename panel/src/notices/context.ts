import { createContext, useContext } from 'react'

/** What a notice is about, which picks its mark:
 * - done: the result of an action (the mark);
 * - working: something under way (the mark lighting up);
 * - problem: something went wrong or needs a hand (the mark in outline);
 * - person: someone else (a person). */
export type NoticeKind = 'done' | 'working' | 'problem' | 'person'

export interface NoticeAction {
  label: string
  /** An action of the panel, or an address to open. */
  run?: () => void
  href?: string
}

export interface Notice {
  kind: NoticeKind
  title: string
  text?: string
  action?: NoticeAction
  /** Stays until closed: errors and requests stay anyway. */
  sticky?: boolean
}

export interface NoticesValue {
  /** Shows a notification at the bottom right; returns its id. */
  notify: (notice: Notice) => number
  /** Replaces a notification, such as "Publishing…" by its result. */
  update: (id: number, notice: Notice) => void
  dismiss: (id: number) => void
  /** Brings the chat forward, for notices that point to it. */
  openChat: () => void
}

export const NoticesContext = createContext<NoticesValue>({
  notify: () => 0,
  update: () => {},
  dismiss: () => {},
  openChat: () => {},
})

export function useNotices(): NoticesValue {
  return useContext(NoticesContext)
}
