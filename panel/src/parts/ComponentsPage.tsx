import { useCallback, useEffect, useMemo, useState, type SubmitEvent } from 'react'
import { ApiError, components, pages, type ComponentSummary, type PageSummary } from '../api'
import { PendingMark } from '../changes/changes'
import { madeBy, useChanges } from '../changes/context'
import { Button } from '../components/Button'
import { Dialog, DialogActions } from '../components/Dialog'
import { Field } from '../components/Field'
import { PageNotice } from '../notices/Notices'
import { useNotices } from '../notices/context'
import { lockedNotice } from '../notices/locked'
import { whereText } from './where'
import '../components/Page.css'
import '../pages/Pages.css'
import './Components.css'

/** The parts several pages share, by name, with where each appears. */
export function ComponentsPage({ onOpen }: { onOpen: (id: string) => void }) {
  const [list, setList] = useState<ComponentSummary[] | null>(null)
  const [pageList, setPageList] = useState<PageSummary[]>([])
  const [locked, setLocked] = useState<string | null>(null)
  const [creating, setCreating] = useState(false)
  const { notify, openChat } = useNotices()
  const { changes, refresh } = useChanges()
  const pendingOf = useMemo(
    () => new Map((changes?.pending.components ?? []).map((item) => [item.id, item])),
    [changes],
  )
  const deleted = changes?.pending.components.filter((item) => item.status === 'deleted') ?? []

  const load = useCallback(() => {
    components.list().then(
      (result) => {
        setList(result.components)
        setLocked(result.locked)
      },
      (err: unknown) =>
        notify({ kind: 'problem', title: 'Couldn’t read the components', text: String(err) }),
    )
    pages.list().then((result) => setPageList(result.pages), () => {})
  }, [notify])
  // The assistant changes components too: the list follows.
  useEffect(() => {
    load()
    const timer = setInterval(load, 4000)
    return () => clearInterval(timer)
  }, [load])

  return (
    <section className="page-frame" aria-labelledby="components-title">
      <div className="page-card">
        <header className="page-head">
          <div>
            <h1 id="components-title">Components</h1>
            <p>Parts shared by several pages. A change here updates every page that uses them.</p>
          </div>
          <Button variant="primary" icon="plus" disabled={Boolean(locked)} onClick={() => setCreating(true)}>
            New component
          </Button>
        </header>

        {locked && <PageNotice notice={lockedNotice(locked, openChat)} />}

        {list && list.length === 0 && (
          <p className="components-empty">
            No components yet. Create one here, or ask the assistant to turn a part of the site, such
            as the footer, into one.
          </p>
        )}

        <ul className="pages-list" aria-label="Components">
          {list?.map((item) => {
            const pending = pendingOf.get(item.id)
            return (
              <li key={item.id} className="pages-row">
                <button type="button" className="pages-text" onClick={() => onOpen(item.id)}>
                  <span className="pages-title">
                    {item.name}
                    {pending && <PendingMark item={pending} />}
                  </span>
                  <span className="components-where">{whereText(item, pageList)}</span>
                </button>
              </li>
            )
          })}
        </ul>

        {deleted.length > 0 && (
          <section className="pages-deleted" aria-labelledby="components-deleted-title">
            <h2 id="components-deleted-title">Deleted, still online</h2>
            <p>These components stay on the site until the next publication.</p>
            <ul>
              {deleted.map((item) => (
                <li key={item.id}>
                  <span className="pages-deleted-title">{item.title || 'Unnamed component'}</span>
                  <span className="pages-deleted-when">Deleted {madeBy(item, changes?.people)}</span>
                </li>
              ))}
            </ul>
          </section>
        )}
      </div>

      {creating && (
        <NewComponentDialog
          onClose={() => setCreating(false)}
          onCreated={(id) => {
            setCreating(false)
            refresh()
            onOpen(id)
          }}
        />
      )}
    </section>
  )
}

function NewComponentDialog({ onClose, onCreated }: { onClose: () => void; onCreated: (id: string) => void }) {
  const [name, setName] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const submit = async (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!name.trim()) {
      setError('Give the component a name')
      return
    }
    setBusy(true)
    try {
      const created = await components.create({ name: name.trim() })
      onCreated(created.id)
    } catch (err) {
      const problem = err instanceof ApiError ? (err.problems[0]?.message ?? err.message) : String(err)
      setError(problem.charAt(0).toUpperCase() + problem.slice(1))
      setBusy(false)
    }
  }

  return (
    <Dialog title="New component" onClose={onClose}>
      <form className="pages-form" onSubmit={submit} noValidate>
        <Field
          id="component-name"
          label="Name"
          note="Only seen here in the panel."
          autoFocus
          error={error}
          value={name}
          onChange={(event) => {
            setName(event.target.value)
            setError(null)
          }}
        />
        <p className="dialog-text">
          It appears on every page at first. The assistant puts it in its place on the site, such
          as the bottom of the page for a footer.
        </p>
        <DialogActions>
          <Button onClick={onClose}>Cancel</Button>
          <Button type="submit" variant="primary" disabled={busy}>
            Create the component
          </Button>
        </DialogActions>
      </form>
    </Dialog>
  )
}
