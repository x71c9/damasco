import { useEffect, useRef, useState } from 'react'
import { auth, type Me, type Role } from '../api'
import { Icon, type IconName } from './icons'
import './NavRail.css'

const sections: { id: string; label: string; icon: IconName }[] = [
  { id: 'website', label: 'Website', icon: 'website' },
  { id: 'pages', label: 'Pages', icon: 'pages' },
  { id: 'components', label: 'Components', icon: 'components' },
  { id: 'media', label: 'Media', icon: 'media' },
]

const roles: Record<Role, string> = { owner: 'Owner', admin: 'Administrator', editor: 'Editor' }

function initials(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean)
  const letters = parts.length > 1 ? [parts[0], parts[parts.length - 1]] : parts
  return letters.map((part) => part[0]?.toUpperCase() ?? '').join('') || '?'
}

interface NavRailProps {
  current: string
  me: Me
  onSignedOut: () => void
}

/** Section navigation: a vertical rail on computers, a bottom bar on phones. */
export function NavRail({ current, me, onSignedOut }: NavRailProps) {
  return (
    <nav className="rail" aria-label="Sections">
      <div className="rail-group" role="group" aria-label="Website">
        {sections.map(({ id, label, icon }) => (
          <button
            key={id}
            className="rail-item"
            aria-label={label}
            title={label}
            aria-current={id === current ? 'page' : undefined}
          >
            <Icon name={icon} />
          </button>
        ))}
      </div>
      <span className="rail-spacer" />
      <button className="rail-item" aria-label="Settings" title="Settings">
        <Icon name="settings" />
      </button>
      <AccountMenu me={me} onSignedOut={onSignedOut} />
    </nav>
  )
}

function AccountMenu({ me, onSignedOut }: { me: Me; onSignedOut: () => void }) {
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

  const signOut = async () => {
    try {
      await auth.logout()
    } finally {
      onSignedOut()
    }
  }

  return (
    <div className="account" ref={root}>
      <button
        className="rail-avatar"
        aria-label={`Account: ${me.name}`}
        title={me.name}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        {initials(me.name)}
      </button>
      {open && (
        <div className="account-menu" role="menu" aria-label="Account">
          <div className="account-who">
            <span className="account-name">{me.name}</span>
            <span className="account-meta">
              {me.email} · {roles[me.role]}
            </span>
          </div>
          <button className="account-item" role="menuitem" onClick={signOut}>
            Sign out
          </button>
        </div>
      )}
    </div>
  )
}
