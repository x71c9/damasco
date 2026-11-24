import { useEffect, useState } from 'react'

/** Phones: the width under which the panel shows one view at a time with
 * the tab bar. The same number is in the CSS media queries. */
export const PHONE_QUERY = '(max-width: 760px)'

/** Whether the panel is laid out for a phone, following the window. */
export function usePhone(): boolean {
  const [phone, setPhone] = useState(() => window.matchMedia(PHONE_QUERY).matches)
  useEffect(() => {
    const query = window.matchMedia(PHONE_QUERY)
    const change = () => setPhone(query.matches)
    change()
    query.addEventListener('change', change)
    return () => query.removeEventListener('change', change)
  }, [])
  return phone
}

/** Keeps the panel as tall as the part of the window that is visible.
 *
 * Safari on iOS does not shrink the layout when its keyboard opens: it
 * scrolls the page under its own bars instead, and the header leaves the
 * screen. `--app-height` follows the visual viewport, so the panel is
 * always the visible height and never scrolls as a whole; `data-keyboard`
 * on <html> tells the CSS that the keyboard takes part of the screen. */
export function watchViewport() {
  const viewport = window.visualViewport
  if (!viewport) return
  const root = document.documentElement
  // The tallest the visible area has been with this width: much shorter
  // than that, a keyboard is open.
  let tallest = 0
  let width = 0
  const update = () => {
    if (window.innerWidth !== width) {
      width = window.innerWidth
      tallest = 0
    }
    tallest = Math.max(tallest, viewport.height)
    root.style.setProperty('--app-height', `${Math.round(viewport.height)}px`)
    const keyboard = viewport.height < tallest - 120
    if (keyboard) root.dataset.keyboard = ''
    else delete root.dataset.keyboard
    // Safari may still scroll the page to show a field: put it back.
    if (window.scrollY !== 0 || window.scrollX !== 0) window.scrollTo(0, 0)
  }
  update()
  viewport.addEventListener('resize', update)
  viewport.addEventListener('scroll', update)
  window.addEventListener('orientationchange', update)
  // A field brought into view (focus, scrollIntoView) may scroll the page
  // itself, which never scrolls as a whole: back to its top.
  window.addEventListener('scroll', () => {
    if (window.scrollY !== 0 || window.scrollX !== 0) window.scrollTo(0, 0)
  })
  document.body.addEventListener('scroll', () => {
    if (document.body.scrollTop !== 0) document.body.scrollTop = 0
  })
}

/** Phones: menus open as sheets over a veil (the CSS of the chat). A tap on
 * the veil lands on the sheet itself, outside its box: it closes the sheet
 * as Escape does, which every menu of the panel listens to. */
export function watchSheets() {
  document.addEventListener(
    'click',
    (event) => {
      const target = event.target
      if (!(target instanceof HTMLElement) || !target.classList.contains('assistant-menu')) return
      const box = target.getBoundingClientRect()
      const inside =
        event.clientX >= box.left && event.clientX <= box.right && event.clientY >= box.top && event.clientY <= box.bottom
      if (inside) return
      event.preventDefault()
      event.stopPropagation()
      document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
    },
    true,
  )
}
