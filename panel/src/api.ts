/** Calls to the axioma API on the panel hostname. */

export class ApiError extends Error {
  status: number
  /** Form field the error is about, when the server names one. */
  field: string | null

  constructor(status: number, message: string, field: string | null = null) {
    super(message)
    this.status = status
    this.field = field
  }
}

async function call<T>(method: 'GET' | 'POST', path: string, body?: unknown): Promise<T> {
  const response = await fetch(path, {
    method,
    headers: body === undefined ? undefined : { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
    credentials: 'same-origin',
  })
  if (response.status === 204) return undefined as T
  const data: unknown = await response.json().catch(() => null)
  if (!response.ok) {
    const message =
      data && typeof data === 'object' && 'error' in data && typeof data.error === 'string'
        ? data.error
        : `${method} ${path}: ${response.status}`
    const field =
      data && typeof data === 'object' && 'field' in data && typeof data.field === 'string'
        ? data.field
        : null
    throw new ApiError(response.status, message, field)
  }
  return data as T
}

export type Role = 'owner' | 'admin' | 'editor'

/** The signed-in person. */
export type Resource = 'people' | 'activity' | 'chat' | 'content' | 'publish' | 'settings'
export type Action = 'read' | 'write'

export interface Me {
  id: number
  email: string
  name: string
  role: Role
  capabilities: { resource: Resource; action: Action }[]
}

/** Whether the signed-in person may do `action` on `resource`. */
export function can(me: Me, resource: Resource, action: Action = 'read'): boolean {
  return me.capabilities.some((c) => c.resource === resource && c.action === action)
}

export type LinkPurpose = 'setup' | 'reset'

export const auth = {
  me: () => call<Me>('GET', '/api/auth/me'),
  login: (email: string, password: string) =>
    call<Me>('POST', '/api/auth/login', { email, password }),
  logout: () => call<void>('POST', '/api/auth/logout'),
  link: (token: string) =>
    call<{ purpose: LinkPurpose; name: string | null }>('POST', '/api/auth/link', { token }),
  setup: (token: string, password: string, email?: string, name?: string) =>
    call<Me>('POST', '/api/auth/setup', { token, password, email, name }),
}

/** What the sign-in page can show before anyone signs in. */
export interface PublicInfo {
  site: string | null
  /** False on a new installation, before the owner's account exists. */
  has_owner: boolean
}

export const fetchPublic = () => call<PublicInfo>('GET', '/api/public')

/** Public facts about the installation, needed to address the preview. */
export interface Installation {
  preview_host: string
  site_hosts: string[]
}

export const fetchInstallation = () => call<Installation>('GET', '/api/installation')

/** Address of a sibling hostname, with the scheme and port of the panel. */
export function siblingUrl(host: string): string {
  const port = window.location.port ? `:${window.location.port}` : ''
  return `${window.location.protocol}//${host}${port}/`
}

/** One person with access, or with a pending invitation. */
export interface Person {
  id: number
  email: string | null
  name: string | null
  role: Role
  status: 'active' | 'invited' | 'revoked'
  created_at: string
}

export const people = {
  list: () => call<Person[]>('GET', '/api/people'),
  invite: (name: string, role: Role) =>
    call<{ id: number; link: string }>('POST', '/api/people', { name, role }),
  setRole: (id: number, role: Role) => call<void>('POST', `/api/people/${id}/role`, { role }),
  link: (id: number) =>
    call<{ purpose: LinkPurpose; link: string }>('POST', `/api/people/${id}/link`),
  revoke: (id: number) => call<void>('POST', `/api/people/${id}/revoke`),
}

/** One entry of the activity log. */
export interface ActivityEntry {
  id: number
  at: string
  environment: 'installation' | 'preview' | 'production'
  action: string
  actor_id: number | null
  actor_name: string | null
  subject: string | null
  subject_name: string | null
}

export const activity = {
  /** Newest first; `before` continues after the oldest entry shown. */
  list: (before?: number) =>
    call<{ entries: ActivityEntry[]; next: number | null }>(
      'GET',
      before === undefined ? '/api/activity' : `/api/activity?before=${before}`,
    ),
}

/** State of the builds of the site and the release shown by the preview. */
export interface BuildStatus {
  state: 'idle' | 'running' | 'ready' | 'failed'
  release: string | null
  error: string | null
  log: string | null
}

export const fetchBuild = () => call<BuildStatus>('GET', '/api/build')
