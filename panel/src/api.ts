/** Public facts about the installation, served by the panel hostname. */
export interface Installation {
  preview_host: string
  site_hosts: string[]
}

export async function fetchInstallation(): Promise<Installation> {
  const response = await fetch('/api/installation')
  if (!response.ok) throw new Error(`GET /api/installation: ${response.status}`)
  return (await response.json()) as Installation
}

/** Address of a sibling hostname, with the scheme and port of the panel. */
export function siblingUrl(host: string): string {
  const port = window.location.port ? `:${window.location.port}` : ''
  return `${window.location.protocol}//${host}${port}/`
}
