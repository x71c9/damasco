import type { Theme } from '../theme'
import { Icon } from './icons'

/** Switches between the light and the dark theme: the header of the panel
 * and the sign-in pages show the same button. */
export function ThemeButton({
  theme,
  onToggle,
  className = 'icon-button',
}: {
  theme: Theme
  onToggle: () => void
  className?: string
}) {
  const next = theme === 'dark' ? 'light' : 'dark'
  return (
    <button
      type="button"
      className={className}
      aria-label={`Switch to the ${next} theme`}
      title={`Switch to the ${next} theme`}
      onClick={onToggle}
    >
      <Icon name={theme === 'dark' ? 'sun' : 'moon'} size={17} />
    </button>
  )
}
