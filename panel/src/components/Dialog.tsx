import { useEffect, useRef, type ReactNode } from 'react'
import { Icon } from './icons'
import './Dialog.css'

interface DialogProps {
  title: string
  /** One line under the title. */
  subtitle?: ReactNode
  /** A class for a dialog of its own size. */
  className?: string
  onClose: () => void
  children: ReactNode
}

/** Modal dialog on the native <dialog>: focus stays inside, Esc closes. */
export function Dialog({ title, subtitle, className, onClose, children }: DialogProps) {
  const ref = useRef<HTMLDialogElement>(null)

  useEffect(() => {
    const dialog = ref.current
    if (dialog && !dialog.open) dialog.showModal()
  }, [])

  return (
    <dialog
      ref={ref}
      className={className ? `dialog ${className}` : 'dialog'}
      aria-labelledby="dialog-title"
      onCancel={(event) => {
        event.preventDefault()
        onClose()
      }}
      onClick={(event) => {
        // A click on the backdrop lands on the dialog element itself.
        if (event.target === ref.current) onClose()
      }}
    >
      <div className="dialog-head">
        <div className="dialog-heading">
          <h2 id="dialog-title">{title}</h2>
          {subtitle && <p className="dialog-subtitle">{subtitle}</p>}
        </div>
        <button type="button" className="dialog-close" aria-label="Close" onClick={onClose}>
          <Icon name="close" size={18} strokeWidth={2} />
        </button>
      </div>
      <div className="dialog-body">{children}</div>
    </dialog>
  )
}

/** Buttons at the bottom of a dialog, aligned to the right. */
export function DialogActions({ children }: { children: ReactNode }) {
  return <div className="dialog-actions">{children}</div>
}
