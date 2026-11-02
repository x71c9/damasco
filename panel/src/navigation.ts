/** Opens an address of the panel without loading the page again: the
 * workspace and the sections follow the address on `popstate`. */
export function goTo(href: string) {
  if (window.location.pathname + window.location.search !== href) {
    window.history.pushState(null, '', href)
  }
  window.dispatchEvent(new PopStateEvent('popstate'))
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
