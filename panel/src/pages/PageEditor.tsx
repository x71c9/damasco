import { Suspense, lazy, useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from 'react'
import { ApiError, pageTypes, pages, type Page, type PageSummary, type PageType } from '../api'
import { PendingState } from '../changes/changes'
import { useChanges } from '../changes/context'
import { Button } from '../components/Button'
import { Dialog, DialogActions } from '../components/Dialog'
import { Field } from '../components/Field'
import { Icon } from '../components/icons'
import { MediaChooser } from '../media/MediaChooser'
import { dropParams, param } from '../navigation'
import { FieldInputs } from './FieldInputs'
import type { PMNode } from './richtext'
import '../components/Page.css'
import './PageEditor.css'

const TextEditor = lazy(() => import('./TextEditor'))

/** The element of a field of the editor, by its name in the page:
 * `title`, `seo.description`, `fields.year`, `blocks.<id>`. */
function fieldElement(field: string): HTMLElement | null {
  const ids: Record<string, string> = {
    title: 'page-title',
    slug: 'page-slug',
    inside: 'page-parent',
    menu: 'page-menu',
    'seo.title': 'seo-title',
    'seo.description': 'seo-description',
    'seo.image': 'seo-image',
    'seo.hidden': 'seo-hidden',
  }
  const id =
    ids[field] ??
    (field.startsWith('fields.')
      ? field.replace(/[^a-zA-Z0-9]+/g, '-')
      : field.startsWith('blocks.')
        ? `block-${field.slice(7)}`
        : null)
  if (!id) return null
  return document.getElementById(id) ?? document.querySelector<HTMLElement>(`label[for="${CSS.escape(id)}"]`)
}

/** Brings a field into view, puts the cursor in it and marks it for a
 * moment: the place the assistant pointed to. */
function pointTo(field: string): boolean {
  const element = fieldElement(field)
  if (!element) return false
  const box = element.closest<HTMLElement>('.field, .editor-block, .editor-check') ?? element
  const still = window.matchMedia('(prefers-reduced-motion: reduce)').matches
  box.scrollIntoView({ block: 'center', behavior: still ? 'auto' : 'smooth' })
  const target = element.matches('input, select, textarea, button, [contenteditable]')
    ? element
    : box.querySelector<HTMLElement>('input, select, textarea, [contenteditable="true"], button')
  target?.focus({ preventScroll: true })
  box.classList.remove('editor-pointed')
  void box.offsetWidth
  box.classList.add('editor-pointed')
  window.setTimeout(() => box.classList.remove('editor-pointed'), 2600)
  return true
}

/** JSON with the keys in order, to tell whether two pages are the same. */
function canonical(value: unknown): string {
  return JSON.stringify(value, (_, inner: unknown) =>
    inner && typeof inner === 'object' && !Array.isArray(inner)
      ? Object.fromEntries(Object.entries(inner).sort(([a], [b]) => a.localeCompare(b)))
      : inner,
  )
}

interface Block {
  id: string
  type: 'text' | 'media'
  body?: PMNode
  media?: unknown
  caption?: string | null
}

function newId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(4))
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')
}

/** Every page inside `id`, at any depth. */
function inside(list: PageSummary[], id: string): Set<string> {
  const found = new Set<string>([id])
  let grew = true
  while (grew) {
    grew = false
    for (const page of list) {
      if (page.inside && found.has(page.inside) && !found.has(page.id)) {
        found.add(page.id)
        grew = true
      }
    }
  }
  return found
}

interface PageEditorProps {
  id: string
  /** The preview of the site, to open the page in. */
  previewUrl: string | null
  onBack: () => void
}

/** One page: title, fields of its type, text and media blocks, place on the
 * site, search engines. Saved as a whole, checked by the server. */
export function PageEditor({ id, previewUrl, onBack }: PageEditorProps) {
  const [saved, setSaved] = useState<Page | null>(null)
  const [draft, setDraft] = useState<Page | null>(null)
  const [list, setList] = useState<PageSummary[]>([])
  const [types, setTypes] = useState<PageType[]>([])
  const [locked, setLocked] = useState<string | null>(null)
  const [problems, setProblems] = useState<Record<string, string>>({})
  const [error, setError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const { changes, refresh: refreshChanges } = useChanges()
  // The page as changed elsewhere (the assistant, another person) while
  // this one has unsaved changes.
  const [newer, setNewer] = useState<Page | null>(null)
  // Raised when the form shows another version: the text editors read
  // their value once, so they start again.
  const [revision, setRevision] = useState(0)
  const current = useRef({ saved, draft, saving })
  // The field the address points to (`?field=…`), from a button of the
  // assistant; taken again when the address changes on this page.
  const [pointed, setPointed] = useState(() => param('field'))
  useEffect(() => {
    const onPop = () => setPointed(param('field'))
    window.addEventListener('popstate', onPop)
    return () => window.removeEventListener('popstate', onPop)
  }, [])
  useEffect(() => {
    current.current = { saved, draft, saving }
  })
  const [leaving, setLeaving] = useState(false)

  useEffect(() => {
    pages.read(id).then(
      (page) => {
        setSaved(page)
        setDraft(page)
      },
      (err: unknown) => setError(err instanceof Error ? err.message : String(err)),
    )
    pageTypes.list().then(setTypes, () => {})
  }, [id])
  // Something was saved on the site (a new build, an answer of the
  // assistant): the form follows when it holds no unsaved change.
  useEffect(() => {
    if (!changes) return
    let gone = false
    pages.read(id).then(
      (page) => {
        const { saved: before, draft: now, saving: busy } = current.current
        if (gone || busy || !before || canonical(before) === canonical(page)) return
        if (canonical(now) === canonical(before)) {
          setSaved(page)
          setDraft(page)
          setNewer(null)
          setRevision((n) => n + 1)
        } else {
          setNewer(page)
        }
      },
      () => {},
    )
    return () => {
      gone = true
    }
  }, [changes, id])
  const loadNewer = () => {
    if (!newer) return
    setSaved(newer)
    setDraft(newer)
    setNewer(null)
    setProblems({})
    setRevision((n) => n + 1)
  }

  // The other pages, for the parent and the links, and whether the
  // assistant is at work.
  const refresh = useCallback(() => {
    pages.list().then(
      (result) => {
        setList(result.pages)
        setLocked(result.locked)
      },
      () => {},
    )
  }, [])
  useEffect(() => {
    refresh()
    const timer = setInterval(refresh, 4000)
    return () => clearInterval(timer)
  }, [refresh])

  const dirty = useMemo(() => canonical(saved) !== canonical(draft), [saved, draft])
  const loaded = draft !== null
  const typed = types.length > 0
  useEffect(() => {
    if (!pointed || !loaded) return
    // Fields of the page type wait for the types.
    if (pointed.startsWith('fields.') && !typed) return
    const frame = requestAnimationFrame(() => {
      pointTo(pointed)
      setPointed(null)
      dropParams()
    })
    return () => cancelAnimationFrame(frame)
  }, [pointed, loaded, typed])
  const editable = !locked && !saving
  const type = types.find((t) => t.name === draft?.type)
  const parentAddress = useMemo(() => {
    const parent = list.find((page) => page.id === draft?.inside)
    return parent ? parent.address.replace(/\/$/, '') : ''
  }, [list, draft?.inside])
  const blocked = useMemo(() => (draft ? inside(list, draft.id) : new Set<string>()), [list, draft])

  const change = (changes: Partial<Page>) => {
    setDraft((current) => (current ? { ...current, ...changes } : current))
    setError(null)
  }
  const changeSeo = (changes: Partial<Page['seo']>) =>
    setDraft((current) => (current ? { ...current, seo: { ...current.seo, ...changes } } : current))

  const blocks = (draft?.blocks ?? []) as Block[]
  const setBlocks = (next: Block[]) => change({ blocks: next })
  const changeBlock = (index: number, changes: Partial<Block>) =>
    setBlocks(blocks.map((block, i) => (i === index ? { ...block, ...changes } : block)))
  const moveBlock = (index: number, by: number) => {
    const next = [...blocks]
    const [block] = next.splice(index, 1)
    next.splice(index + by, 0, block)
    setBlocks(next)
  }

  const save = async () => {
    if (!draft || !dirty || !editable) return
    setSaving(true)
    setError(null)
    setProblems({})
    try {
      const { title, slug, inside: parent, menu, fields, seo } = draft
      const page = await pages.update(draft.id, {
        title,
        slug,
        inside: parent,
        menu,
        fields,
        blocks: draft.blocks,
        seo,
      })
      setSaved(page)
      setDraft(page)
      setNewer(null)
      refreshChanges()
    } catch (err) {
      if (err instanceof ApiError && err.problems.length) {
        setProblems(Object.fromEntries(err.problems.map((p) => [p.field, p.message])))
        setError('Some fields need a change: they are marked below.')
      } else {
        setError(err instanceof Error ? err.message : String(err))
      }
    }
    setSaving(false)
  }

  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if ((event.ctrlKey || event.metaKey) && event.key === 's') {
      event.preventDefault()
      save()
    }
  }

  if (!draft) {
    return (
      <section className="page-frame">
        <div className="page-card">
          <p className="editor-note">{error ?? 'Loading the page…'}</p>
        </div>
      </section>
    )
  }

  const problem = (field: string) => {
    const message = problems[field]
    return message ? message.charAt(0).toUpperCase() + message.slice(1) : null
  }
  const isHome = draft.inside === null
  const seoTitle = draft.seo.title ?? ''
  const seoDescription = draft.seo.description ?? ''

  return (
    <section className="page-frame" aria-labelledby="editor-title" onKeyDown={onKeyDown}>
      <div className="page-card editor-card">
        <header className="editor-head">
          <button
            type="button"
            className="editor-back"
            aria-label="Back to all pages"
            title="All pages"
            onClick={() => (dirty ? setLeaving(true) : onBack())}
          >
            <Icon name="chevronLeft" size={20} strokeWidth={2} />
          </button>
          <div className="editor-heading">
            <h1 id="editor-title">{draft.title || 'Untitled page'}</h1>
            <span className="editor-status">
              {locked
                ? `Read-only while ${locked}`
                : dirty
                  ? 'Unsaved changes'
                  : <PendingState kind="pages" id={id} />}
            </span>
          </div>
          {previewUrl && (
            <a
              className="button button-secondary"
              href={`${previewUrl.replace(/\/$/, '')}${saved?.address ?? list.find((p) => p.id === id)?.address ?? '/'}`}
              target="_blank"
              rel="noreferrer"
            >
              View in preview
            </a>
          )}
          <Button variant="primary" onClick={save} disabled={!dirty || !editable}>
            {saving ? 'Saving…' : 'Save'}
          </Button>
        </header>

        {error && (
          <p className="editor-error" role="alert">
            {error}
          </p>
        )}

        {newer && (
          <div className="editor-newer" role="status">
            <p>
              This page was changed elsewhere while it was open here. Saving now keeps the
              changes made here over that version.
            </p>
            <Button small onClick={loadNewer}>
              Load the new version
            </Button>
          </div>
        )}

        <div className="editor-columns" key={revision}>
          <div className="editor-main">
            <Field
              id="page-title"
              label="Title"
              note="Also the name in the site menu."
              error={problem('title')}
              value={draft.title}
              disabled={!editable}
              onChange={(event) => change({ title: event.target.value })}
            />

            {type && type.fields.length > 0 && (
              <section className="editor-section" aria-label={`${type.label} details`}>
                <h2>{type.label} details</h2>
                <FieldInputs
                  fields={type.fields}
                  value={draft.fields}
                  onChange={(fields) => change({ fields })}
                  problems={problems}
                  path="fields"
                  pages={list}
                  editable={editable}
                />
              </section>
            )}

            <section className="editor-section" aria-label="Page content">
              <h2>Page content</h2>
              {blocks.length === 0 && <p className="editor-note">No content yet: add a block.</p>}
              <ol className="editor-blocks">
                {blocks.map((block, index) => {
                  const blockProblem =
                    problem(`blocks[${index}]`) ?? problem(`blocks[${index}].body`) ?? problem(`blocks[${index}].caption`)
                  return (
                    <li
                      key={block.id}
                      id={`block-${block.id}`}
                      className={`editor-block${blockProblem ? ' editor-block-invalid' : ''}`}
                    >
                      <div className="editor-block-head">
                        <span>{block.type === 'text' ? 'Text' : 'Media'}</span>
                        {blockProblem && <span className="field-error">{blockProblem}</span>}
                        <span className="editor-block-actions">
                          <button
                            type="button"
                            className="icon-button"
                            aria-label="Move up"
                            title="Move up"
                            disabled={!editable || index === 0}
                            onClick={() => moveBlock(index, -1)}
                          >
                            <Icon name="chevronUp" size={16} strokeWidth={2} />
                          </button>
                          <button
                            type="button"
                            className="icon-button"
                            aria-label="Move down"
                            title="Move down"
                            disabled={!editable || index === blocks.length - 1}
                            onClick={() => moveBlock(index, 1)}
                          >
                            <Icon name="chevronDown" size={16} strokeWidth={2} />
                          </button>
                          <button
                            type="button"
                            className="icon-button"
                            aria-label="Remove the block"
                            title="Remove the block"
                            disabled={!editable}
                            onClick={() => setBlocks(blocks.filter((_, i) => i !== index))}
                          >
                            <Icon name="close" size={16} strokeWidth={2} />
                          </button>
                        </span>
                      </div>
                      {block.type === 'text' ? (
                        <Suspense fallback={<p className="editor-note">Loading the editor…</p>}>
                          <TextEditor
                            label={`Text block ${index + 1}`}
                            value={block.body}
                            editable={editable}
                            onChange={(body) => changeBlock(index, { body })}
                          />
                        </Suspense>
                      ) : (
                        <div className="editor-media">
                          <MediaChooser
                            purpose="a picture for this block"
                            value={(block.media as { media: string } | null) ?? null}
                            editable={editable}
                            onChange={(media) => changeBlock(index, { media })}
                          />
                          <Field
                            id={`caption-${block.id}`}
                            label="Caption"
                            value={block.caption ?? ''}
                            disabled={!editable}
                            onChange={(event) => changeBlock(index, { caption: event.target.value || null })}
                          />
                        </div>
                      )}
                    </li>
                  )
                })}
              </ol>
              <div className="editor-add">
                <span>Add a block</span>
                <Button
                  small
                  icon="plus"
                  disabled={!editable}
                  onClick={() =>
                    setBlocks([...blocks, { id: newId(), type: 'text', body: { type: 'doc', content: [] } }])
                  }
                >
                  Text
                </Button>
                <Button
                  small
                  icon="plus"
                  disabled={!editable}
                  onClick={() => setBlocks([...blocks, { id: newId(), type: 'media', media: null, caption: null }])}
                >
                  Media
                </Button>
              </div>
            </section>
          </div>

          <aside className="editor-side">
            <section className="editor-section" aria-label="Place on the site">
              <h2>Place on the site</h2>
              <div className="field">
                <div className="field-head">
                  <label htmlFor="page-parent">Parent</label>
                  {problem('inside') && <span className="field-error">{problem('inside')}</span>}
                </div>
                <select
                  id="page-parent"
                  value={draft.inside ?? ''}
                  disabled={!editable || isHome}
                  onChange={(event) => change({ inside: event.target.value })}
                >
                  {isHome && <option value="">None: this is the home page</option>}
                  {list
                    .filter((page) => !blocked.has(page.id))
                    .map((page) => (
                      <option key={page.id} value={page.id}>
                        {page.title} ({page.address})
                      </option>
                    ))}
                </select>
              </div>
              <div className="field">
                <div className="field-head">
                  <label htmlFor="page-slug">Address</label>
                  {problem('slug') && (
                    <span className="field-error" title={problem('slug') ?? ''}>
                      {problem('slug')}
                    </span>
                  )}
                </div>
                <div className="editor-address">
                  <span className="editor-address-prefix">{parentAddress}/</span>
                  <input
                    id="page-slug"
                    value={draft.slug}
                    disabled={!editable || isHome}
                    aria-invalid={Boolean(problem('slug')) || undefined}
                    onChange={(event) => change({ slug: event.target.value })}
                  />
                </div>
              </div>
              <label className="editor-check">
                <input
                  id="page-menu"
                  type="checkbox"
                  checked={draft.menu}
                  disabled={!editable}
                  onChange={(event) => change({ menu: event.target.checked })}
                />
                Show it in the site menu
              </label>
            </section>

            <section className="editor-section" aria-label="Search engines and sharing">
              <h2>Search engines and sharing</h2>
              <p className="editor-note">
                How the page appears on Google and when shared in a message. Empty fields use the
                page title and the first text.
              </p>
              <div className="editor-serp" aria-label="Preview of the search result">
                <span className="editor-serp-address">{saved?.address ?? '/'}</span>
                <span className="editor-serp-title">{seoTitle || draft.title}</span>
                <span className="editor-serp-text">{seoDescription || 'The first text of the page.'}</span>
              </div>
              <Field
                id="seo-title"
                label="Title in search results"
                note={`${seoTitle.length} / 60`}
                error={problem('seo.title')}
                value={seoTitle}
                disabled={!editable}
                onChange={(event) => changeSeo({ title: event.target.value || null })}
              />
              <div className="field">
                <div className="field-head">
                  <label htmlFor="seo-description">Description in search results</label>
                  <span className={problem('seo.description') ? 'field-error' : 'field-note'}>
                    {problem('seo.description') ?? `${seoDescription.length} / 160`}
                  </span>
                </div>
                <textarea
                  id="seo-description"
                  rows={3}
                  value={seoDescription}
                  disabled={!editable}
                  onChange={(event) => changeSeo({ description: event.target.value || null })}
                />
              </div>
              <div className="field" id="seo-image">
                <div className="field-head">
                  <span className="editor-label">Image when shared</span>
                  {problem('seo.image') && <span className="field-error">{problem('seo.image')}</span>}
                </div>
                <MediaChooser
                  purpose="the image when shared"
                  value={(draft.seo.image as { media: string } | null) ?? null}
                  editable={editable}
                  onChange={(image) => changeSeo({ image })}
                />
                <p className="editor-note">Shown on WhatsApp, Facebook, LinkedIn. Without one, the first picture of the page.</p>
              </div>
              <label className="editor-check">
                <input
                  id="seo-hidden"
                  type="checkbox"
                  checked={draft.seo.hidden}
                  disabled={!editable}
                  onChange={(event) => changeSeo({ hidden: event.target.checked })}
                />
                Hide from search engines
              </label>
            </section>
          </aside>
        </div>
      </div>

      {leaving && (
        <Dialog title="Leave without saving?" onClose={() => setLeaving(false)}>
          <p className="dialog-text">The changes made since the last save are lost.</p>
          <DialogActions>
            <Button onClick={() => setLeaving(false)} autoFocus>
              Keep editing
            </Button>
            <Button variant="danger" onClick={onBack}>
              Leave
            </Button>
          </DialogActions>
        </Dialog>
      )}
    </section>
  )
}
