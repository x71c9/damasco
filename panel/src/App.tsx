import { useEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from 'react'
import { fetchInstallation, siblingUrl, type Installation } from './api'
import { Chat } from './components/Chat'
import { Header, type Device, type MobileView } from './components/Header'
import { NavRail } from './components/NavRail'
import { Preview } from './components/Preview'
import './App.css'

const CHAT_MIN = 360
const CHAT_MAX = 720
const clamp = (width: number) => Math.min(CHAT_MAX, Math.max(CHAT_MIN, width))

export function App() {
  const [installation, setInstallation] = useState<Installation | null>(null)
  const [device, setDevice] = useState<Device>('computer')
  const [reloadKey, setReloadKey] = useState(0)
  const [mobileView, setMobileView] = useState<MobileView>('chat')
  const [chatWidth, setChatWidth] = useState(420)
  const drag = useRef<{ x: number; width: number } | null>(null)

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
        siteName={siteName}
        device={device}
        onDevice={setDevice}
        onReload={() => setReloadKey((key) => key + 1)}
        pending={0}
        mobileView={mobileView}
        onMobileView={setMobileView}
      />
      <div className="app-body">
        <NavRail current="website" />
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
      </div>
    </div>
  )
}
