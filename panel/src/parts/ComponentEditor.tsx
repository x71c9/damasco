import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from 'react'
import { ApiError, components, pages, type Component, type PageSummary } from '../api'
import { PendingState } from '../changes/changes'
import { useChanges } from '../changes/context'
import { Button } from '../components/Button'
import { Dialog, DialogActions } from '../components/Dialog'
import { Field } from '../components/Field'
import { Icon } from '../components/icons'
import { PageNotice } from '../notices/Notices'
import { useNotices } from '../notices/context'
import { lockedNotice } from '../notices/locked'
import { BlocksEditor, type Block } from '../pages/BlocksEditor'
import { sentence } from '../text'
import { whereText } from './where'
import '../components/Page.css'
import '../pages/PageEditor.css'
import './Components.css'

/** JSON with the keys in order, to tell whether two versions are the same. */
function canonical(value: unknown): string {
  return JSON.stringify(value, (_, inner: unknown) =>
    inner && typeof inner === 'object' && !Array.isArray(inner)
      ? Object.fromEntries(Object.entries(inner).sort(([a], [b]) => a.localeCompare(b)))
      : inner,
  )
}

/** How deep each page sits in the tree. */
function depths(list: PageSummary[]): Map<string, number> {
  const byId = new Map(list.map((page) => [page.id, page]))
  const depth = new Map<string, number>()
  for (const page of list) {
    let level = 0
    let parent = page.inside
    while (parent && level <= list.length) {
      level += 1
      parent = byId.get(parent)?.inside ?? null
    }
    depth.set(page.id, level)
  }
  return depth
}

const ids = (refs: { page: string }[]) => refs.map((ref) => ref.page)
const refs = (list: string[]) => list.map((page) => ({ page }))

/** One component: its name, its blocks, and the pages it appears on. */
export function ComponentEditor({ id, onBack }: { id: string; onBack: () => void }) {
  const [saved, setSaved] = useState<Component | null>(null)
  const [draft, setDraft] = useState<Component | null>(null)
  const [list, setList] = useState<PageSummary[]>([])
  const [locked, setLocked] = useState<string | null>(null)
  const [problems, setProblems] = useState<Record<string, string>>({})
  const [error, setError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const [leaving, setLeaving] = useState(false)
  const [deleting, setDeleting] = useState(false)
  const [scrolled, setScrolled] = useState(false)
  // Raised when the form shows another version: the text editors read
  // their value once, so they start again.
  const [revision, setRevision] = useState(0)
  const { changes, refresh: refreshChanges } = useChanges()
  const { notify, openChat } = useNotices()
  const current = useRef({ saved, draft, saving })
  useEffect(() => {
    current.current = { saved, draft, saving }
  })

  useEffect(() => {
    components.read(id).then(
      (component) => {
        setSaved(component)
        setDraft(component)
      },
      (err: unknown) => setError(err instanceof Error ? sentence(err.message) : String(err)),
    )
  }, [id])
  // Saved elsewhere (the assistant, another person): the form follows when
  // it holds no unsaved change.
  useEffect(() => {
    if (!changes) return
    let gone = false
    components.read(id).then(
      (component) => {
        const { saved: before, draft: now, saving: busy } = current.current
        if (gone || busy || !before || canonical(before) === canonical(component)) return
        if (canonical(now) === canonical(before)) {
          setSaved(component)
          setDraft(component)
          setRevision((n) => n + 1)
        }
      },
      () => {},
    )
    return () => {
      gone = true
    }
  }, [changes, id])

  const load = useCallback(() => {
    pages.list().then(
      (result) => {
        setList(result.pages)
        setLocked(result.locked)
      },
      () => {},
    )
  }, [])
  useEffect(() => {
    load()
    const timer = setInterval(load, 4000)
    return () => clearInterval(timer)
  }, [load])

  const depth = useMemo(() => depths(list), [list])
  const parents = useMemo(() => new Set(list.map((page) => page.inside).filter(Boolean)), [list])
  const dirty = useMemo(() => canonical(saved) !== canonical(draft), [saved, draft])
  const editable = !locked && !saving

  const change = (changes: Partial<Component>) => {
    setDraft((now) => (now ? { ...now, ...changes } : now))
    setError(null)
  }
  const toggle = (key: 'pages' | 'inside', page: string, on: boolean) => {
    if (!draft) return
    const chosen = ids(draft[key]).filter((id) => id !== page)
    change({ [key]: refs(on ? [...chosen, page] : chosen) })
  }

  const save = async () => {
    if (!draft || !dirty || !editable) return
    setSaving(true)
    setError(null)
    setProblems({})
    try {
      const { name, blocks, everywhere, pages: on, inside } = draft
      const component = await components.update(draft.id, { name, blocks, everywhere, pages: on, inside })
      setSaved(component)
      setDraft(component)
      refreshChanges()
    } catch (err) {
      if (err instanceof ApiError && err.problems.length) {
        setProblems(Object.fromEntries(err.problems.map((p) => [p.field, p.message])))
        setError('Some fields need a change')
      } else {
        notify({
          kind: 'problem',
          title: 'Couldn’t save',
          text: `${err instanceof Error ? sentence(err.message) : String(err)} The changes are still here.`,
        })
      }
    }
    setSaving(false)
  }

  const remove = async () => {
    if (!draft) return
    setDeleting(false)
    try {
      await components.remove(draft.id)
      refreshChanges()
      onBack()
    } catch (err) {
      notify({
        kind: 'problem',
        title: `Couldn’t delete “${draft.name}”`,
        text: err instanceof Error ? sentence(err.message) : String(err),
      })
    }
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
          <p className="editor-note">{error ?? 'Loading the component…'}</p>
        </div>
      </section>
    )
  }

  const problem = (field: string) => {
    const message = problems[field]
    return message ? message.charAt(0).toUpperCase() + message.slice(1) : null
  }
  const where = { everywhere: draft.everywhere, pages: ids(draft.pages), inside: ids(draft.inside) }
  const whereProblem = Object.entries(problems).find(([field]) => /^(pages|inside)/.test(field))?.[1]

  return (
    <section className="page-frame" aria-labelledby="editor-title" onKeyDown={onKeyDown}>
      <div className="page-card editor-card" onScroll={(event) => setScrolled(event.currentTarget.scrollTop > 0)}>
        <header className={`editor-head${scrolled ? ' editor-head-stuck' : ''}`}>
          <button
            type="button"
            className="editor-back"
            aria-label="Back to all components"
            title="All components"
            onClick={() => (dirty ? setLeaving(true) : onBack())}
          >
            <Icon name="chevronLeft" size={20} strokeWidth={2} />
          </button>
          <div className="editor-heading">
            <h1 id="editor-title">{draft.name || 'Unnamed component'}</h1>
            <span className="editor-status">
              {locked ? `Read-only while ${locked}` : dirty ? 'Unsaved changes' : <PendingState kind="components" id={id} />}
            </span>
          </div>
          <Button variant="primary" onClick={save} disabled={!dirty || !editable}>
            {saving ? 'Saving…' : 'Save'}
          </Button>
        </header>

        {locked && <PageNotice notice={lockedNotice(locked, openChat)} />}
        {error && (
          <PageNotice
            notice={{
              kind: 'problem',
              title: error,
              text: Object.keys(problems).length ? 'They are marked below.' : undefined,
            }}
          />
        )}

        <div className="editor-columns" key={revision}>
          <div className="editor-main">
            <Field
              id="component-name"
              label="Name"
              note="Only seen here in the panel."
              error={problem('name')}
              value={draft.name}
              disabled={!editable}
              onChange={(event) => change({ name: event.target.value })}
            />
            <section className="editor-section" aria-label="Content">
              <h2>Content</h2>
              <BlocksEditor
                blocks={draft.blocks as Block[]}
                onChange={(blocks) => change({ blocks })}
                editable={editable}
                problem={problem}
              />
            </section>
          </div>

          <aside className="editor-side">
            <section className="editor-section" aria-label="Where it appears">
              <h2>Where it appears</h2>
              <p className="editor-note">{whereText(where, list)}.</p>
              <div className="components-choice" role="radiogroup" aria-label="Where it appears">
                <label className="components-option" data-checked={draft.everywhere}>
                  <input
                    type="radio"
                    name="where"
                    checked={draft.everywhere}
                    disabled={!editable}
                    onChange={() => change({ everywhere: true })}
                  />
                  <span>
                    <span className="components-option-name">On every page</span>
                    <span className="components-option-help">The whole site, where the site puts it</span>
                  </span>
                </label>
                <label className="components-option" data-checked={!draft.everywhere}>
                  <input
                    type="radio"
                    name="where"
                    checked={!draft.everywhere}
                    disabled={!editable}
                    onChange={() => change({ everywhere: false })}
                  />
                  <span>
                    <span className="components-option-name">Only on some pages</span>
                    <span className="components-option-help">Chosen one by one, or all the pages inside a page</span>
                  </span>
                </label>
              </div>
              {whereProblem && <p className="field-error">{sentence(whereProblem)}</p>}
              {!draft.everywhere && (
                <ul className="components-pages" aria-label="Pages">
                  {list.map((page) => (
                    <li key={page.id} style={{ paddingLeft: `${(depth.get(page.id) ?? 0) * 16}px` }}>
                      <span className="components-page-title">{page.title}</span>
                      <label className="editor-check">
                        <input
                          type="checkbox"
                          checked={where.pages.includes(page.id)}
                          disabled={!editable}
                          onChange={(event) => toggle('pages', page.id, event.target.checked)}
                        />
                        This page
                      </label>
                      {parents.has(page.id) && (
                        <label className="editor-check">
                          <input
                            type="checkbox"
                            checked={where.inside.includes(page.id)}
                            disabled={!editable}
                            onChange={(event) => toggle('inside', page.id, event.target.checked)}
                          />
                          Pages inside
                        </label>
                      )}
                    </li>
                  ))}
                </ul>
              )}
            </section>

            <section className="editor-section" aria-label="Delete">
              <Button className="pages-delete" variant="quiet" small disabled={!editable} onClick={() => setDeleting(true)}>
                Delete the component
              </Button>
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
      {deleting && (
        <Dialog title={`Delete the component “${draft.name}”?`} onClose={() => setDeleting(false)}>
          <p className="dialog-text">
            It disappears from every page that shows it. The change goes online with the next
            publication, and an earlier version can bring it back.
          </p>
          <DialogActions>
            <Button onClick={() => setDeleting(false)} autoFocus>
              Keep it
            </Button>
            <Button variant="danger" onClick={remove}>
              Delete
            </Button>
          </DialogActions>
        </Dialog>
      )}
    </section>
  )
}
