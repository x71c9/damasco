import { Suspense, lazy } from 'react'
import type { PageSummary, TypeField } from '../api'
import { Button } from '../components/Button'
import type { PMNode } from './richtext'

const TextEditor = lazy(() => import('./TextEditor'))

type Kind = Omit<TypeField, 'name' | 'label' | 'required' | 'help'>

interface FieldInputsProps {
  fields: TypeField[]
  value: Record<string, unknown>
  onChange: (value: Record<string, unknown>) => void
  /** Problems by path, such as `fields.year` or `fields.team[0].name`. */
  problems: Record<string, string>
  path: string
  pages: PageSummary[]
  editable: boolean
}

/** The fields of a page type, as a form: one input per kind of value. */
export function FieldInputs({ fields, value, onChange, problems, path, pages, editable }: FieldInputsProps) {
  return (
    <div className="type-fields">
      {fields.map((field) => {
        const here = `${path}.${field.name}`
        const id = here.replace(/[^a-zA-Z0-9]+/g, '-')
        const problem = problems[here]
        return (
          <div key={field.name} className="field type-field">
            <div className="field-head">
              <label htmlFor={id}>
                {field.label}
                {field.required && <span className="type-required"> · required</span>}
              </label>
              {problem && (
                <span className="field-error" role="alert" title={problem}>
                  {problem.charAt(0).toUpperCase() + problem.slice(1)}
                </span>
              )}
            </div>
            <Input
              id={id}
              kind={field}
              label={field.label}
              value={value[field.name]}
              onChange={(next) => onChange({ ...value, [field.name]: next })}
              problems={problems}
              path={here}
              pages={pages}
              editable={editable}
              invalid={Boolean(problem)}
            />
            {field.help && <p className="type-help">{field.help}</p>}
          </div>
        )
      })}
    </div>
  )
}

interface InputProps {
  id: string
  kind: Kind
  label: string
  value: unknown
  onChange: (value: unknown) => void
  problems: Record<string, string>
  path: string
  pages: PageSummary[]
  editable: boolean
  invalid?: boolean
}

/** The empty value of a kind, for a new list item. */
function emptyOf(kind: Kind): unknown {
  switch (kind.type) {
    case 'boolean':
      return false
    case 'list':
      return []
    case 'group':
      return {}
    case 'richtext':
      return { type: 'doc', content: [] }
    default:
      return null
  }
}

function Input({ id, kind, label, value, onChange, problems, path, pages, editable, invalid }: InputProps) {
  const common = { id, disabled: !editable, 'aria-invalid': invalid || undefined }
  switch (kind.type) {
    case 'text':
      return (
        <input
          {...common}
          maxLength={kind.max ?? undefined}
          value={typeof value === 'string' ? value : ''}
          onChange={(event) => onChange(event.target.value === '' ? null : event.target.value)}
        />
      )
    case 'number':
      return (
        <input
          {...common}
          type="number"
          value={typeof value === 'number' ? value : ''}
          onChange={(event) => onChange(event.target.value === '' ? null : Number(event.target.value))}
        />
      )
    case 'date':
      return (
        <input
          {...common}
          type="date"
          value={typeof value === 'string' ? value : ''}
          onChange={(event) => onChange(event.target.value || null)}
        />
      )
    case 'boolean':
      return (
        <label className="type-check">
          <input
            {...common}
            type="checkbox"
            checked={value === true}
            onChange={(event) => onChange(event.target.checked)}
          />
          {label}
        </label>
      )
    case 'page': {
      const current = (value as { page?: string } | null)?.page ?? ''
      return (
        <select
          {...common}
          value={current}
          onChange={(event) => onChange(event.target.value ? { page: event.target.value } : null)}
        >
          <option value="">None</option>
          {pages.map((page) => (
            <option key={page.id} value={page.id}>
              {page.title} ({page.address})
            </option>
          ))}
        </select>
      )
    }
    case 'media':
      return <p className="type-later">Pictures arrive with the media library.</p>
    case 'richtext':
      return (
        <div className={`type-richtext${invalid ? ' type-invalid' : ''}`}>
          <Suspense fallback={<p className="type-later">Loading the editor…</p>}>
            <TextEditor
              label={label}
              value={value as PMNode}
              onChange={onChange}
              editable={editable}
            />
          </Suspense>
        </div>
      )
    case 'group':
      return (
        <div className="type-group">
          <FieldInputs
            fields={kind.fields ?? []}
            value={(value as Record<string, unknown>) ?? {}}
            onChange={onChange}
            problems={problems}
            path={path}
            pages={pages}
            editable={editable}
          />
        </div>
      )
    case 'list': {
      const items = Array.isArray(value) ? value : []
      const of = kind.of ?? { type: 'text' }
      return (
        <div className="type-list">
          {items.map((item, index) => (
            <div key={index} className="type-list-item">
              <div className="type-list-input">
                <Input
                  id={`${id}-${index}`}
                  kind={of}
                  label={`${label} ${index + 1}`}
                  value={item}
                  onChange={(next) => onChange(items.map((old, i) => (i === index ? next : old)))}
                  problems={problems}
                  path={`${path}[${index}]`}
                  pages={pages}
                  editable={editable}
                  invalid={Boolean(problems[`${path}[${index}]`])}
                />
              </div>
              <Button
                variant="quiet"
                small
                disabled={!editable}
                onClick={() => onChange(items.filter((_, i) => i !== index))}
              >
                Remove
              </Button>
            </div>
          ))}
          <Button
            variant="secondary"
            small
            icon="plus"
            disabled={!editable}
            onClick={() => onChange([...items, emptyOf(of)])}
          >
            Add
          </Button>
        </div>
      )
    }
  }
}
