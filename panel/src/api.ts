/** Calls to the axioma API on the panel hostname. */

/** A problem with one field of a saved item, as the server names it. */
export interface FieldProblem {
  field: string
  message: string
}

export class ApiError extends Error {
  status: number
  /** Form field the error is about, when the server names one. */
  field: string | null
  /** Every field that is wrong, for forms with many. */
  problems: FieldProblem[]

  constructor(status: number, message: string, field: string | null = null, problems: FieldProblem[] = []) {
    super(message)
    this.status = status
    this.field = field
    this.problems = problems
  }
}

async function call<T>(
  method: 'GET' | 'POST' | 'PATCH' | 'DELETE',
  path: string,
  body?: unknown,
): Promise<T> {
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
    const problems =
      data && typeof data === 'object' && 'problems' in data && Array.isArray(data.problems)
        ? (data.problems as FieldProblem[])
        : []
    throw new ApiError(response.status, message, field, problems)
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
  /** A message the site can go back to before; a going back that can be undone. */
  back?: boolean
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

/** The person using the chat: during a turn, and for a while after it. */
export interface ChatHolder {
  id: number
  name: string
  /** How long the person keeps the chat after the turn; null during it. */
  remaining_ms: number | null
}

export interface ChatSnapshot {
  conversation: ChatConversation | null
  items: ChatItem[]
  turn: ChatTurn | null
  limits: unknown
  holder: ChatHolder | null
  /** Who has the terminal open: the chat waits meanwhile. */
  terminal: { id: number; name: string } | null
  problem: ChatProblem | null
}

/** Why the last turn did not finish. */
export interface ChatProblem {
  /** error: it can be tried again; limit: the plan is used up until
   * `until`; login: the account must sign in again. */
  kind: 'error' | 'limit' | 'login'
  /** Unix seconds. */
  until?: number
}

/** Questions of the assistant, kept in the detail of a question item. */
export interface ChatQuestions {
  questions: { question: string; options: { label: string; description?: string }[]; multiple: boolean }[]
  /** One text per question, once answered. */
  answers?: string[]
}

export interface QuestionAnswer {
  choices: string[]
  other?: string
}

/** What the server sends on the chat's event stream. */
export type ChatEvent =
  | ({ type: 'snapshot' } & ChatSnapshot)
  | { type: 'item'; item: ChatItem }
  | { type: 'delta'; id: number; seq: number; text: string }
  | {
      type: 'turn'
      turn: ChatTurn | null
      conversation: ChatConversation | null
      holder: ChatHolder | null
      terminal: { id: number; name: string } | null
      problem: ChatProblem | null
    }
  | { type: 'limits'; limits: unknown }
  | { type: 'reset' }

export const chat = {
  eventsUrl: '/api/chat/events',
  send: (text: string) => call<void>('POST', '/api/chat/messages', { text }),
  stop: () => call<void>('POST', '/api/chat/stop'),
  newConversation: () => call<void>('POST', '/api/chat/new'),
  /** Takes the site back to before a message; returns the message. */
  goBack: (id: number) => call<{ text: string }>('POST', `/api/chat/back/${id}`),
  /** Undoes a going back. */
  undoBack: (id: number) => call<void>('POST', `/api/chat/undo/${id}`),
  /** Tries the last request again after something went wrong. */
  retry: () => call<void>('POST', '/api/chat/retry'),
  /** Takes the chat from the person holding it, once the turn is over. */
  takeOver: () => call<void>('POST', '/api/chat/take-over'),
  /** Answers the questions of the assistant, one answer per question. */
  answerQuestions: (id: number, answers: QuestionAnswer[]) =>
    call<void>('POST', `/api/chat/questions/${id}`, { answers }),
  /** Answers a permission request of the assistant. */
  answerPermission: (id: number, allow: boolean) =>
    call<void>('POST', `/api/chat/permissions/${id}`, { allow }),
}

export const terminal = {
  /** Ends the terminal: its changes are saved and the chat goes on. */
  close: () => call<void>('POST', '/api/terminal/close'),
}

/** A page as listed: enough to find it and place it in the tree. */
export interface PageSummary {
  id: string
  type: string
  title: string
  slug: string
  inside: string | null
  order: number
  menu: boolean
  /** The public address, from the slugs of the page and its parents. */
  address: string
}

/** A whole page, as stored. */
export type Page = PageSummary & {
  lang: string
  fields: Record<string, unknown>
  blocks: unknown[]
  seo: { title: string | null; description: string | null; image: unknown; hidden: boolean }
}

export const pages = {
  /** The pages in tree order; `locked` says why they cannot change now. */
  list: () => call<{ pages: PageSummary[]; locked: string | null }>('GET', '/api/pages'),
  read: (id: string) => call<Page>('GET', `/api/pages/${id}`),
  create: (page: Partial<Page>) => call<Page>('POST', '/api/pages', { page }),
  update: (id: string, changes: Partial<Page>) =>
    call<Page>('PATCH', `/api/pages/${id}`, { changes }),
  remove: (id: string) => call<void>('DELETE', `/api/pages/${id}`),
}

/** A field of a page type, as the schema defines it. */
export interface TypeField {
  name: string
  label: string
  type: 'text' | 'richtext' | 'number' | 'boolean' | 'date' | 'media' | 'page' | 'list' | 'group'
  required?: boolean
  help?: string | null
  /** For text: the most characters. */
  max?: number | null
  /** For a list: what each item is. */
  of?: Omit<TypeField, 'name' | 'label' | 'required' | 'help'>
  /** For a group: its fields. */
  fields?: TypeField[]
}

export interface PageType {
  name: string
  label: string
  fields: TypeField[]
}

export const pageTypes = {
  list: () => call<PageType[]>('GET', '/api/page-types'),
}

/** A picture of the site. */
export interface MediaItem {
  id: string
  name: string
  type: string
  bytes: number
  width: number
  height: number
  /** What the picture shows, read aloud and used by search engines. */
  alt: string
  sha256: string
  /** Titles of the pages using it. */
  used_on?: string[]
}

export const media = {
  list: () => call<{ media: MediaItem[]; locked: string | null }>('GET', '/api/media'),
  /** The address of a picture's file, for the panel. */
  fileUrl: (id: string) => `/api/media/${encodeURIComponent(id)}/file`,
  /** Uploads a file; the server checks it is a picture. */
  upload: async (file: File): Promise<MediaItem> => {
    const response = await fetch(`/api/media?name=${encodeURIComponent(file.name)}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/octet-stream' },
      body: file,
      credentials: 'same-origin',
    })
    const data: unknown = await response.json().catch(() => null)
    if (!response.ok) {
      const message =
        data && typeof data === 'object' && 'error' in data && typeof data.error === 'string'
          ? data.error
          : `upload: ${response.status}`
      throw new ApiError(response.status, message)
    }
    return data as MediaItem
  },
  update: (id: string, changes: Partial<Pick<MediaItem, 'alt' | 'name'>>) =>
    call<MediaItem>('PATCH', `/api/media/${encodeURIComponent(id)}`, { changes }),
  remove: (id: string) => call<void>('DELETE', `/api/media/${encodeURIComponent(id)}`),
}
