import type { ReactNode } from 'react'
import { auth } from '../api'
import type { Theme } from '../theme'
import type { SettingsSection } from './sections'
import './Settings.css'
import { Version } from './Version'
import { Icon, type IconName } from '../components/icons'

const labels: Record<SettingsSection, string> = {
  assistant: 'Assistant',
  instructions: 'Instructions',
  sites: 'Sites',
  builds: 'Builds',
}

const icons: Record<SettingsSection, IconName> = {
  assistant: 'assistant',
  instructions: 'plan',
  sites: 'website',
  builds: 'history',
}

interface SettingsPageProps {
  current: SettingsSection
  sections: SettingsSection[]
  onOpen: (section: SettingsSection) => void
  /** Version of damasco, at the foot of the menu. */
  version?: string
  /** Laid out for a phone: the list of settings, or one section with the
   * way back to the list. */
  phone: boolean
  /** On a phone: show the list instead of the section. */
  list: boolean
  onList: () => void
  /** The theme and the account have no place of their own on a phone: they
   * are in the list. */
  theme: Theme
  onToggleTheme: () => void
  onSignedOut: () => void
  children: ReactNode
}

/** Settings: its sections in a menu on the left, the open one beside it. On
 * a phone, a list of the sections, the theme, the version and the account;
 * each section opens on its own. */
export function SettingsPage({
  current,
  sections,
  onOpen,
  version,
  phone,
  list,
  onList,
  theme,
  onToggleTheme,
  onSignedOut,
  children,
}: SettingsPageProps) {
  if (phone && list) {
    return (
      <SettingsList
        sections={sections}
        onOpen={onOpen}
        version={version}
        theme={theme}
        onToggleTheme={onToggleTheme}
        onSignedOut={onSignedOut}
      />
    )
  }
  return (
    <div className="settings">
      {phone ? (
        <button className="settings-back" onClick={onList}>
          <Icon name="chevronLeft" size={18} />
          Settings
        </button>
      ) : (
        <nav className="settings-menu" aria-label="Settings">
          <h2 className="settings-title">Settings</h2>
          {sections.map((section) => (
            <button
              key={section}
              className="settings-item"
              aria-current={section === current ? 'page' : undefined}
              onClick={() => onOpen(section)}
            >
              <Icon name={icons[section]} size={17} />
              <span className="settings-item-label">{labels[section]}</span>
            </button>
          ))}
          {version && <Version version={version} />}
        </nav>
      )}
      {children}
    </div>
  )
}

function SettingsList({
  sections,
  onOpen,
  version,
  theme,
  onToggleTheme,
  onSignedOut,
}: Omit<SettingsPageProps, 'current' | 'phone' | 'list' | 'onList' | 'children'>) {
  const signOut = async () => {
    try {
      await auth.logout()
    } finally {
      onSignedOut()
    }
  }
  return (
    <div className="page-frame">
      <div className="page-card settings-list">
        <h1 className="settings-list-title">Settings</h1>
        <nav className="settings-group" aria-label="Settings">
          {sections.map((section) => (
            <button key={section} className="settings-row" onClick={() => onOpen(section)}>
              <Icon name={icons[section]} size={19} />
              <span className="settings-row-label">{labels[section]}</span>
              <Icon name="chevronRight" size={17} />
            </button>
          ))}
        </nav>
        <div className="settings-group">
          <button className="settings-row" onClick={onToggleTheme} aria-label={`Theme: ${theme}. Switch to the ${theme === 'dark' ? 'light' : 'dark'} theme`}>
            <Icon name={theme === 'dark' ? 'moon' : 'sun'} size={19} />
            <span className="settings-row-label">Theme</span>
            <span className="settings-row-value">{theme === 'dark' ? 'Dark' : 'Light'}</span>
          </button>
        </div>
        {version && (
          <div className="settings-group settings-group-version">
            <Version version={version} />
          </div>
        )}
        <div className="settings-group" role="group" aria-label="Account">
          <div className="settings-row settings-row-static">
            <Icon name="user" size={19} />
            <span className="settings-row-label">Owner</span>
          </div>
          <button className="settings-row settings-row-out" onClick={signOut}>
            <span className="settings-row-label">Sign out</span>
          </button>
        </div>
      </div>
    </div>
  )
}
