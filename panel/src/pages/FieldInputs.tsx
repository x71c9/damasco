import { Suspense, lazy, useState } from 'react'
import type { PageSummary, TypeField } from '../api'
import { Button } from '../components/Button'
import { MediaChooser } from '../media/MediaChooser'
import { Fold } from './Fold'
import type { PMNode } from './richtext'
import { firstWords, hasProblem } from './sections'

const TextEditor = lazy(() => import('./TextEditor'))

type Kind = Omit<TypeField, 'name' | 'label' | 'required' | 'help' | 'section'>

interface FieldInputsProps {
  fields: TypeField[]
  value: Record<string, unknown>
  onChange: (value: Record<string, unknown>) => void
  /** Problems by path, such as `fields.year` or `fields.team[0].name`. */
  problems: Record<string, string>
  path: string
  pages: PageSummary[]
  editable: boolean
  /** The box around shows the label of the one field: only problems and
   * screen readers need it again. */
  titled?: boolean
}

/** The fields of a page type, as a form: one input per kind of value. */
export function FieldInputs({ fields, value, onChange, problems, path, pages, editable, titled = false }: FieldInputsProps) {
  return (
    <div className="type-fields">
      {fields.map((field) => {
        const here = `${path}.${field.name}`
        const id = here.replace(/[^a-zA-Z0-9]+/g, '-')
        const problem = problems[here]
        const named = titled && !problem && !field.required
        return (
          <div key={field.name} className="field type-field">
            <div className={named ? 'visually-hidden' : 'field-head'}>
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
      return (
        <MediaChooser
          purpose={label.toLowerCase()}
          value={(value as { media: string } | null) ?? null}
          editable={editable}
          onChange={onChange}
        />
      )
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
    case 'list':
      return (
        <ListInput
          {...{ id, label, value, onChange, problems, path, pages, editable }}
          of={kind.of ?? { type: 'text' }}
        />
      )
  }
}

/** A list: each item keeps its place in the form when one before it goes,
 * and items with several fields are boxes of their own. */
function ListInput({
  id,
  of,
  label,
  value,
  onChange,
  problems,
  path,
  pages,
  editable,
}: Omit<InputProps, 'kind' | 'invalid'> & { of: Kind }) {
  const items: unknown[] = Array.isArray(value) ? value : []
  // A name for each item: by position, a removed one would leave its text
  // editor, and its box open or closed, to the next.
  const [names, setNames] = useState(() => items.map((_, index) => index))
  // The only item starts open, and each one added.
  const [open, setOpen] = useState<number[]>(() => (items.length === 1 ? [0] : []))
  const nameOf = (index: number) => names[index] ?? index
  const add = () => {
    const name = Math.max(-1, ...names) + 1
    setNames([...names, name])
    setOpen([...open, name])
    onChange([...items, emptyOf(of)])
  }
  const remove = (index: number) => {
    setNames(names.filter((_, i) => i !== index))
    onChange(items.filter((_, i) => i !== index))
  }

  return (
    <div className="type-list">
      {items.map((item, index) => {
        const name = nameOf(index)
        const here = `${path}[${index}]`
        const input = (
          <Input
            id={`${id}-${index}`}
            kind={of}
            label={`${label} ${index + 1}`}
            value={item}
            onChange={(next) => onChange(items.map((old, i) => (i === index ? next : old)))}
            problems={problems}
            path={here}
            pages={pages}
            editable={editable}
            invalid={Boolean(problems[here])}
          />
        )
        const removeButton = (
          <Button variant="quiet" small disabled={!editable} onClick={() => remove(index)}>
            Remove
          </Button>
        )
        return of.type === 'group' ? (
          <div key={name} className="type-list-box">
            <Fold
              small
              title={`${label} ${index + 1}`}
              summary={firstWords(of.fields ?? [], item)}
              invalid={hasProblem(problems, here)}
              open={open.includes(name)}
              onOpen={(now) => setOpen((all) => (now ? [...all, name] : all.filter((one) => one !== name)))}
            >
              {input}
              <div className="type-list-remove">{removeButton}</div>
            </Fold>
          </div>
        ) : (
          <div key={name} className="type-list-item">
            <div className="type-list-input">{input}</div>
            {removeButton}
          </div>
        )
      })}
      <Button variant="secondary" small icon="plus" disabled={!editable} onClick={add}>
        Add
      </Button>
    </div>
  )
}
