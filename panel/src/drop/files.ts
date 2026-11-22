/** Files dragged onto the panel.
 *
 * One set of listeners for the whole window: a file dropped outside the
 * places that take files does nothing, where the browser would open it in
 * place of the panel, losing what was being written. */

/** A place that takes files. */
export interface Zone {
  /** Whether it takes them now. */
  accepts: () => boolean
  /** The files dropped, and whether folders were among them. */
  take: (files: File[], folders: boolean) => void
}

/** On <html> while files are over the panel: the frame of the preview, a
 * document of its own, then lets them through to it. */
const DRAGGED = 'data-files-dragged'
/** `dragover` repeats at least every 550 ms while files are over the
 * window: longer without one, and they are gone. */
const GONE_MS = 1000

const zones = new Map<Element, Zone>()
const listeners = new Set<() => void>()
let over: Element | null = null
/** Something of the panel is dragged, such as one of its pictures, which
 * some browsers offer as a file too. */
let ownDrag = false
let gone: ReturnType<typeof setTimeout> | undefined

export function addZone(element: Element, zone: Zone): () => void {
  zones.set(element, zone)
  return () => {
    zones.delete(element)
    if (over === element) show(null)
  }
}

/** The zone the files are over, for `useSyncExternalStore`. */
export function zoneOver(): Element | null {
  return over
}

export function onZoneChange(listener: () => void): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

function show(element: Element | null) {
  if (over === element) return
  over = element
  for (const listener of listeners) listener()
}

/** The zone under `target`. A dialog hides those of the page behind it. */
function zoneAt(target: EventTarget | null): Element | null {
  for (let at = target instanceof Element ? target : null; at; at = at.parentElement) {
    if (zones.has(at)) return at
    if (at.tagName === 'DIALOG') return null
  }
  return null
}

function end() {
  clearTimeout(gone)
  document.documentElement.removeAttribute(DRAGGED)
  show(null)
}

function hasFiles(event: DragEvent): boolean {
  return event.dataTransfer?.types.includes('Files') ?? false
}

/** The files of a drop. A folder arrives as a file that cannot be read. */
function dropped(data: DataTransfer): { files: File[]; folders: boolean } {
  const files: File[] = []
  let folders = false
  for (const item of Array.from(data.items)) {
    if (item.kind !== 'file') continue
    if (item.webkitGetAsEntry()?.isDirectory) {
      folders = true
      continue
    }
    const file = item.getAsFile()
    if (file) files.push(file)
  }
  return { files, folders }
}

/** Starts following the files dragged onto the window. */
export function watchFileDrags() {
  const onOver = (event: DragEvent) => {
    if (!hasFiles(event) || !event.dataTransfer) return
    event.preventDefault()
    // The editors inside would show where text goes.
    event.stopPropagation()
    const zone = ownDrag ? null : zoneAt(event.target)
    event.dataTransfer.dropEffect = zone && zones.get(zone)?.accepts() ? 'copy' : 'none'
    if (ownDrag) return
    document.documentElement.setAttribute(DRAGGED, '')
    show(zone)
    clearTimeout(gone)
    gone = setTimeout(end, GONE_MS)
  }
  const onDrop = (event: DragEvent) => {
    const own = ownDrag
    ownDrag = false
    end()
    if (!hasFiles(event) || !event.dataTransfer) return
    event.preventDefault()
    event.stopPropagation()
    const element = own ? null : zoneAt(event.target)
    const zone = element && zones.get(element)
    if (!zone || !zone.accepts()) return
    const { files, folders } = dropped(event.dataTransfer)
    if (files.length > 0 || folders) zone.take(files, folders)
  }
  const onLeave = (event: DragEvent) => {
    // Out of the window. Between two elements, Safari has no relatedTarget
    // either: the pointer tells them apart.
    if (event.relatedTarget) return
    const { clientX: x, clientY: y } = event
    if (x <= 0 || y <= 0 || x >= window.innerWidth || y >= window.innerHeight) end()
  }

  window.addEventListener('dragenter', onOver, true)
  window.addEventListener('dragover', onOver, true)
  window.addEventListener('drop', onDrop, true)
  window.addEventListener('dragleave', onLeave, true)
  window.addEventListener('dragstart', () => (ownDrag = true), true)
  window.addEventListener('dragend', () => {
    ownDrag = false
    end()
  }, true)
}
