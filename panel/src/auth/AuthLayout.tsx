import { useState, type ReactNode } from 'react'
import { Mark } from '../components/icons'
import { ThemeButton } from '../components/ThemeButton'
import { currentTheme, setTheme, type Theme } from '../theme'
import './Auth.css'

const REPOSITORY = 'https://github.com/x71c9/damasco'

interface AuthLayoutProps {
  title: string
  site: string | null
  children: ReactNode
}

/** Centered petal card used by sign-in and account setup, with the link to
 * the project underneath and the theme switch in the corner, the same
 * choice the panel remembers. */
export function AuthLayout({ title, site, children }: AuthLayoutProps) {
  const [theme, setThemeState] = useState<Theme>(currentTheme)
  const toggleTheme = () => {
    const next = theme === 'dark' ? 'light' : 'dark'
    setTheme(next)
    setThemeState(next)
  }
  return (
    <main className="auth">
      <ThemeButton theme={theme} onToggle={toggleTheme} className="icon-button auth-theme" />
      <div className="auth-card">
        <div className="auth-head">
          <Mark size={44} label="damasco" />
          <div className="auth-title">
            <h1>{title}</h1>
            {site && <span>{site}</span>}
          </div>
        </div>
        {children}
      </div>
      <a className="auth-project" href={REPOSITORY} target="_blank" rel="noreferrer">
        damasco on GitHub
      </a>
    </main>
  )
}
