import type { ReactNode } from 'react'
import type { SettingsSection } from './sections'
import './Settings.css'
import { Version } from './Version'
import { Icon, type IconName } from '../components/icons'

const labels: Record<SettingsSection, string> = {
  assistant: 'Assistant',
  sites: 'Sites',
  builds: 'Builds',
}

const icons: Record<SettingsSection, IconName> = {
  assistant: 'assistant',
  sites: 'website',
  builds: 'history',
}

/** Settings: its sections in a menu on the left, the open one beside it. */
export function SettingsPage({
  current,
  sections,
  onOpen,
  version,
  children,
}: {
  current: SettingsSection
  sections: SettingsSection[]
  onOpen: (section: SettingsSection) => void
  /** Version of damasco, at the foot of the menu. */
  version?: string
  children: ReactNode
}) {
  return (
    <div className="settings">
      <nav className="settings-menu" aria-label="Settings">
        <h2 className="settings-title">Settings</h2>
        {sections.map((section) => (
          <button
            key={section}
            className="settings-item"
            aria-current={section === current ? 'page' : undefined}
            onClick={() => onOpen(section)}
          >
            <span className="settings-item-label">{labels[section]}</span>
            <Icon name={icons[section]} size={17} />
          </button>
        ))}
        {version && <Version version={version} />}
      </nav>
      {children}
    </div>
  )
}
