import { useCallback, useEffect, useMemo, useState, type SubmitEvent } from 'react'
import { ApiError, pages, type PageSummary } from '../api'
import { Button } from '../components/Button'
import { Dialog, DialogActions } from '../components/Dialog'
import { Field } from '../components/Field'
import { Icon } from '../components/icons'
import { sentence } from '../text'
import '../components/Page.css'
import './Pages.css'

type Open = { kind: 'new'; inside: string | null } | { kind: 'delete'; page: PageSummary } | null

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

/** The pages of the site, in the order of the tree. */
export function PagesPage({ onOpen }: { onOpen: (id: string) => void }) {
  const [list, setList] = useState<PageSummary[] | null>(null)
  const [locked, setLocked] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [open, setOpen] = useState<Open>(null)

  const load = useCallback(() => {
    pages.list().then(
      (result) => {
        setList(result.pages)
        setLocked(result.locked)
      },
      (err: unknown) => setError(String(err)),
    )
  }, [])
  // The assistant changes pages too: the list follows.
  useEffect(() => {
    load()
    const timer = setInterval(load, 4000)
    return () => clearInterval(timer)
  }, [load])

  const depth = useMemo(() => depths(list ?? []), [list])
  const home = list?.find((page) => page.inside === null) ?? null

  const remove = async (page: PageSummary) => {
    setOpen(null)
    try {
      await pages.remove(page.id)
    } catch (err) {
      setError(err instanceof Error ? sentence(err.message) : String(err))
    }
    load()
  }

  return (
    <section className="page-frame" aria-labelledby="pages-title">
      <div className="page-card">
        <header className="page-head">
          <div>
            <h1 id="pages-title">Pages</h1>
            <p>Every page of the site, in the order of the site.</p>
          </div>
          <Button
            variant="primary"
            icon="plus"
            disabled={Boolean(locked) || !home}
            onClick={() => setOpen({ kind: 'new', inside: home?.id ?? null })}
          >
            New page
          </Button>
        </header>

        {locked && (
          <p className="pages-locked" role="status">
            <Icon name="history" size={16} />
            Pages are read-only while {locked}. They open again when it
            finishes.
          </p>
        )}

        {error && (
          <p className="page-toast" role="alert">
            {error}
            <button className="icon-button" aria-label="Dismiss" onClick={() => setError(null)}>
              <Icon name="close" size={16} />
            </button>
          </p>
        )}

        <ul className="pages-list" aria-label="Pages">
          {list?.map((page) => (
            <li
              key={page.id}
              className="pages-row"
              style={{ paddingLeft: `calc(${depth.get(page.id) ?? 0} * 24px + 8px)` }}
            >
              <button type="button" className="pages-text" onClick={() => onOpen(page.id)}>
                <span className="pages-title">
                  {page.title}
                  {!page.menu && <span className="pages-tag">Not in the menu</span>}
                  {page.type !== 'page' && <span className="pages-tag">{page.type}</span>}
                </span>
                <span className="pages-address">{page.address}</span>
              </button>
              <span className="pages-actions">
                <Button
                  variant="quiet"
                  small
                  icon="plus"
                  disabled={Boolean(locked)}
                  onClick={() => setOpen({ kind: 'new', inside: page.id })}
                >
                  Add inside
                </Button>
                {page.inside !== null && (
                  <Button
                    variant="quiet"
                    small
                    disabled={Boolean(locked)}
                    onClick={() => setOpen({ kind: 'delete', page })}
                  >
                    Delete
                  </Button>
                )}
              </span>
            </li>
          ))}
        </ul>
      </div>

      {open?.kind === 'new' && list && (
        <NewPageDialog
          list={list}
          depth={depth}
          inside={open.inside}
          onClose={() => setOpen(null)}
          onCreated={() => {
            setOpen(null)
            load()
          }}
        />
      )}
      {open?.kind === 'delete' && (
        <Dialog
          title={`Delete the page “${open.page.title}”?`}
          onClose={() => setOpen(null)}
        >
          <p className="dialog-text">
            Its address {open.page.address} stops working
            {open.page.menu ? ' and it leaves the site menu' : ''}. The change goes online
            with the next publication, and an earlier version can bring it back.
          </p>
          <DialogActions>
            <Button onClick={() => setOpen(null)} autoFocus>
              Keep it
            </Button>
            <Button variant="danger" onClick={() => remove(open.page)}>
              Delete
            </Button>
          </DialogActions>
        </Dialog>
      )}
    </section>
  )
}

function NewPageDialog({
  list,
  depth,
  inside,
  onClose,
  onCreated,
}: {
  list: PageSummary[]
  depth: Map<string, number>
  inside: string | null
  onClose: () => void
  onCreated: () => void
}) {
  const [title, setTitle] = useState('')
  const [parent, setParent] = useState(inside ?? '')
  const [menu, setMenu] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const submit = async (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!title.trim()) {
      setError('Give the page a title')
      return
    }
    setBusy(true)
    try {
      await pages.create({ title: title.trim(), inside: parent, menu })
      onCreated()
    } catch (err) {
      const problem = err instanceof ApiError ? (err.problems[0]?.message ?? err.message) : String(err)
      setError(problem)
      setBusy(false)
    }
  }

  return (
    <Dialog title="New page" onClose={onClose}>
      <form className="pages-form" onSubmit={submit} noValidate>
        <Field
          id="page-title"
          label="Title"
          autoFocus
          error={error}
          value={title}
          onChange={(event) => {
            setTitle(event.target.value)
            setError(null)
          }}
        />
        <div className="field">
          <div className="field-head">
            <label htmlFor="page-inside">Parent</label>
          </div>
          <select id="page-inside" value={parent} onChange={(event) => setParent(event.target.value)}>
            {list.map((page) => (
              <option key={page.id} value={page.id}>
                {' '.repeat(depth.get(page.id) ?? 0)}
                {page.title}
              </option>
            ))}
          </select>
        </div>
        <label className="pages-check">
          <input type="checkbox" checked={menu} onChange={(event) => setMenu(event.target.checked)} />
          Show it in the site menu
        </label>
        <DialogActions>
          <Button onClick={onClose}>Cancel</Button>
          <Button type="submit" variant="primary" disabled={busy}>
            Create the page
          </Button>
        </DialogActions>
      </form>
    </Dialog>
  )
}
