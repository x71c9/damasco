import { useCallback, useEffect, useState } from 'react'
import { activity, type ActivityEntry } from '../api'
import { sentence } from '../text'
import { textOf } from '../assistants'
import { Button } from '../components/Button'
import '../components/Page.css'
import './Activity.css'
import { useNotices } from '../notices/context'

const when = new Intl.DateTimeFormat(undefined, {
  day: 'numeric',
  month: 'short',
  year: 'numeric',
  hour: '2-digit',
  minute: '2-digit',
})

interface Described {
  who: string
  what: string
  /** Something worth a second look, such as a failed sign-in. */
  notice?: boolean
}

/** Plain-language description of an entry. An entry with an actor comes
 * from the owner, the only account; one without, from alogeno itself. */
function describe(entry: ActivityEntry): Described {
  const actor = entry.actor_id !== null ? 'The owner' : 'alogeno'
  switch (entry.action) {
    case 'owner.link_created':
      return { who: actor, what: "created a one-time link for the owner's account" }
    case 'person.setup':
      return { who: actor, what: 'created the account' }
    case 'person.password_reset':
      return { who: actor, what: 'set a new password' }
    case 'session.login':
      return { who: actor, what: 'signed in' }
    case 'session.login_failed':
      return { who: 'Someone', what: 'failed to sign in: wrong password', notice: true }
    case 'session.logout':
      return { who: actor, what: 'signed out' }
    case 'content.page_created':
      return { who: actor, what: 'created a page' }
    case 'content.page_changed':
      return { who: actor, what: 'changed a page' }
    case 'content.page_deleted':
      return { who: actor, what: 'deleted a page' }
    case 'content.component_created':
      return { who: actor, what: 'created a component' }
    case 'content.component_changed':
      return { who: actor, what: 'changed a component' }
    case 'content.component_deleted':
      return { who: actor, what: 'deleted a component' }
    case 'content.media_added':
      return { who: actor, what: 'uploaded a picture' }
    case 'content.media_changed':
      return { who: actor, what: 'described a picture' }
    case 'content.media_removed':
      return { who: actor, what: 'removed a picture' }
    case 'site.published':
      return { who: actor, what: 'published the site' }
    case 'site.went_back':
      return { who: actor, what: 'put the previous publication back online' }
    case 'chat.went_back':
      return { who: actor, what: 'took the site back to before a message' }
    case 'chat.undid_going_back':
      return { who: actor, what: 'undid going back' }
    case 'terminal.opened':
      return { who: actor, what: 'opened the terminal' }
    case 'terminal.closed':
      return { who: actor, what: 'closed the terminal' }
    case 'chat.taken_over':
      return { who: actor, what: 'took over the chat' }
    case 'agent.signed_in':
      return { who: actor, what: `connected ${assistantOf(entry.subject)}` }
    case 'agent.signed_out':
      return { who: actor, what: `disconnected ${assistantOf(entry.subject)}` }
    case 'agent.switched': {
      const [from, to] = (entry.subject?.replace(/^assistant:/, '') ?? '').split('>')
      return {
        who: actor,
        what: `switched the assistant from ${textOf(from).name} to ${textOf(to).name}`,
      }
    }
    case 'agent.permission_allowed':
      return { who: actor, what: 'allowed an action of the assistant' }
    case 'agent.permission_denied':
      return { who: actor, what: 'refused an action of the assistant' }
    case 'agent.changes_saved':
      return { who: actor, what: "saved the assistant's changes as a new version" }
    case 'sandbox.host_allowed':
      return {
        who: actor,
        what: `let the assistant reach ${entry.subject?.replace(/^host:/, '') ?? 'an address'}`,
      }
    case 'sandbox.host_removed':
      return {
        who: actor,
        what: `took back the assistant's access to ${entry.subject?.replace(/^host:/, '') ?? 'an address'}`,
      }
    case 'installation.update_requested':
      return {
        who: actor,
        what: `asked to update alogeno to ${entry.subject?.replace(/^version:/, '') ?? 'the latest version'}`,
      }
    default:
      return { who: actor, what: entry.action }
  }
}

/** The assistant an entry names; older entries, from before there was a
 * choice, are Claude's. */
function assistantOf(subject: string | null | undefined): string {
  return textOf(subject?.replace(/^assistant:/, '') || 'claude').name
}

/** The activity log: read-only, newest first. */
export function ActivityPage() {
  const [entries, setEntries] = useState<ActivityEntry[]>([])
  const [next, setNext] = useState<number | null>(null)
  const [loaded, setLoaded] = useState(false)
  const { notify } = useNotices()

  const load = useCallback(
    (before?: number) =>
    activity.list(before).then(
      (page) => {
        setEntries((current) => (before === undefined ? page.entries : [...current, ...page.entries]))
        setNext(page.next)
        setLoaded(true)
      },
      (err: unknown) =>
        notify({
          kind: 'problem',
          title: 'Couldn’t read the activity',
          text: err instanceof Error ? sentence(err.message) : String(err),
        }),
    ),
    [notify],
  )

  useEffect(() => {
    load()
  }, [load])

  return (
    <section className="page-frame" aria-labelledby="activity-title">
      <div className="page-card">
        <header className="page-head">
          <div>
            <h1 id="activity-title">Activity</h1>
            <p>What happened on the site and in the panel, newest first. Entries are never changed or removed.</p>
          </div>
        </header>

        <div className="activity-scroll">
          <table className="activity">
            <thead>
              <tr>
                <th scope="col">What</th>
                <th scope="col">Who</th>
                <th scope="col">When</th>
              </tr>
            </thead>
            <tbody>
              {entries.map((entry) => {
                const { who, what, notice } = describe(entry)
                return (
                  <tr key={entry.id} data-notice={notice || undefined}>
                    <td className="activity-what">{what}</td>
                    <td className="activity-who">{who}</td>
                    <td className="activity-when">
                      <time dateTime={entry.at}>{when.format(new Date(entry.at))}</time>
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>

        {loaded && entries.length === 0 && <p className="activity-empty">Nothing happened yet.</p>}
        {next !== null && (
          <Button className="activity-more" onClick={() => load(next)}>
            Show older entries
          </Button>
        )}
      </div>
    </section>
  )
}
