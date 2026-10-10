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

/** The agent's account, as the CLI reports it. */
export interface AgentAccount {
  signed_in: boolean
  method: string | null
  expires_at: string | null
  signing_in: boolean
  problem: string | null
}

export const agent = {
  account: () => call<AgentAccount>('GET', '/api/agent/account'),
  /** Starts the sign-in and returns the address of the sign-in page. */
  login: () => call<{ url: string }>('POST', '/api/agent/login'),
  code: (code: string) => call<AgentAccount>('POST', '/api/agent/login/code', { code }),
  logout: () => call<void>('POST', '/api/agent/logout'),
}

/** One line of the chat. */
export interface ChatItem {
  id: number
  kind: 'person' | 'answer' | 'thinking' | 'activity' | 'notice' | 'error' | 'permission' | 'question'
  text: string
  detail?: string
  state?: 'running' | 'done' | 'failed' | 'stopped' | 'waiting' | 'allowed' | 'denied' | 'answered'
  author?: string
  at: string
  /** Pieces of text received while the item is being written; 0 when complete. */
  seq: number
}

export interface ChatConversation {
  id: number
  title: string
}

export interface ChatTurn {
  elapsed_ms: number
  stopping: boolean
  /** The assistant is done; the preview is being built. */
  updating: boolean
  /** The assistant waits for a person to answer a request. */
  waiting: boolean
}

export interface ChatSnapshot {
  conversation: ChatConversation | null
  items: ChatItem[]
  turn: ChatTurn | null
  limits: unknown
}

/** What the server sends on the chat's event stream. */
export type ChatEvent =
  | ({ type: 'snapshot' } & ChatSnapshot)
  | { type: 'item'; item: ChatItem }
  | { type: 'delta'; id: number; seq: number; text: string }
  | { type: 'turn'; turn: ChatTurn | null; conversation: ChatConversation | null }
  | { type: 'limits'; limits: unknown }
  | { type: 'reset' }

export const chat = {
  eventsUrl: '/api/chat/events',
  send: (text: string) => call<void>('POST', '/api/chat/messages', { text }),
  stop: () => call<void>('POST', '/api/chat/stop'),
  newConversation: () => call<void>('POST', '/api/chat/new'),
  /** Answers a permission request of the assistant. */
  answerPermission: (id: number, allow: boolean) =>
    call<void>('POST', `/api/chat/permissions/${id}`, { allow }),
}
