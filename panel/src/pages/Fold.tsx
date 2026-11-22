import type { ReactNode } from 'react'
import { Icon } from '../components/icons'
import './Fold.css'

interface FoldProps {
  title: string
  /** What is inside, in a few words, while it is closed. */
  summary?: string
  /** Something inside needs a change. */
  invalid?: boolean
  open: boolean
  onOpen: (open: boolean) => void
  /** A box inside another, such as an item of a list. */
  small?: boolean
  children: ReactNode
}

/** A titled box that opens and closes. What is inside stays there while it
 * is closed: the text editors keep their undo, and the browser's find
 * opens the box it finds words in. */
export function Fold({ title, summary, invalid = false, open, onOpen, small = false, children }: FoldProps) {
  return (
    <details
      className={`fold${small ? ' fold-small' : ''}`}
      data-invalid={invalid || undefined}
      open={open}
      // `toggle` also comes as the box appears open, and from boxes inside.
      onToggle={(event) => {
        if (event.target === event.currentTarget && event.currentTarget.open !== open) {
          onOpen(event.currentTarget.open)
        }
      }}
    >
      <summary className="fold-head">
        <span className="fold-heading">
          <span className="fold-title" role="heading" aria-level={small ? 4 : 3}>
            {title}
          </span>
          {!open && summary && <span className="fold-summary">{summary}</span>}
        </span>
        {invalid && <span className="fold-problem">Needs a change</span>}
        <span className="fold-chevron">
          <Icon name="chevronDown" size={16} strokeWidth={2} />
        </span>
      </summary>
      <div className="fold-body">{children}</div>
    </details>
  )
}
