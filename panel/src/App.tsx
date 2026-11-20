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
import { setBase } from './navigation'

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

/** The slug in the address, `/s/<slug>/…`, if any. */
function slugInAddress(): string | null {
  const found = /^\/s\/([^/]+)/.exec(window.location.pathname)
  return found ? decodeURIComponent(found[1]) : null
}

/** The panel on one site: the one the address names (`/s/<slug>/…`), or
 * the first. Every request about a site goes to the one opened here. */
function SiteWorkspace({ me, onSignedOut }: { me: Me; onSignedOut: () => void }) {
  const [list, setList] = useState<SiteInfo[] | null>(null)
  const [problem, setProblem] = useState<string | null>(null)
  const readList = () => {
    sites.list().then(setList, (err: unknown) =>
      setProblem(err instanceof Error ? err.message : String(err)),
    )
  }
  useEffect(readList, [])
  if (problem) {
    return (
      <AuthLayout title="No site" site={null}>
        <p className="auth-text">{problem}</p>
      </AuthLayout>
    )
  }
  if (!list) return <AuthLayout title="One moment" site={null}>{null}</AuthLayout>
  const slug = slugInAddress()
  const site = list.find((candidate) => candidate.slug === slug) ?? list[0]
  if (!site) {
    return (
      <AuthLayout title="No site" site={null}>
        <p className="auth-text">This installation has no site yet.</p>
      </AuthLayout>
    )
  }
  const prefix = `/s/${encodeURIComponent(site.slug)}`
  // An address without the site, or with an unknown one, moves under the
  // site shown, keeping the section.
  if (slug !== site.slug) {
    const rest = slug === null ? window.location.pathname : '/'
    window.history.replaceState(null, '', `${prefix}${rest === '/' ? '' : rest}${window.location.search}`)
  }
  setBase(prefix)
  openSite(site.id)
  return <Workspace
      key={site.id}
      me={me}
      site={site}
      sites={list}
      onSitesChanged={readList}
      onSignedOut={onSignedOut}
    />
}
