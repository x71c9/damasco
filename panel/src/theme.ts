/** Light or dark theme of the panel, remembered per browser. */

export type Theme = 'light' | 'dark'

const KEY = 'axioma.theme'

function stored(): Theme | null {
  try {
    const value = localStorage.getItem(KEY)
    return value === 'light' || value === 'dark' ? value : null
  } catch {
    return null
  }
}

/** The theme in use: the stored choice, otherwise the system's. */
export function currentTheme(): Theme {
  return stored() ?? (window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark')
}

/** Applies the stored choice before the first render, to avoid a flash. */
export function applyStoredTheme() {
  const theme = stored()
  if (theme) document.documentElement.dataset.theme = theme
}

export function setTheme(theme: Theme) {
  document.documentElement.dataset.theme = theme
  try {
    localStorage.setItem(KEY, theme)
  } catch {
    // Private windows may refuse storage: the choice lasts for this page only.
  }
}
