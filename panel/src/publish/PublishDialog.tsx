import { useState } from 'react'
import { ApiError, changes as changesApi, media as mediaApi, type ChangeStatus, type PendingItem } from '../api'
import { useChanges, when } from '../changes/context'
import { Button } from '../components/Button'
import { Dialog, DialogActions } from '../components/Dialog'
import { Icon } from '../components/icons'
import { sentence } from '../text'
import './Publish.css'

const labels: Record<ChangeStatus, string> = {
  created: 'New',
  changed: 'Changed',
  deleted: 'Deleted',
}

/** "Giulia Mancini · today at 10:14", or only when for the site's first
 * version, which nobody asked for. */
function byLine(by: number | null, at: number, people: Record<string, string>): string {
  const name = by === null ? null : people[String(by)]
  return name ? `${name} · ${when(at)}` : when(at)
}

interface PublishDialogProps {
  /** The site's domain, as the dialog names it. */
  site: string | null
  siteUrl: string | null
  canPublish: boolean
  onClose: () => void
}

/** What goes online with the next publication, as the design draws it: the
 * assistant's work on the code as one row, then pages and pictures. All of
 * it goes online together, as it is in the preview. */
export function PublishDialog({ site, siteUrl, canPublish, onClose }: PublishDialogProps) {
  const { changes, refresh } = useChanges()
  const [busy, setBusy] = useState(false)
  const [problem, setProblem] = useState<string | null>(null)
  const [done, setDone] = useState<number | null>(null)

  const where = site ? (
    <>
      on <span className="publish-domain">{site}</span>
    </>
  ) : null

  if (done !== null) {
    return (
      <Dialog title="Published" subtitle={<>The site is online {where}</>} className="publish-dialog" onClose={onClose}>
        <p className="publish-done">
          <Icon name="circleCheck" size={20} strokeWidth={1.75} />
          {done} {done === 1 ? 'change is' : 'changes are'} online.
        </p>
        <DialogActions>
          {siteUrl && (
            <a className="button button-secondary" href={siteUrl} target="_blank" rel="noreferrer">
              Open the site
            </a>
          )}
          <Button variant="primary" onClick={onClose} autoFocus>
            Close
          </Button>
        </DialogActions>
      </Dialog>
    )
  }

  if (!changes) {
    return (
      <Dialog title="Publish" className="publish-dialog" onClose={onClose}>
        <p className="dialog-text">Reading the changes…</p>
      </Dialog>
    )
  }

  const { pending, people, blocked, count } = changes

  if (count === 0) {
    return (
      <Dialog title="Publish" subtitle={<>Changes go online {where}</>} className="publish-dialog" onClose={onClose}>
        <p className="dialog-text">
          Everything is online. Changes made in the chat or in the panel wait here until they are
          published.
        </p>
        <DialogActions>
          <Button onClick={onClose} autoFocus>
            Close
          </Button>
        </DialogActions>
      </Dialog>
    )
  }

  const publish = async () => {
    if (!pending.head) return
    setBusy(true)
    setProblem(null)
    try {
      const result = await changesApi.publish(pending.head)
      setDone(result.changes)
    } catch (err) {
      setProblem(err instanceof ApiError ? sentence(err.message) : String(err))
    }
    setBusy(false)
    refresh()
  }

  const reason = !canPublish ? 'Publishing is not part of your role.' : blocked ? `${sentence(blocked)}.` : null

  // The assistant's work on the code: what was asked, and by whom.
  const asked = pending.versions.map((version) => version.subject)
  const askers = [
    ...new Set(
      pending.versions
        .map((version) => (version.by === null ? null : people[String(version.by)]))
        .filter((name): name is string => Boolean(name)),
    ),
  ]
  const latest = pending.versions[0]

  return (
    <Dialog title="Publish" subtitle={<>Changes go online {where}</>} className="publish-dialog" onClose={onClose}>
      <div className="publish-groups">
        {pending.files.length > 0 && (
          <section className="publish-group" aria-labelledby="publish-code">
            <h3 id="publish-code">Made by the assistant</h3>
            <ul>
              <li className="publish-row">
                <span className="publish-text">
                  <span className="publish-title publish-clip" title={asked.join(', ')}>
                    {asked.length ? asked.join(', ') : 'Layout, style and code of the site'}
                  </span>
                  <span className="publish-meta">
                    {askers.length ? `Asked by ${askers.join(', ')}` : latest ? when(latest.at) : 'Code of the site'} · go
                    online together
                  </span>
                </span>
                <span className="publish-state">Changed</span>
              </li>
            </ul>
          </section>
        )}

        {pending.pages.length > 0 && (
          <section className="publish-group" aria-labelledby="publish-pages">
            <h3 id="publish-pages">Pages</h3>
            <ul>
              {pending.pages.map((item) => (
                <Row key={item.id} item={item} people={people} />
              ))}
            </ul>
          </section>
        )}

        {pending.media.length > 0 && (
          <section className="publish-group" aria-labelledby="publish-media">
            <h3 id="publish-media">Media</h3>
            <ul>
              {pending.media.map((item) => (
                <Row key={item.id} item={item} people={people} picture />
              ))}
            </ul>
          </section>
        )}
      </div>

      <DialogActions>
        <span className={`publish-summary${problem ? ' publish-summary-problem' : ''}`} role={problem ? 'alert' : undefined}>
          {problem ?? reason ?? (
            <>
              <strong>
                {count} {count === 1 ? 'change' : 'changes'}.
              </strong>{' '}
              They go online as they are in the preview.
            </>
          )}
        </span>
        <Button onClick={onClose}>Cancel</Button>
        <button
          type="button"
          className="publish-confirm"
          disabled={busy || Boolean(reason) || !pending.head}
          onClick={publish}
        >
          {busy ? 'Publishing…' : 'Publish'}
          <span className="publish-confirm-count">{count}</span>
        </button>
      </DialogActions>
    </Dialog>
  )
}

function Row({
  item,
  people,
  picture = false,
}: {
  item: PendingItem
  people: Record<string, string>
  picture?: boolean
}) {
  const deleted = item.status === 'deleted'
  return (
    <li className="publish-row">
      {picture &&
        (deleted ? (
          <span className="publish-thumb" aria-hidden="true" />
        ) : (
          <img className="publish-thumb" src={mediaApi.fileUrl(item.id)} alt="" loading="lazy" />
        ))}
      <span className="publish-text">
        <span className={`publish-title${deleted ? ' publish-title-deleted' : ''}`}>
          {item.title || (picture ? 'Unnamed picture' : 'Untitled page')}
        </span>
        <span className="publish-meta">{byLine(item.by, item.at, people)}</span>
      </span>
      <span className={`publish-state${deleted ? " publish-state-deleted" : ""}`}>{labels[item.status]}</span>
    </li>
  )
}
