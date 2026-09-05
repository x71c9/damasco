import type { ReactNode } from 'react'
import { Mark } from '../components/icons'
import './Auth.css'

const REPOSITORY = 'https://github.com/x71c9/axioma'

interface AuthLayoutProps {
  title: string
  site: string | null
  children: ReactNode
}

/** Centered petal card used by sign-in and account setup, with the link to
 * the project underneath. */
export function AuthLayout({ title, site, children }: AuthLayoutProps) {
  return (
    <main className="auth">
      <div className="auth-card">
        <div className="auth-head">
          <Mark size={44} label="axioma" />
          <div className="auth-title">
            <h1>{title}</h1>
            {site && <span>{site}</span>}
          </div>
        </div>
        {children}
      </div>
      <a className="auth-project" href={REPOSITORY} target="_blank" rel="noreferrer">
        axioma on GitHub
      </a>
    </main>
  )
}
