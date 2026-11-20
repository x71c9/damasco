/** The addresses of the panel. Each site's sections live under its own
 * base, `/s/<slug>`: the sections use their paths (`/pages`,
 * `/settings/assistant`) and the base is added and taken away here, so
 * that two tabs can stay on two sites. */
let base = ''

/** Sets the base of the site the panel works on, such as `/s/blog`. */
export function setBase(prefix: string) {
  base = prefix
}

/** The full address of a path of the site the panel works on. */
export function hrefOf(path: string): string {
  return `${base}${path}`
}

/** The path of the current address within the site, without its base. */
export function here(): string {
  const path = window.location.pathname
  if (base && (path === base || path.startsWith(`${base}/`))) {
    return path.slice(base.length) || '/'
  }
  return path
}

/** Opens a path of the site without loading the page again: the workspace
 * and the sections follow the address on `popstate`. */
export function goTo(path: string) {
  const href = hrefOf(path)
  if (window.location.pathname + window.location.search !== href) {
    window.history.pushState(null, '', href)
  }
  window.dispatchEvent(new PopStateEvent('popstate'))
}

/** Records a path of the site in the browser's history, as a section does
 * when it opens. */
export function pushPath(path: string) {
  const href = hrefOf(path)
  if (window.location.pathname !== href) window.history.pushState(null, '', href)
}

/** A parameter of the current address, such as `field` in
 * `/pages/<id>?field=title`. */
export function param(name: string): string | null {
  return new URLSearchParams(window.location.search).get(name)
}

/** Takes the parameters away from the address, once they were used: a
 * reload does not repeat them. */
export function dropParams() {
  if (window.location.search) {
    window.history.replaceState(null, '', window.location.pathname)
  }
}
