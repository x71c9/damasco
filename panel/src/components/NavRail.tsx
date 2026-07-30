import { Icon, type IconName } from './icons'
import './NavRail.css'

const sections: { id: string; label: string; icon: IconName }[] = [
  { id: 'website', label: 'Website', icon: 'website' },
  { id: 'pages', label: 'Pages', icon: 'pages' },
  { id: 'components', label: 'Components', icon: 'components' },
  { id: 'media', label: 'Media', icon: 'media' },
]

interface NavRailProps {
  current: string
}

/** Section navigation: a vertical rail on computers, a bottom bar on phones. */
export function NavRail({ current }: NavRailProps) {
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
      <button className="rail-avatar" aria-label="Account" title="Account">
        <Icon name="user" size={18} />
      </button>
    </nav>
  )
}
