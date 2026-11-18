import type { ReactNode } from 'react'
import type { SettingsSection } from './sections'
import './Settings.css'
import { Version } from './Version'

const labels: Record<SettingsSection, string> = {
  assistant: 'Assistant',
  people: 'People',
  activity: 'Activity',
}

/** Settings: its sections in a menu on the left, the open one beside it. */
export function SettingsPage({
  current,
  sections,
  onOpen,
  version,
  manage = false,
  children,
}: {
  current: SettingsSection
  sections: SettingsSection[]
  onOpen: (section: SettingsSection) => void
  /** Version of alogeno, at the foot of the menu. */
  version?: string
  /** Whether the person manages the installation, and may update it. */
  manage?: boolean
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
            {labels[section]}
          </button>
        ))}
        {version && <Version version={version} manage={manage} />}
      </nav>
      {children}
    </div>
  )
}
