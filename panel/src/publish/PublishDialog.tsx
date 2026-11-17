import { useEffect, useMemo, useState, type ReactNode } from 'react'
import {
  ApiError,
  changes as changesApi,
  media as mediaApi,
  type ChangeStatus,
  type PendingItem,
  type PublishPlan,
} from '../api'
import { useChanges, when } from '../changes/context'
import { useNotices } from '../notices/context'
import { Button } from '../components/Button'
import { Dialog, DialogActions } from '../components/Dialog'
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
  /** Opens the dialog again, from a notification. */
  onReopen?: () => void
}

type Stage = { kind: 'list' } | { kind: 'back' }

/** What goes online with the next publication, as the design draws it:
 * the assistant's work on the code as one row, then pages and pictures,
 * each with its tick. Everything ticked goes online as the preview shows
 * it; part of it is composed and built apart first, with what it needs. */
export function PublishDialog({ site, siteUrl, canPublish, onClose, onReopen }: PublishDialogProps) {
  const { changes, refresh } = useChanges()
  const { notify, update } = useNotices()
  const [problem, setProblem] = useState<string | null>(null)
  const [stage, setStage] = useState<Stage>({ kind: 'list' })
  // Keys left out; everything is ticked at first.
  const [unticked, setUnticked] = useState<Set<string>>(new Set())
  // The plan of a selection, with the ticks it was asked for.
  const [plan, setPlan] = useState<{ ticks: string; plan: PublishPlan } | null>(null)

  const pending = changes?.pending
  const keys = useMemo(() => {
    if (!pending) return []
    return [
      ...(pending.files.length ? ['code'] : []),
      ...pending.pages.map((item) => `page:${item.id}`),
      ...pending.media.map((item) => `media:${item.id}`),
      ...pending.components.map((item) => `component:${item.id}`),
    ]
  }, [pending])
  const ticked = keys.filter((key) => !unticked.has(key))
  const partial = ticked.length > 0 && ticked.length < keys.length
  const head = pending?.head ?? null

  // What a partial selection needs, asked again as the ticks change.
  const tickedKey = ticked.join(' ')
  useEffect(() => {
    if (!partial || !head) return
    let gone = false
    const timer = window.setTimeout(() => {
      changesApi.plan(head, tickedKey.split(' ')).then(
        (found) => {
          if (!gone) setPlan({ ticks: tickedKey, plan: found })
        },
        (err: unknown) => {
          if (!gone) setProblem(err instanceof ApiError ? sentence(err.message) : String(err))
        },
      )
    }, 200)
    return () => {
      gone = true
      window.clearTimeout(timer)
    }
  }, [partial, head, tickedKey])
  const shownPlan = partial && plan?.ticks === tickedKey ? plan.plan : null
  const included = new Map((shownPlan?.included ?? []).map((item) => [item.key, item.reason]))

  const where = site ? (
    <>
      on <span className="publish-domain">{site}</span>
    </>
  ) : null

  if (!changes || !pending) {
    return (
      <Dialog title="Publish" className="publish-dialog" onClose={onClose}>
        <p className="dialog-text">Reading the changes…</p>
      </Dialog>
    )
  }

  const { people, blocked, count } = changes
  const last = changes.last_publication
  // "by Giulia Mancini today at 18:21", or only when.
  const lastBy = last
    ? (() => {
        const name = last.by === null ? null : people[String(last.by)]
        const at = when(Date.parse(last.at) / 1000)
        return name ? `by ${name} ${at}` : at
      })()
    : null
  const reason = !canPublish ? 'Publishing is not part of your role.' : blocked ? `${sentence(blocked)}.` : null

  const view = siteUrl ? { label: 'View the site', href: siteUrl } : undefined
  const goBack = async () => {
    onClose()
    const id = notify({ kind: 'working', title: 'Putting back the previous publication…' })
    try {
      await changesApi.back()
      update(id, {
        kind: 'done',
        title: 'Previous publication online',
        text: 'What came after it waits in the list.',
        action: view,
      })
    } catch (err) {
      update(id, {
        kind: 'problem',
        title: 'Couldn’t put it back',
        text: err instanceof ApiError ? sentence(err.message) : String(err),
      })
    }
    refresh()
  }

  if (stage.kind === 'back') {
    return (
      <Dialog title="Put back the previous publication?" className="publish-dialog" onClose={onClose}>
        <p className="dialog-text">
          The site goes back online as it was before the last publication{lastBy ? `, made ${lastBy}` : ''}.
          Nothing is lost: what came after it waits in the list, to publish again.
        </p>
        {problem && (
          <p className="publish-summary publish-summary-problem" role="alert">
            {problem}
          </p>
        )}
        <DialogActions>
          <Button onClick={() => setStage({ kind: 'list' })} autoFocus>
            Cancel
          </Button>
          <Button variant="primary" disabled={Boolean(reason)} onClick={goBack}>
            Put it back
          </Button>
        </DialogActions>
      </Dialog>
    )
  }

  const earlier =
    last || changes.previous_publication ? (
      <p className="publish-history">
        {lastBy ? `Last published ${lastBy}.` : null}
        {changes.previous_publication && canPublish && (
          <button type="button" className="publish-history-back" onClick={() => setStage({ kind: 'back' })}>
            Put back the previous one
          </button>
        )}
      </p>
    ) : null

  if (count === 0) {
    return (
      <Dialog title="Publish" subtitle={<>Changes go online {where}</>} className="publish-dialog" onClose={onClose}>
        <p className="dialog-text">
          Everything is online. Changes made in the chat or in the panel wait here until they are
          published.
        </p>
        {earlier}
        <DialogActions>
          <Button onClick={onClose} autoFocus>
            Close
          </Button>
        </DialogActions>
      </Dialog>
    )
  }

  const toggle = (key: string) => {
    setProblem(null)
    setUnticked((current) => {
      const next = new Set(current)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })
  }

  const going = shownPlan ? shownPlan.keys.length : ticked.length
  const titleOf = (key: string) => {
    if (key === 'code') return 'the assistant’s changes'
    const [kind, id] = key.split(':')
    const list = kind === 'page' ? pending.pages : kind === 'media' ? pending.media : pending.components
    const item = list.find((i) => i.id === id)
    return `“${item?.title || 'Untitled'}”`
  }
  const waiting = keys.filter((key) => unticked.has(key) && !included.has(key))
  const problems = shownPlan?.problems ?? []
  const planning = partial && !shownPlan

  const publish = async () => {
    if (!head) return
    const items = partial ? ticked : undefined
    onClose()
    const id = notify({
      kind: 'working',
      title: 'Publishing…',
      text: partial ? 'Usually under a minute. The panel stays usable.' : 'The panel stays usable.',
    })
    try {
      const result = await changesApi.publish(head, items)
      update(id, {
        kind: 'done',
        title: 'Site published',
        text: `${result.changes} ${result.changes === 1 ? 'change is' : 'changes are'} online.`,
        action: view,
      })
    } catch (err) {
      update(id, {
        kind: 'problem',
        title: 'Couldn’t publish',
        text: err instanceof ApiError ? sentence(err.message) : String(err),
        action: onReopen ? { label: 'Open the list', run: onReopen } : undefined,
      })
    }
    refresh()
  }

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

  let summary: ReactNode
  if (problem) summary = problem
  else if (reason) summary = reason
  else if (ticked.length === 0) summary = 'Tick at least one change.'
  else if (problems.length) summary = `These cannot go online by themselves: ${problems[0]}.`
  else if (partial)
    summary = (
      <>
        <strong>
          {going} of {keys.length} ticked.
        </strong>{' '}
        {waiting.length === 1
          ? `${sentence(titleOf(waiting[0]))} stays waiting`
          : `${waiting.length} changes stay waiting`}
        ; a fresh preview is made first.
      </>
    )
  else
    summary = (
      <>
        <strong>
          {count} {count === 1 ? 'change' : 'changes'}.
        </strong>{' '}
        They go online as they are in the preview.
      </>
    )

  const row = (key: string) => ({
    checked: !unticked.has(key) || included.has(key),
    forced: included.get(key) ?? null,
    onToggle: () => toggle(key),
  })

  return (
    <Dialog title="Publish" subtitle={<>Ticked changes go online {where}</>} className="publish-dialog" onClose={onClose}>
      <div className="publish-groups">
        {pending.files.length > 0 && (
          <section className="publish-group" aria-labelledby="publish-code">
            <h3 id="publish-code">Made by the assistant</h3>
            <ul>
              <Row
                {...row('code')}
                title={asked.length ? asked.join(', ') : 'Layout, style and code of the site'}
                clip
                meta={`${askers.length ? `Asked by ${askers.join(', ')}` : latest ? when(latest.at) : 'Code of the site'} · go online together`}
                state="Changed"
              />
            </ul>
          </section>
        )}

        {pending.pages.length > 0 && (
          <section className="publish-group" aria-labelledby="publish-pages">
            <h3 id="publish-pages">Pages</h3>
            <ul>
              {pending.pages.map((item) => (
                <Row key={item.id} {...row(`page:${item.id}`)} {...describe(item, people, false)} />
              ))}
            </ul>
          </section>
        )}

        {pending.components.length > 0 && (
          <section className="publish-group" aria-labelledby="publish-components">
            <h3 id="publish-components">Components</h3>
            <ul>
              {pending.components.map((item) => (
                <Row
                  key={item.id}
                  {...row(`component:${item.id}`)}
                  {...describe(item, people, false)}
                  title={item.title || 'Unnamed component'}
                />
              ))}
            </ul>
          </section>
        )}

        {pending.media.length > 0 && (
          <section className="publish-group" aria-labelledby="publish-media">
            <h3 id="publish-media">Media</h3>
            <ul>
              {pending.media.map((item) => (
                <Row key={item.id} {...row(`media:${item.id}`)} {...describe(item, people, true)} />
              ))}
            </ul>
          </section>
        )}
      </div>
      {earlier}

      <DialogActions>
        <span
          className={`publish-summary${problem || problems.length ? ' publish-summary-problem' : ''}`}
          role={problem ? 'alert' : undefined}
        >
          {summary}
        </span>
        <Button onClick={onClose}>Cancel</Button>
        <button
          type="button"
          className="publish-confirm"
          disabled={Boolean(reason) || !head || ticked.length === 0 || planning || problems.length > 0}
          onClick={publish}
        >
          {partial ? 'Preview and publish' : 'Publish'}
          <span className="publish-confirm-count">{going}</span>
        </button>
      </DialogActions>
    </Dialog>
  )
}

/** The parts of a row of a page or a picture. */
function describe(item: PendingItem, people: Record<string, string>, picture: boolean) {
  const deleted = item.status === 'deleted'
  return {
    title: item.title || (picture ? 'Unnamed picture' : 'Untitled page'),
    meta: byLine(item.by, item.at, people),
    state: labels[item.status],
    deleted,
    thumb: picture ? (deleted ? '' : mediaApi.fileUrl(item.id)) : undefined,
  }
}

interface RowProps {
  title: string
  meta: string
  state: string
  checked: boolean
  /** Why it is ticked whatever the person chooses: another change needs it. */
  forced: string | null
  onToggle: () => void
  deleted?: boolean
  clip?: boolean
  /** A picture's thumbnail; empty for a deleted one. */
  thumb?: string
}

function Row({ title, meta, state, checked, forced, onToggle, deleted = false, clip = false, thumb }: RowProps) {
  return (
    <li className="publish-row">
      <label className={`publish-pick${forced ? ' publish-pick-forced' : ''}`}>
        <input type="checkbox" checked={checked} disabled={Boolean(forced)} onChange={onToggle} />
        <span className="publish-box" aria-hidden="true">
          {checked && (
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
              <path d="M20 6 9 17l-5-5" />
            </svg>
          )}
        </span>
        {thumb !== undefined &&
          (thumb ? (
            <img className="publish-thumb" src={thumb} alt="" loading="lazy" />
          ) : (
            <span className="publish-thumb" aria-hidden="true" />
          ))}
        <span className="publish-text">
          <span
            className={`publish-title${deleted ? ' publish-title-deleted' : ''}${clip ? ' publish-clip' : ''}`}
            title={clip ? title : undefined}
          >
            {title}
          </span>
          <span className="publish-meta">{forced ?? meta}</span>
        </span>
        <span className={`publish-state${deleted ? ' publish-state-deleted' : ''}`}>{state}</span>
      </label>
    </li>
  )
}
