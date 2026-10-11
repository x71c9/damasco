import { useEffect, useState } from 'react'
import { activity, type ActivityEntry } from '../api'
import { sentence } from '../text'
import { Button } from '../components/Button'
import '../components/Page.css'
import './Activity.css'

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

/** Plain-language description of an entry. The log stores codes and ids;
 * names are those of today, looked up when the page is read. */
function describe(entry: ActivityEntry): Described {
  const actor = entry.actor_name ?? 'axioma'
  const subject = entry.subject_name ?? 'a person not yet set up'
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
      return { who: entry.subject_name ?? 'Someone', what: 'failed to sign in: wrong password', notice: true }
    case 'session.logout':
      return { who: actor, what: 'signed out' }
    case 'person.invited':
      return { who: actor, what: `invited ${subject}` }
    case 'person.role_changed':
      return { who: actor, what: `changed the role of ${subject}` }
    case 'person.link_created':
      return { who: actor, what: `created a new one-time link for ${subject}` }
    case 'chat.taken_over':
      return { who: actor, what: `took over the chat from ${subject}` }
    case 'person.revoked':
      return { who: actor, what: `revoked the access of ${subject}` }
    case 'agent.signed_in':
      return { who: actor, what: "connected the assistant's Claude account" }
    case 'agent.signed_out':
      return { who: actor, what: "disconnected the assistant's Claude account" }
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
    default:
      return { who: actor, what: entry.action }
  }
}

/** The activity log: read-only, newest first, for owner and administrators. */
export function ActivityPage() {
  const [entries, setEntries] = useState<ActivityEntry[]>([])
  const [next, setNext] = useState<number | null>(null)
  const [loaded, setLoaded] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const load = (before?: number) =>
    activity.list(before).then(
      (page) => {
        setEntries((current) => (before === undefined ? page.entries : [...current, ...page.entries]))
        setNext(page.next)
        setLoaded(true)
      },
      (err: unknown) => setError(err instanceof Error ? sentence(err.message) : String(err)),
    )

  useEffect(() => {
    load()
  }, [])

  return (
    <section className="page-frame" aria-labelledby="activity-title">
      <div className="page-card">
        <header className="page-head">
          <div>
            <h1 id="activity-title">Activity</h1>
            <p>What happened on the site and in the panel, newest first. Entries are never changed or removed.</p>
          </div>
        </header>

        {error && (
          <p className="page-toast" role="alert">
            {error}
          </p>
        )}

        <table className="activity">
          <thead>
            <tr>
              <th scope="col">When</th>
              <th scope="col">Who</th>
              <th scope="col">What</th>
            </tr>
          </thead>
          <tbody>
            {entries.map((entry) => {
              const { who, what, notice } = describe(entry)
              return (
                <tr key={entry.id} data-notice={notice || undefined}>
                  <td className="activity-when">
                    <time dateTime={entry.at}>{when.format(new Date(entry.at))}</time>
                  </td>
                  <td className="activity-who">{who}</td>
                  <td className="activity-what">{what}</td>
                </tr>
              )
            })}
          </tbody>
        </table>

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
