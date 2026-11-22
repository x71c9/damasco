import type { TypeField } from '../api'

/** A box of the editor: fields that belong together, under a title. */
export interface FieldSection {
  key: string
  title: string
  fields: TypeField[]
  /** The box is one group or list, whose name is the box's title. */
  titled: boolean
}

/** Whether a field is a part of the page by itself. */
function isPart(field: TypeField): boolean {
  return field.type === 'group' || (field.type === 'list' && field.of?.type === 'group')
}

function box(key: string, title: string, fields: TypeField[]): FieldSection {
  const only = fields.length === 1 ? fields[0] : null
  return { key, title, fields, titled: only !== null && isPart(only) && only.label === title }
}

/** The boxes of a page type's fields: the sections the type names or,
 * without any, those its own shape and names tell. */
export function sectionsOf(fields: TypeField[]): FieldSection[] {
  return fields.some((field) => field.section) ? named(fields) : guessed(fields)
}

/** The sections the type names, each where it first appears. */
function named(fields: TypeField[]): FieldSection[] {
  const found = new Map<string, TypeField[]>()
  for (const field of fields) {
    const title = field.section?.trim() ?? ''
    found.set(title, [...(found.get(title) ?? []), field])
  }
  return [...found].map(([title, its]) =>
    box(
      title ? `named:${title}` : 'unnamed',
      title || (its.length === 1 ? its[0].label : 'Other details'),
      its,
    ),
  )
}

/** For a type without sections, in the order of its fields: a group or a
 * list of groups is a box; so are neighbours whose names start alike
 * (`hero_title`, `hero_image`); the others in between share one. */
function guessed(fields: TypeField[]): FieldSection[] {
  const plain = fields.filter((field) => !isPart(field)).map((field) => field.name.split('_'))
  // What every name starts with tells nothing apart, as `home_` in
  // `home_hero_title` and `home_about_title`.
  let skip = 0
  while (
    plain.length > 1 &&
    plain.every((words) => words.length > skip + 2 && words[skip] === plain[0][skip])
  ) {
    skip++
  }
  const start = (field: TypeField) => (isPart(field) ? null : field.name.split('_')[skip])

  const boxes: FieldSection[] = []
  let loose: TypeField[] = []
  const closeLoose = () => {
    if (loose.length) boxes.push(box(`from:${loose[0].name}`, titleOfLoose(loose), loose))
    loose = []
  }
  for (let at = 0; at < fields.length; ) {
    const field = fields[at]
    const word = start(field)
    let end = at + 1
    while (word && end < fields.length && start(fields[end]) === word) end++
    if (isPart(field) || end - at > 1) {
      closeLoose()
      const its = fields.slice(at, end)
      const title = isPart(field) ? field.label : titleOfRun(its, word ?? '')
      boxes.push(box(`from:${field.name}`, title, its))
    } else {
      loose.push(field)
    }
    at = end
  }
  closeLoose()
  return boxes
}

/** Words that only join a label to the part it names: "Testo del footer". */
const JOINING = /^(?:(?:di|del|dello|della|dei|degli|delle|de|du|des|of|the)\s+)+/iu

/** How many of the first words the labels share. */
function wordsShared(labels: string[][]): number {
  let same = 0
  while (labels.every((words) => same < words.length && words[same].toLowerCase() === labels[0][same].toLowerCase())) {
    same++
  }
  return same
}

/** The words the labels start with ("Hero title", "Hero image": Hero) or
 * end with ("Titolo chi siamo", "Testo chi siamo": Chi siamo); otherwise
 * the word the names start with, which is the code's, often in English. */
function titleOfRun(fields: TypeField[], word: string): string {
  const labels = fields.map((field) => field.label.trim().split(/\s+/))
  const first = wordsShared(labels)
  const start = labels[0].slice(0, first).join(' ').replace(/[\s:·–—-]+$/u, '')
  if (start) return capital(start)
  const last = wordsShared(labels.map((words) => [...words].reverse()))
  const end = labels[0].slice(labels[0].length - last).join(' ').replace(/^[\s:·–—-]+/u, '').replace(JOINING, '')
  return capital(/\p{L}/u.test(end) ? end : word)
}

function capital(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1)
}

/** Fields with nothing in common are named by what they are. */
function titleOfLoose(fields: TypeField[]): string {
  const names = fields.slice(0, 3).map((field) => field.label)
  return fields.length > 3 ? `${names.join(', ')}…` : names.join(', ')
}

/** Inputs a person finds in these fields. */
export function inputsIn(fields: TypeField[]): number {
  return fields.reduce(
    (sum, field) => sum + (field.type === 'group' ? inputsIn(field.fields ?? []) : 1),
    0,
  )
}

/** More inputs than this do not fit at a glance: the boxes start closed. */
const AT_A_GLANCE = 8

/** Whether the boxes start closed, the first one aside: they are then the
 * index of the page. */
export function startsClosed(sections: FieldSection[]): boolean {
  return sections.length > 1 && inputsIn(sections.flatMap((section) => section.fields)) > AT_A_GLANCE
}

/** The first words written in these fields, to tell a closed box apart. */
export function firstWords(fields: TypeField[], value: unknown): string {
  if (!value || typeof value !== 'object') return ''
  const values = value as Record<string, unknown>
  for (const field of fields) {
    const here = values[field.name]
    if (field.type === 'text' && typeof here === 'string' && here.trim()) return here.trim().slice(0, 120)
    if (field.type === 'group') {
      const inner = firstWords(field.fields ?? [], here)
      if (inner) return inner
    }
  }
  return ''
}

/** Whether a problem of the server is in the field at `path`. */
export function hasProblem(problems: Record<string, string>, path: string): boolean {
  return Object.keys(problems).some(
    (key) => key === path || key.startsWith(`${path}.`) || key.startsWith(`${path}[`),
  )
}
