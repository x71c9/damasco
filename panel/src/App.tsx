import { useEffect, useState } from 'react'
import {
  ApiError,
  auth,
  fetchPublic,
  openSite,
  sites,
  type Me,
  type PublicInfo,
  type SiteInfo,
} from './api'
import { AuthLayout } from './auth/AuthLayout'
import { LoginScreen, NoOwnerScreen } from './auth/LoginScreen'
import { SetupScreen } from './auth/SetupScreen'
import { Workspace } from './Workspace'

/** Token of a one-time link: /setup#<token>. The fragment never reaches the
 * server or the proxy logs. */
function linkToken(): string | null {
  if (window.location.pathname !== '/setup') return null
  const token = window.location.hash.slice(1)
  return token || null
}

type Session = { state: 'loading' } | { state: 'signed-out' } | { state: 'signed-in'; me: Me }

/** Chooses between the one-time link page, sign-in and the panel. */
export function App() {
  const [token, setToken] = useState(linkToken)
  const [info, setInfo] = useState<PublicInfo | null>(null)
  const [session, setSession] = useState<Session>({ state: 'loading' })

  useEffect(() => {
    fetchPublic().then(setInfo, () => setInfo({ site: null, has_owner: true }))
    if (token) return
    auth.me().then(
      (me) => setSession({ state: 'signed-in', me }),
      (err: unknown) => {
        if (!(err instanceof ApiError && err.status === 401)) console.error(err)
        setSession({ state: 'signed-out' })
      },
    )
  }, [token])

  const signedIn = (me: Me) => {
    window.history.replaceState(null, '', '/')
    setToken(null)
    setSession({ state: 'signed-in', me })
  }
  const signedOut = () => setSession({ state: 'signed-out' })
  const site = info?.site ?? null

  if (token) return <SetupScreen token={token} site={site} onSignedIn={signedIn} />
  switch (session.state) {
    case 'loading':
      return <AuthLayout title="One moment" site={site}>{null}</AuthLayout>
    case 'signed-out':
      if (!info) return <AuthLayout title="One moment" site={site}>{null}</AuthLayout>
      if (!info.has_owner) return <NoOwnerScreen site={site} />
      return <LoginScreen site={site} onSignedIn={signedIn} />
    case 'signed-in':
      return <SiteWorkspace me={session.me} onSignedOut={signedOut} />
  }
}

/** The panel on one site: the first, until the panel lets the owner choose.
 * Every request about a site goes to the one opened here. */
function SiteWorkspace({ me, onSignedOut }: { me: Me; onSignedOut: () => void }) {
  const [site, setSite] = useState<SiteInfo | null>(null)
  const [problem, setProblem] = useState<string | null>(null)
  useEffect(() => {
    sites.list().then(
      (list) => {
        const first = list[0]
        if (!first) {
          setProblem('This installation has no site yet.')
          return
        }
        openSite(first.id)
        setSite(first)
      },
      (err: unknown) => setProblem(err instanceof Error ? err.message : String(err)),
    )
  }, [])
  if (problem) return <AuthLayout title="No site" site={null}><p className="auth-text">{problem}</p></AuthLayout>
  if (!site) return <AuthLayout title="One moment" site={null}>{null}</AuthLayout>
  return <Workspace key={site.id} me={me} site={site} onSignedOut={onSignedOut} />
}
