import type { InputHTMLAttributes } from 'react'
import { sentence } from '../text'
import './Field.css'

interface FieldProps extends InputHTMLAttributes<HTMLInputElement> {
  id: string
  label: string
  /** Short note on the right of the label, such as "10+ characters". */
  note?: string
  /** Error for this field: replaces the note and turns the input red. */
  error?: string | null
  /** Marks the input as wrong without a message of its own, when the
   * message belongs to the form and appears on another field. */
  invalid?: boolean
}

/**
 * A labelled input. Notes and errors share one line with the label, so a
 * message never pushes the layout: the panel never moves when an error
 * appears or disappears.
 */
export function Field({ id, label, note, error, invalid, ...input }: FieldProps) {
  const message = error ? sentence(error) : note
  return (
    <div className="field">
      <div className="field-head">
        <label htmlFor={id}>{label}</label>
        {message && (
          <span
            id={`${id}-message`}
            className={error ? 'field-error' : 'field-note'}
            role={error ? 'alert' : undefined}
            title={message}
          >
            {message}
          </span>
        )}
      </div>
      <input
        id={id}
        aria-invalid={Boolean(error) || invalid || undefined}
        aria-describedby={message ? `${id}-message` : undefined}
        {...input}
      />
    </div>
  )
}
