import type { ReactNode } from 'react'
import type { SettingsSection } from './sections'
import './Settings.css'

const labels: Record<SettingsSection, string> = { people: 'People', activity: 'Activity' }

/** Settings: its sections in a menu on the left, the open one beside it. */
export function SettingsPage({
  current,
  sections,
  onOpen,
  children,
}: {
  current: SettingsSection
  sections: SettingsSection[]
  onOpen: (section: SettingsSection) => void
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
      </nav>
      {children}
    </div>
  )
}
