import { useCallback, useEffect, useRef, useState, type SubmitEvent } from 'react'
import { ApiError, people, type Me, type Person, type Role } from '../api'
import { Dialog, DialogActions } from '../components/Dialog'
import { Field } from '../components/Field'
import { Icon } from '../components/icons'
import { sentence } from '../text'
import { Button } from '../components/Button'
import { useNotices } from '../notices/context'
import '../components/Page.css'
import './People.css'

const roleNames: Record<Role, string> = { owner: 'Owner', admin: 'Administrator', editor: 'Editor' }

const roleHelp: Record<'admin' | 'editor', string> = {
  admin: "Everything the owner can do, except changing the owner's account.",
  editor: 'Edits content and media, sees the preview and publishes. No chat, people or settings.',
}

const date = new Intl.DateTimeFormat(undefined, { day: 'numeric', month: 'short', year: 'numeric' })

function initials(name: string | null): string {
  const parts = (name ?? '').trim().split(/\s+/).filter(Boolean)
  const letters = parts.length > 1 ? [parts[0], parts[parts.length - 1]] : parts
  return letters.map((part) => part[0]?.toUpperCase() ?? '').join('') || '?'
}

type Open =
  | { kind: 'invite' }
  | { kind: 'link'; person: Person; link: string; invitation: boolean }
  | { kind: 'revoke'; person: Person }
  | null

/** People with access to the panel: invitations, roles, links, revocation. */
export function PeoplePage({ me }: { me: Me }) {
  const [list, setList] = useState<Person[] | null>(null)
  const { notify } = useNotices()
  const [open, setOpen] = useState<Open>(null)

  const load = useCallback(() => {
    people
      .list()
      .then(setList, (err: unknown) =>
        notify({ kind: 'problem', title: 'Couldn’t read the people', text: String(err) }),
      )
  }, [notify])
  useEffect(load, [load])

  const act = async (work: () => Promise<void>) => {
    try {
      await work()
    } catch (err) {
      notify({
        kind: 'problem',
        title: 'That didn’t work',
        text: err instanceof Error ? sentence(err.message) : String(err),
      })
    }
    load()
  }

  const newLink = (person: Person) =>
    act(async () => {
      const { link, purpose } = await people.link(person.id)
      setOpen({ kind: 'link', person, link, invitation: purpose === 'setup' })
    })

  return (
    <section className="page-frame" aria-labelledby="people-title">
      <div className="page-card">
        <header className="page-head">
          <div>
            <h1 id="people-title">People</h1>
            <p>Who can open this panel, and with which role.</p>
          </div>
          <Button variant="primary" icon="plus" onClick={() => setOpen({ kind: 'invite' })}>
            Invite
          </Button>
        </header>

        <ul className="people-list">
          {list?.map((person) => (
            <PersonRow
              key={person.id}
              person={person}
              me={me}
              onRole={(role) => act(() => people.setRole(person.id, role))}
              onLink={() => newLink(person)}
              onRevoke={() => setOpen({ kind: 'revoke', person })}
            />
          ))}
        </ul>
      </div>

      {open?.kind === 'invite' && (
        <InviteDialog
          onClose={() => setOpen(null)}
          onInvited={(person, link) => {
            load()
            setOpen({ kind: 'link', person, link, invitation: true })
          }}
        />
      )}
      {open?.kind === 'link' && (
        <LinkDialog
          person={open.person}
          link={open.link}
          invitation={open.invitation}
          onClose={() => setOpen(null)}
        />
      )}
      {open?.kind === 'revoke' && (
        <RevokeDialog
          person={open.person}
          onClose={() => setOpen(null)}
          onRevoke={() => {
            setOpen(null)
            act(() => people.revoke(open.person.id))
          }}
        />
      )}
    </section>
  )
}

interface PersonRowProps {
  person: Person
  me: Me
  onRole: (role: Role) => void
  onLink: () => void
  onRevoke: () => void
}

function PersonRow({ person, me, onRole, onLink, onRevoke }: PersonRowProps) {
  const self = person.id === me.id
  const manageable = !self && person.role !== 'owner' && person.status !== 'revoked'
  return (
    <li className="person" data-status={person.status}>
      <span className="person-avatar" aria-hidden="true">
        {person.status === 'invited' ? <Icon name="user" size={16} /> : initials(person.name)}
      </span>
      <span className="person-who">
        <span className="person-name">
          {person.name ?? 'Unnamed'}
          {self && <span className="person-tag">You</span>}
        </span>
        <span className="person-meta">
          {person.status === 'invited'
            ? `Invited ${date.format(new Date(person.created_at))} · link not used yet`
            : person.status === 'revoked'
              ? `${person.email ?? ''} · access revoked`
              : person.email}
        </span>
      </span>
      <span className="person-role">{roleNames[person.role]}</span>
      {manageable ? (
        <PersonMenu person={person} onRole={onRole} onLink={onLink} onRevoke={onRevoke} />
      ) : (
        <span className="person-menu-space" aria-hidden="true" />
      )}
    </li>
  )
}

function PersonMenu({ person, onRole, onLink, onRevoke }: Omit<PersonRowProps, 'me'>) {
  const [open, setOpen] = useState(false)
  const root = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    const close = (event: MouseEvent | KeyboardEvent) => {
      if (event instanceof KeyboardEvent ? event.key === 'Escape' : !root.current?.contains(event.target as Node)) {
        setOpen(false)
      }
    }
    document.addEventListener('mousedown', close)
    document.addEventListener('keydown', close)
    return () => {
      document.removeEventListener('mousedown', close)
      document.removeEventListener('keydown', close)
    }
  }, [open])

  const choose = (action: () => void) => () => {
    setOpen(false)
    action()
  }
  const other: Role = person.role === 'admin' ? 'editor' : 'admin'

  return (
    <div className="person-menu" ref={root}>
      <button
        className="icon-button"
        aria-label={`Actions for ${person.name ?? 'this person'}`}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        <Icon name="more" size={18} />
      </button>
      {open && (
        <div className="menu" role="menu">
          <button role="menuitem" onClick={choose(() => onRole(other))}>
            Make {roleNames[other].toLowerCase()}
          </button>
          <button role="menuitem" onClick={choose(onLink)}>
            {person.status === 'invited' ? 'New invitation link' : 'New password link'}
          </button>
          <button role="menuitem" className="menu-danger" onClick={choose(onRevoke)}>
            Revoke access
          </button>
        </div>
      )}
    </div>
  )
}

function InviteDialog({
  onClose,
  onInvited,
}: {
  onClose: () => void
  onInvited: (person: Person, link: string) => void
}) {
  const [name, setName] = useState('')
  const [role, setRole] = useState<'admin' | 'editor'>('editor')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const submit = async (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!name.trim()) {
      setError('Required')
      return
    }
    setBusy(true)
    try {
      const { id, link } = await people.invite(name.trim(), role)
      const person: Person = { id, name: name.trim(), email: null, role, status: 'invited', created_at: new Date().toISOString() }
      onInvited(person, link)
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err))
      setBusy(false)
    }
  }

  return (
    <Dialog title="Invite a person" onClose={onClose}>
      <form className="people-form" onSubmit={submit} noValidate>
        <Field
          id="invite-name"
          label="Name"
          note="The person can change it"
          autoFocus
          maxLength={80}
          error={error}
          value={name}
          onChange={(event) => {
            setName(event.target.value)
            setError(null)
          }}
        />
        <fieldset className="role-choice">
          <legend>Role</legend>
          {(['editor', 'admin'] as const).map((value) => (
            <label key={value} className="role-option" data-checked={role === value}>
              <input
                type="radio"
                name="role"
                value={value}
                checked={role === value}
                onChange={() => setRole(value)}
              />
              <span className="role-option-name">{roleNames[value]}</span>
              <span className="role-option-help">{roleHelp[value]}</span>
            </label>
          ))}
        </fieldset>
        <DialogActions>
          <Button onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" type="submit" disabled={busy}>
            Create the link
          </Button>
        </DialogActions>
      </form>
    </Dialog>
  )
}

function LinkDialog({
  person,
  link,
  invitation,
  onClose,
}: {
  person: Person
  link: string
  invitation: boolean
  onClose: () => void
}) {
  const [copied, setCopied] = useState(false)
  const input = useRef<HTMLInputElement>(null)

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(link)
      setCopied(true)
    } catch {
      input.current?.select()
    }
  }

  const who = person.name ?? 'the person'
  return (
    <Dialog title={invitation ? 'Invitation link' : 'New password link'} onClose={onClose}>
      <p className="people-text">
        {invitation
          ? `Send this link to ${who}: it opens the page to create the account. It works once, for 7 days.`
          : `Send this link to ${who}: it opens the page to choose a new password and signs out every open session. It works once, for 24 hours.`}
      </p>
      <div className="link-box">
        <input ref={input} readOnly value={link} aria-label="Link" onFocus={(e) => e.target.select()} />
        <Button onClick={copy} autoFocus>
          <Icon name={copied ? 'check' : 'copy'} size={16} />
          <span className="link-copy-label">{copied ? 'Copied' : 'Copy'}</span>
        </Button>
      </div>
      <p className="people-text people-note">
        axioma sends no email: the link travels by any channel, by hand. Earlier links for{' '}
        {who} no longer work.
      </p>
      <DialogActions>
        <Button variant="primary" onClick={onClose}>
          Done
        </Button>
      </DialogActions>
    </Dialog>
  )
}

function RevokeDialog({
  person,
  onClose,
  onRevoke,
}: {
  person: Person
  onClose: () => void
  onRevoke: () => void
}) {
  return (
    <Dialog title={`Revoke access for ${person.name ?? 'this person'}?`} onClose={onClose}>
      <p className="people-text">
        Open sessions close now and pending links stop working. The account stays in the list as
        revoked, and its actions stay in the activity log.
      </p>
      <DialogActions>
        <Button onClick={onClose} autoFocus>
          Cancel
        </Button>
        <Button variant="danger" onClick={onRevoke}>
          Revoke access
        </Button>
      </DialogActions>
    </Dialog>
  )
}
