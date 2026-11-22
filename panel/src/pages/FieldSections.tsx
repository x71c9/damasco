import { useMemo } from 'react'
import type { PageSummary, TypeField } from '../api'
import { Button } from '../components/Button'
import { FieldInputs } from './FieldInputs'
import { Fold } from './Fold'
import { firstWords, hasProblem, inputsIn, sectionsOf, startsClosed, type FieldSection } from './sections'

interface FieldSectionsProps {
  title: string
  fields: TypeField[]
  value: Record<string, unknown>
  onChange: (value: Record<string, unknown>) => void
  problems: Record<string, string>
  pages: PageSummary[]
  editable: boolean
  /** The boxes the person opened or closed, kept by the editor. */
  open: Record<string, boolean>
  onOpen: (open: Record<string, boolean>) => void
}

function summaryOf(section: FieldSection, value: Record<string, unknown>): string {
  const words = firstWords(section.fields, value)
  if (words) return words
  const only = section.fields.length === 1 ? section.fields[0] : null
  if (only?.type === 'list') {
    const items = value[only.name]
    const count = Array.isArray(items) ? items.length : 0
    return count === 0 ? 'Empty' : `${count} ${count === 1 ? 'item' : 'items'}`
  }
  const count = inputsIn(section.fields)
  return `${count} ${count === 1 ? 'field' : 'fields'}`
}

/** The fields of a page type in titled boxes. With many, the boxes start
 * closed and read as the index of the page. */
export function FieldSections({
  title,
  fields,
  value,
  onChange,
  problems,
  pages,
  editable,
  open,
  onOpen,
}: FieldSectionsProps) {
  const sections = useMemo(() => sectionsOf(fields), [fields])
  const closed = startsClosed(sections)
  const isOpen = (section: FieldSection, index: number) => open[section.key] ?? (!closed || index === 0)
  const allOpen = sections.every(isOpen)

  return (
    <section className="editor-section" aria-label={title}>
      <div className="editor-section-head">
        <h2>{title}</h2>
        {sections.length > 1 && (
          <Button
            variant="quiet"
            small
            onClick={() =>
              onOpen({ ...open, ...Object.fromEntries(sections.map((section) => [section.key, !allOpen])) })
            }
          >
            {allOpen ? 'Close all' : 'Open all'}
          </Button>
        )}
      </div>
      <div className="editor-folds">
        {sections.map((section, index) => (
          <Fold
            key={section.key}
            title={section.title}
            summary={summaryOf(section, value)}
            invalid={section.fields.some((field) => hasProblem(problems, `fields.${field.name}`))}
            open={isOpen(section, index)}
            onOpen={(now) => onOpen({ ...open, [section.key]: now })}
          >
            <FieldInputs
              fields={section.fields}
              value={value}
              onChange={onChange}
              problems={problems}
              path="fields"
              pages={pages}
              editable={editable}
              titled={section.titled}
            />
          </Fold>
        ))}
      </div>
    </section>
  )
}
