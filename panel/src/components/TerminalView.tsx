import { useEffect, useRef, useState } from 'react'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'
import { sitePath } from '../api'
import { currentTheme } from '../theme'
import './TerminalView.css'

type State = 'connecting' | 'open' | 'closed' | 'refused'

/** The colours of the terminal, from the panel's theme. */
function themeOf() {
  const style = getComputedStyle(document.documentElement)
  const token = (name: string) => style.getPropertyValue(name).trim()
  return {
    background: token('--bg'),
    foreground: token('--fg'),
    cursor: token('--fg'),
    cursorAccent: token('--bg'),
    // The highlighter of the panel. The terminal reads only plain colours,
    // not the wider display-p3 accent of some screens.
    selectionBackground: token('--accent').startsWith('#') ? token('--accent') : '#ff2e8c',
    selectionForeground: token('--on-accent'),
  }
}

interface Callbacks {
  setState: (state: State) => void
  /** The background the terminal opened with. */
  setBackground: (color: string) => void
  /** Output arrived from the CLI: it is working. */
  onOutput: () => void
}

interface Running {
  /** Changes the size of the text; the terminal fits again. */
  setFontSize: (size: number) => void
  stop: () => void
}

/** Opens the terminal in `element` and connects it. */
function startTerminal(
  element: HTMLDivElement,
  fontSize: number,
  { setState, setBackground, onOutput }: Callbacks,
): Running {
  // The CLI picks its text colours for the theme it opens with, and keeps
  // them: the terminal keeps that theme's colours too, so it stays
  // readable when the panel's theme changes.
  const theme = themeOf()
  setBackground(theme.background)
  const term = new Terminal({
    fontFamily: "'Geist Mono Variable', 'Geist Mono', ui-monospace, monospace",
    fontSize,
    cursorBlink: true,
    theme,
  })
  const fit = new FitAddon()
  term.loadAddon(fit)
  term.open(element)
  fit.fit()
  const scheme = window.location.protocol === 'https:' ? 'wss' : 'ws'
  const socket = new WebSocket(
    `${scheme}://${window.location.host}${sitePath(`/terminal?cols=${term.cols}&rows=${term.rows}&theme=${currentTheme()}`)}`,
  )
  socket.binaryType = 'arraybuffer'
  let opened = false
  socket.onopen = () => {
    opened = true
    setState('open')
    term.focus()
  }
  socket.onmessage = (event) => {
    term.write(new Uint8Array(event.data as ArrayBuffer))
    onOutput()
  }
  // Refused before opening: the assistant is busy or someone else has it.
  socket.onclose = () => setState(opened ? 'closed' : 'refused')
  const input = term.onData((data) => {
    if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ type: 'input', data }))
  })
  const refit = () => {
    fit.fit()
    if (socket.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify({ type: 'resize', cols: term.cols, rows: term.rows }))
    }
  }
  const observer = new ResizeObserver(refit)
  observer.observe(element)
  return {
    setFontSize: (size) => {
      if (term.options.fontSize === size) return
      term.options.fontSize = size
      refit()
    },
    stop: () => {
      observer.disconnect()
      input.dispose()
      socket.close()
      term.dispose()
    },
  }
}

interface TerminalViewProps {
  /** Size of the text, in pixels: it follows the chat's text size. */
  fontSize: number
  /** The CLI ended, or the terminal could not open. */
  onEnded: (refused: boolean) => void
  /** Called with the time of the CLI's latest output, as performance.now(). */
  onOutput?: (at: number) => void
}

/** The interactive CLI of the assistant, on the chat's conversation, in
 * place of the chat. */
export function TerminalView({ fontSize, onEnded, onOutput }: TerminalViewProps) {
  const container = useRef<HTMLDivElement>(null)
  const [state, setState] = useState<State>('connecting')
  const [background, setBackground] = useState<string | undefined>(undefined)
  const ended = useRef(onEnded)
  const output = useRef(onOutput)
  useEffect(() => {
    ended.current = onEnded
    output.current = onOutput
  })

  const running = useRef<Running | null>(null)
  const firstSize = useRef(fontSize)
  useEffect(() => {
    running.current?.setFontSize(fontSize)
  }, [fontSize])

  useEffect(() => {
    if (!container.current) return
    const element = container.current
    let gone = false
    // The cells are measured when the terminal opens: the font must be there.
    document.fonts
      .load(`${firstSize.current}px 'Geist Mono Variable'`)
      .catch(() => [])
      .then(() => {
        if (gone) return
        running.current = startTerminal(element, firstSize.current, {
          setState: (next) => {
            setState(next)
            if (next === 'closed' || next === 'refused') ended.current(next === 'refused')
          },
          setBackground,
          onOutput: () => output.current?.(performance.now()),
        })
      })
    return () => {
      gone = true
      running.current?.stop()
      running.current = null
    }
  }, [])

  return (
    <div className="terminal-view" data-state={state}>
      <div className="terminal-screen" ref={container} style={{ background }} />
      {state === 'connecting' && <p className="terminal-note">Opening the terminal…</p>}
    </div>
  )
}
