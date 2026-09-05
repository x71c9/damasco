import { useEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from 'react'
import { fetchInstallation, siblingUrl, type Installation, type Me } from './api'
import { Chat } from './components/Chat'
import { Header, type Device, type MobileView } from './components/Header'
import { NavRail, type Section } from './components/NavRail'
import { Preview } from './components/Preview'
import { PeoplePage } from './people/PeoplePage'
import { currentTheme, setTheme, type Theme } from './theme'
import './Workspace.css'

const CHAT_MIN = 360
const CHAT_MAX = 720
const clamp = (width: number) => Math.min(CHAT_MAX, Math.max(CHAT_MIN, width))

const paths: Record<Section, string> = { website: '/', people: '/people' }

function sectionOf(pathname: string): Section {
  return pathname === '/people' ? 'people' : 'website'
}

/** The signed-in panel: header, sections, preview and chat. */
export function Workspace({ me, onSignedOut }: { me: Me; onSignedOut: () => void }) {
  const [installation, setInstallation] = useState<Installation | null>(null)
  const [device, setDevice] = useState<Device>('computer')
  const [reloadKey, setReloadKey] = useState(0)
  const [mobileView, setMobileView] = useState<MobileView>('chat')
  const [chatWidth, setChatWidth] = useState(420)
  const [theme, setThemeState] = useState<Theme>(currentTheme)
  const toggleTheme = () => {
    const next = theme === 'dark' ? 'light' : 'dark'
    setTheme(next)
    setThemeState(next)
  }
  const drag = useRef<{ x: number; width: number } | null>(null)
  const [section, setSection] = useState<Section>(() => sectionOf(window.location.pathname))

  // Each section has its own address, so reload and the back button work.
  useEffect(() => {
    const onPop = () => setSection(sectionOf(window.location.pathname))
    window.addEventListener('popstate', onPop)
    return () => window.removeEventListener('popstate', onPop)
  }, [])
  const navigate = (next: Section) => {
    if (next !== section) window.history.pushState(null, '', paths[next])
    setSection(next)
  }
  const shown: Section = section === 'people' && !me.capabilities.some((c) => c.resource === 'people') ? 'website' : section

  useEffect(() => {
    fetchInstallation().then(setInstallation, (error: unknown) => console.error(error))
  }, [])

  // The separator sits on the left of the chat: dragging left widens it.
  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    event.currentTarget.setPointerCapture(event.pointerId)
    drag.current = { x: event.clientX, width: chatWidth }
  }
  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    if (drag.current) setChatWidth(clamp(drag.current.width + drag.current.x - event.clientX))
  }
  const onPointerUp = () => {
    drag.current = null
  }
  const onSeparatorKey = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === 'ArrowLeft') setChatWidth((width) => clamp(width + 20))
    if (event.key === 'ArrowRight') setChatWidth((width) => clamp(width - 20))
  }

  const siteName = installation?.site_hosts[0] ?? 'Site'
  const previewUrl = installation ? siblingUrl(installation.preview_host) : null

  return (
    <div className="app" data-mobile-view={mobileView}>
      <Header
        section={shown}
        siteName={siteName}
        device={device}
        onDevice={setDevice}
        onReload={() => setReloadKey((key) => key + 1)}
        pending={0}
        mobileView={mobileView}
        onMobileView={setMobileView}
        theme={theme}
        onToggleTheme={toggleTheme}
      />
      <div className="app-body">
        <NavRail current={shown} onNavigate={navigate} me={me} onSignedOut={onSignedOut} />
        {shown === 'people' ? (
          <main className="app-main">
            <PeoplePage me={me} />
          </main>
        ) : (
        <main className="app-main">
          <Preview url={previewUrl} device={device} reloadKey={reloadKey} />
          <div
            className="app-separator"
            role="separator"
            aria-orientation="vertical"
            aria-label="Drag to resize the chat"
            aria-valuemin={CHAT_MIN}
            aria-valuemax={CHAT_MAX}
            aria-valuenow={chatWidth}
            tabIndex={0}
            onPointerDown={onPointerDown}
            onPointerMove={onPointerMove}
            onPointerUp={onPointerUp}
            onKeyDown={onSeparatorKey}
          >
            <span />
          </div>
          <div className="app-chat" style={{ width: chatWidth }}>
            <Chat />
          </div>
        </main>
        )}
      </div>
    </div>
  )
}
