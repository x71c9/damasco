import type { Theme } from '../theme'
import { Icon, Mark, type IconName } from './icons'
import './Header.css'

export type Device = 'computer' | 'tablet' | 'phone'
export type MobileView = 'chat' | 'preview'

const devices: { id: Device; label: string }[] = [
  { id: 'computer', label: 'Computer' },
  { id: 'tablet', label: 'Tablet' },
  { id: 'phone', label: 'Phone' },
]

interface HeaderProps {
  /** Website shows the preview controls; other sections show their name. */
  section: 'website' | 'pages' | 'activity' | 'people'
  siteName: string
  device: Device
  onDevice: (device: Device) => void
  onReload: () => void
  pending: number
  mobileView: MobileView
  onMobileView: (view: MobileView) => void
  theme: Theme
  onToggleTheme: () => void
}

export function Header({
  section,
  siteName,
  device,
  onDevice,
  onReload,
  pending,
  mobileView,
  onMobileView,
  theme,
  onToggleTheme,
}: HeaderProps) {
  const nextTheme = theme === 'dark' ? 'light' : 'dark'
  const website = section === 'website'
  return (
    <header className="header">
      <Mark size={24} label="axioma" />
      <button className="header-crumb header-site" aria-label={`Site: ${siteName}`}>
        {siteName}
        <span className="header-chevron">
          <Icon name="chevronDown" size={14} strokeWidth={2} />
        </span>
      </button>
      <span className="header-separator" aria-hidden="true">
        /
      </span>
      {website ? (
        <>
          <button className="header-crumb" aria-label="Page in the preview: Home. Change page">
            Home
            <span className="header-chevron">
              <Icon name="chevronDown" size={14} strokeWidth={2} />
            </span>
          </button>
          <button className="icon-button header-reload" aria-label="Reload the preview" title="Reload" onClick={onReload}>
            <Icon name="reload" size={16} />
          </button>
        </>
      ) : (
        <span className="header-section">
          {{ pages: 'Pages', people: 'People', activity: 'Activity' }[section as 'pages' | 'people' | 'activity']}
        </span>
      )}

      <span className="header-spacer" />

      {website && (
      <div className="header-views" role="group" aria-label="View">
        {(['chat', 'preview'] as const).map((view) => (
          <button
            key={view}
            aria-pressed={mobileView === view}
            onClick={() => onMobileView(view)}
          >
            {view === 'chat' ? 'Chat' : 'Preview'}
          </button>
        ))}
      </div>
      )}

      {website && (
      <div className="header-devices" role="group" aria-label="Preview width">
        {devices.map(({ id, label }) => (
          <button
            key={id}
            className="icon-button"
            aria-label={label}
            title={label}
            aria-pressed={device === id}
            onClick={() => onDevice(id)}
          >
            <Icon name={id satisfies IconName} size={17} />
          </button>
        ))}
      </div>
      )}
      {website && <span className="header-divider" aria-hidden="true" />}

      <button
        className="icon-button"
        aria-label={`Switch to the ${nextTheme} theme`}
        title={`Switch to the ${nextTheme} theme`}
        onClick={onToggleTheme}
      >
        <Icon name={theme === 'dark' ? 'sun' : 'moon'} size={17} />
      </button>

      <button
        className="publish"
        disabled={pending === 0}
        aria-label={pending === 0 ? 'Publish: no changes to publish' : `Publish ${pending} changes`}
        title={pending === 0 ? 'No changes to publish' : undefined}
      >
        Publish
        <span className="publish-count">{pending}</span>
      </button>
    </header>
  )
}
