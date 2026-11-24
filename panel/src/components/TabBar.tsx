import { Icon, type IconName } from './icons'
import './TabBar.css'

/** The four places of the panel on a phone. */
export type Tab = 'chat' | 'preview' | 'site' | 'settings'

const tabs: { id: Tab; label: string; icon: IconName }[] = [
  { id: 'chat', label: 'Chat', icon: 'chat' },
  { id: 'preview', label: 'Preview', icon: 'eye' },
  { id: 'site', label: 'Site', icon: 'website' },
  { id: 'settings', label: 'Settings', icon: 'settings' },
]

/** Section navigation on phones, at the foot of the screen: the chat, the
 * preview, the site's content (pages, components, media) and settings
 * (with the account). Each tab comes back where it was left. */
export function TabBar({ current, onTab }: { current: Tab; onTab: (tab: Tab) => void }) {
  return (
    <nav className="tabbar" aria-label="Sections">
      {tabs.map(({ id, label, icon }) => (
        <button
          key={id}
          className="tabbar-item"
          aria-current={id === current ? 'page' : undefined}
          onClick={() => onTab(id)}
        >
          <span className="tabbar-icon">
            <Icon name={icon} size={20} />
          </span>
          <span className="tabbar-label">{label}</span>
        </button>
      ))}
    </nav>
  )
}
