import { useEffect, useRef, useState } from 'react'
import { auth } from '../api'
import { isSettings, type SettingsSection } from '../settings/sections'
import { Icon, type IconName } from './icons'
import './NavRail.css'

export type Section = 'website' | 'pages' | 'components' | 'media' | SettingsSection

const WEBSITE_COOKIE = 'damasco_rail_website'

/** Whether the website's group was left open, as the cookie remembers it:
 * closed until it is opened. */
function storedOpen(): boolean {
  return document.cookie.split('; ').includes(`${WEBSITE_COOKIE}=open`)
}

function storeOpen(open: boolean) {
  document.cookie = `${WEBSITE_COOKIE}=${open ? 'open' : 'closed'}; Path=/; Max-Age=31536000; SameSite=Lax`
}

interface NavRailProps {
  current: Section
  onNavigate: (section: Section) => void
  /** The sections of the website to show: those with something in them,
   * and Media. */
  website: ('pages' | 'components' | 'media')[]
  /** The sections of settings the person may open, first one first. */
  settings: SettingsSection[]
  onSignedOut: () => void
}

const websiteItems: Record<'pages' | 'components' | 'media', { label: string; icon: IconName }> = {
  pages: { label: 'Pages', icon: 'pages' },
  components: { label: 'Components', icon: 'components' },
  media: { label: 'Media', icon: 'media' },
}

/** Section navigation on computers: a vertical rail (phones have the TabBar).
 * The website's sections fold into the globe; settings sit at the bottom. */
export function NavRail({ current, onNavigate, website, settings, onSignedOut }: NavRailProps) {
  const [open, setOpen] = useState(storedOpen)
  const folds = website.length > 0
  const expanded = folds && open
  const inWebsite = current === 'website' || current === 'pages' || current === 'components' || current === 'media'

  // The globe shows the site; on the site it opens and closes its group.
  const onGlobe = () => {
    if (folds && current === 'website') {
      setOpen(!open)
      storeOpen(!open)
    }
    onNavigate('website')
  }

  const globe = (
    <button
      className="rail-item"
      aria-label="Website"
      title="Website"
      aria-expanded={folds ? expanded : undefined}
      aria-current={current === 'website' || (!expanded && inWebsite) ? 'page' : undefined}
      onClick={onGlobe}
    >
      <Icon name="website" />
    </button>
  )

  return (
    <nav className="rail" aria-label="Sections">
      {/* The globe stays in its group, open or closed, so it never moves. */}
      <div className="rail-group" role="group" aria-label="Website" data-open={expanded}>
        {globe}
        {expanded &&
          website.map((id) => (
            <button
              key={id}
              className="rail-item"
              aria-label={websiteItems[id].label}
              title={websiteItems[id].label}
              aria-current={id === current ? 'page' : undefined}
              onClick={() => onNavigate(id)}
            >
              <Icon name={websiteItems[id].icon} />
            </button>
          ))}
      </div>
      <span className="rail-spacer" />
      {settings.length > 0 && (
        <button
          className="rail-item"
          aria-label="Settings"
          title="Settings"
          aria-current={isSettings(current) ? 'page' : undefined}
          onClick={() => onNavigate(settings[0])}
        >
          <Icon name="settings" />
        </button>
      )}
      <AccountMenu onSignedOut={onSignedOut} />
    </nav>
  )
}
function AccountMenu({ onSignedOut }: { onSignedOut: () => void }) {
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
        aria-label="Account"
        title="Owner"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        <Icon name="user" />
      </button>
      {open && (
        <div className="account-menu" role="menu" aria-label="Account">
          <div className="account-who">
            <span className="account-name">Owner</span>
          </div>
          <button className="account-item" role="menuitem" onClick={signOut}>
            Sign out
          </button>
        </div>
      )}
    </div>
  )
}
