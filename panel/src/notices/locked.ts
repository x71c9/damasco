import type { Notice } from './context'

/** The notice of a section whose content cannot change now: why (from the
 * server, "the assistant is working on the site" or "Ada has the terminal
 * open"), and the way to the chat. */
export function lockedNotice(locked: string, openChat: () => void): Notice {
  if (locked.startsWith('the assistant')) {
    return {
      kind: 'working',
      title: 'The assistant is working on the site',
      text: 'Editing is paused until it finishes.',
      action: { label: 'Open the chat', run: openChat },
    }
  }
  if (locked.startsWith('the site is going back')) {
    return {
      kind: 'working',
      title: 'The site is going back to a build',
      text: 'Editing is paused for a moment.',
    }
  }
  return {
    kind: 'person',
    title: locked.charAt(0).toUpperCase() + locked.slice(1),
    text: 'Editing is paused until the terminal closes.',
    action: { label: 'Open the chat', run: openChat },
  }
}
