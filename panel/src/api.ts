import type { ProviderId } from './assistants'
/** Calls to the alogeno API on the panel hostname. */

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

/** The site the panel works on: its requests go under /api/sites/<id>. */
let currentSite = ''

/** Points the requests about a site at the site `id`. */
export function openSite(id: string) {
  currentSite = id
}

/** The address of `path` for the site the panel works on. */
export function sitePath(path: string): string {
  return `/api/sites/${encodeURIComponent(currentSite)}${path}`
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

/** The signed-in owner, the only account of the panel. */
export interface Me {
  id: number
}

export type LinkPurpose = 'setup' | 'reset'

export const auth = {
  me: () => call<Me>('GET', '/api/auth/me'),
  login: (password: string) => call<Me>('POST', '/api/auth/login', { password }),
  logout: () => call<void>('POST', '/api/auth/logout'),
  link: (token: string) => call<{ purpose: LinkPurpose }>('POST', '/api/auth/link', { token }),
  setup: (token: string, password: string) =>
    call<Me>('POST', '/api/auth/setup', { token, password }),
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
  /** The panel's hostname. */
  panel: string
  version: string
}

export const fetchInstallation = () => call<Installation>('GET', '/api/installation')

/** A site of the installation, with its addresses. */
export interface SiteInfo {
  id: string
  /** Names the preview and the panel's addresses; never changes. */
  slug: string
  name: string
  /** The hostname of its preview. */
  preview_host: string
  /** The hostnames of the published site; none before it has a domain. */
  site_hosts: string[]
}

export const sites = {
  list: () => call<SiteInfo[]>('GET', '/api/sites'),
  create: (name: string, slug: string) => call<SiteInfo>('POST', '/api/sites', { name, slug }),
  remove: (id: string) => call<void>('DELETE', `/api/sites/${encodeURIComponent(id)}`),
}

/** Address of a sibling hostname, with the scheme and port of the panel. */
export function siblingUrl(host: string): string {
  const port = window.location.port ? `:${window.location.port}` : ''
  return `${window.location.protocol}//${host}${port}/`
}

/** One entry of the activity log. */
export interface ActivityEntry {
  id: number
  at: string
  /** The site the entry is about; null for the installation itself. */
  site_id: string | null
  environment: 'installation' | 'preview' | 'production'
  action: string
  /** The owner, or null when alogeno itself acted. */
  actor_id: number | null
  subject: string | null
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
  /** Whether a version of the site is online. */
  live: boolean
}

export const fetchBuild = () => call<BuildStatus>('GET', sitePath('/build'))

/** An assistant this installation offers. */
export interface OfferedAssistant {
  provider: ProviderId
  name: string
  /** It keeps a sign-in on this server. */
  saved: boolean
  capabilities: { subscription: boolean; api_key: boolean; terminal: boolean }
  /** The modes of its CLI, as the CLI names them; the first is the default. */
  modes: string[]
  /** Hosts it reaches on its own. */
  hosts: string[]
}

/** The agent's account, as the CLI reports it. */
export interface AgentAccount {
  provider: ProviderId
  signed_in: boolean
  method: string | null
  /** The last characters of the API key, when the assistant uses one. */
  key_ending: string | null
  expires_at: string | null
  email: string | null
  plan: string | null
  signing_in: boolean
  /** For a sign-in with a code: the code to enter, and where. */
  user_code: string | null
  verification_url: string | null
  problem: string | null
  /** The assistant in use, and those offered. */
  active?: ProviderId
  assistants?: OfferedAssistant[]
}

export const agent = {
  account: (provider?: ProviderId) =>
    call<AgentAccount>('GET', `/api/agent/account${provider ? `?provider=${provider}` : ''}`),
  /** Starts the sign-in: the address of the sign-in page and, for a sign-in
   * with a code, the code to enter there. */
  login: (provider?: ProviderId) =>
    call<{ url: string; code: string | null; connected: boolean }>('POST', '/api/agent/login', {
      provider,
    }),
  code: (code: string) => call<AgentAccount>('POST', '/api/agent/login/code', { code }),
  /** Connects an assistant with an API key of its vendor instead. */
  key: (key: string, provider?: ProviderId) =>
    call<AgentAccount>('POST', '/api/agent/key', { key, provider }),
  logout: (provider?: ProviderId) => call<void>('POST', '/api/agent/logout', { provider }),
  /** The models the assistant in use can work with. */
  models: () => call<{ models: AssistantModel[] }>('GET', '/api/agent/models'),
  /** Makes another assistant, already connected, the one in use. */
  use: (provider: ProviderId) => call<AgentAccount>('POST', '/api/agent/use', { provider }),
  /** The sites people allowed the assistant to reach, and those the
   * configuration allows. */
  hosts: () => call<{ hosts: AllowedHost[]; configured: string[] }>('GET', sitePath('/agent/hosts')),
  removeHost: (host: string) =>
    call<void>('DELETE', sitePath(`/agent/hosts/${encodeURIComponent(host)}`)),
}

/** A site a person allowed the assistant to reach, in the chat. */
export interface AllowedHost {
  host: string
  by: number | null
  by_name: string | null
  at: string
}

/** One line of the chat. */
export interface ChatItem {
  id: number
  kind: 'person' | 'answer' | 'thinking' | 'activity' | 'notice' | 'error' | 'permission' | 'question' | 'link'
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

export interface ChatSnapshot {
  conversation: ChatConversation | null
  items: ChatItem[]
  turn: ChatTurn | null
  limits: unknown
  /** Who has the terminal open: the chat waits meanwhile. */
  terminal: { id: number; name: string } | null
  problem: ChatProblem | null
  /** The mode of the assistant's CLI for the next message. */
  mode: string
  /** The model for the next message; the CLI's default if null. */
  model: string | null
}

/** A conversation in the list of past ones. */
export interface ConversationSummary {
  id: number
  title: string
  /** The assistant it is with. */
  provider: string
  updated_at: string
  /** Messages people sent in it. */
  messages: number
  /** It is the one the chat shows. */
  current: boolean
}

/** A model the assistant's CLI can use. */
export interface AssistantModel {
  id: string
  name: string
  detail: string
  /** The CLI uses it when none is chosen. */
  default: boolean
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
          terminal: { id: number; name: string } | null
      problem: ChatProblem | null
    }
  | { type: 'limits'; limits: unknown }
  | { type: 'mode'; mode: string }
  | { type: 'model'; model: string | null }
  | { type: 'reset' }

export const chat = {
  /** The chat's stream of events, for the site the panel works on. */
  eventsUrl: () => sitePath('/chat/events'),
  send: (text: string) => call<void>('POST', sitePath('/chat/messages'), { text }),
  stop: () => call<void>('POST', sitePath('/chat/stop')),
  newConversation: () => call<void>('POST', sitePath('/chat/new')),
  /** The past conversations, newest first. */
  conversations: () => call<{ conversations: ConversationSummary[] }>('GET', sitePath('/chat/conversations')),
  /** Goes back to a past conversation of the assistant in use. */
  openConversation: (id: number) => call<void>('POST', sitePath(`/chat/conversations/${id}`)),
  /** Chooses the mode of the assistant's CLI for the next messages. */
  setMode: (mode: string) => call<{ mode: string }>('POST', sitePath('/chat/mode'), { mode }),
  /** Chooses the model for the next messages; null for the CLI's default. */
  setModel: (model: string | null) =>
    call<{ model: string | null }>('POST', sitePath('/chat/model'), { model }),
  /** Takes the site back to before a message; returns the message. */
  goBack: (id: number) => call<{ text: string }>('POST', sitePath(`/chat/back/${id}`)),
  /** Undoes a going back. */
  undoBack: (id: number) => call<void>('POST', sitePath(`/chat/undo/${id}`)),
  /** Tries the last request again after something went wrong. */
  retry: () => call<void>('POST', sitePath('/chat/retry')),
  /** Answers the questions of the assistant, one answer per question. */
  answerQuestions: (id: number, answers: QuestionAnswer[]) =>
    call<void>('POST', sitePath(`/chat/questions/${id}`), { answers }),
  /** Answers a permission request of the assistant. */
  answerPermission: (id: number, allow: boolean) =>
    call<void>('POST', sitePath(`/chat/permissions/${id}`), { allow }),
}

export const terminal = {
  /** Ends the terminal: its changes are saved and the chat goes on. */
  close: () => call<void>('POST', sitePath('/terminal/close')),
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
  list: () => call<{ pages: PageSummary[]; locked: string | null }>('GET', sitePath('/pages')),
  read: (id: string) => call<Page>('GET', sitePath(`/pages/${id}`)),
  create: (page: Partial<Page>) => call<Page>('POST', sitePath('/pages'), { page }),
  update: (id: string, changes: Partial<Page>) =>
    call<Page>('PATCH', sitePath(`/pages/${id}`), { changes }),
  remove: (id: string) => call<void>('DELETE', sitePath(`/pages/${id}`)),
}

/** A part several pages share, such as a footer. */
export interface ComponentSummary {
  id: string
  name: string
  /** The name the site's code finds it by. */
  key: string
  everywhere: boolean
  /** The pages it appears on, chosen one by one. */
  pages: string[]
  /** The pages whose pages inside show it. */
  inside: string[]
}

export interface Component {
  id: string
  name: string
  key: string
  blocks: unknown[]
  everywhere: boolean
  pages: { page: string }[]
  inside: { page: string }[]
}

export const components = {
  list: () =>
    call<{ components: ComponentSummary[]; locked: string | null }>('GET', sitePath('/components')),
  read: (id: string) => call<Component>('GET', sitePath(`/components/${id}`)),
  create: (component: Partial<Component>) =>
    call<Component>('POST', sitePath('/components'), { component }),
  update: (id: string, changes: Partial<Component>) =>
    call<Component>('PATCH', sitePath(`/components/${id}`), { changes }),
  remove: (id: string) => call<void>('DELETE', sitePath(`/components/${id}`)),
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
  list: () => call<PageType[]>('GET', sitePath('/page-types')),
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
  list: () => call<{ media: MediaItem[]; locked: string | null }>('GET', sitePath('/media')),
  /** The address of a picture's file, for the panel. */
  fileUrl: (id: string) => sitePath(`/media/${encodeURIComponent(id)}/file`),
  /** Uploads a file; the server checks it is a picture. */
  upload: async (file: File): Promise<MediaItem> => {
    const response = await fetch(sitePath(`/media?name=${encodeURIComponent(file.name)}`), {
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
    call<MediaItem>('PATCH', sitePath(`/media/${encodeURIComponent(id)}`), { changes }),
  remove: (id: string) => call<void>('DELETE', sitePath(`/media/${encodeURIComponent(id)}`)),
}

export type ChangeStatus = 'created' | 'changed' | 'deleted'

/** A page or picture that is not online as it is now. */
export interface PendingItem {
  id: string
  status: ChangeStatus
  /** The title or name; for a deleted item, as it is online. */
  title: string
  /** Who made the last change, when known. */
  by: number | null
  /** When the last change was saved, in seconds since 1970. */
  at: number
}

/** What is not online yet. */
export interface Changes {
  /** Pages, pictures and components one each, code and schema as one. */
  count: number
  pending: {
    published: string | null
    /** The last saved version: a publication confirms against it. */
    head: string | null
    pages: PendingItem[]
    media: PendingItem[]
    components: PendingItem[]
    files: { path: string; status: ChangeStatus }[]
    versions: { commit: string; subject: string; by: number | null; at: number }[]
  }
  /** Why the site cannot be published now, if it cannot. */
  blocked: string | null
  last_publication: { at: string; by: number | null; changes: number } | null
  /** An earlier publication is kept, to put back online. */
  previous_publication: boolean
}

/** A selection of the changes, checked by the server. */
export interface PublishPlan {
  /** Every change that goes online: the ticked ones and those they need. */
  keys: string[]
  included: { key: string; reason: string }[]
  /** Why the selection cannot go online by itself. */
  problems: string[]
  all: boolean
}

export const changes = {
  list: () => call<Changes>('GET', sitePath('/changes')),
  /** Puts online the version in the preview; `commit` is the version the
   * list was read for. */
  publish: (commit: string, items?: string[]) =>
    call<{ release: string; commit: string; changes: number }>('POST', sitePath('/publish'), { commit, items }),
  /** What a selection brings with it, and its problems; builds nothing. */
  plan: (commit: string, items: string[]) =>
    call<PublishPlan>('POST', sitePath('/publish/plan'), { commit, items }),
  /** Puts the publication before the last one back online. */
  back: () => call<{ release: string; commit: string }>('POST', sitePath('/publish/back')),
}

/** The preview is private: its host takes a one-time code of the panel and
 * keeps a session of its own. */
export const preview = {
  code: () => call<{ code: string }>('POST', sitePath('/preview/code')),
}

/** The address that enters the preview and shows `to`, one of its paths. */
export async function previewEntry(previewUrl: string, to: string): Promise<string> {
  const { code } = await preview.code()
  return `${previewUrl.replace(/\/$/, '')}/_alogeno/enter?code=${encodeURIComponent(code)}&to=${encodeURIComponent(to)}`
}

/** Opens a page of the preview in a new tab. The tab opens at once, so the
 * browser does not take it for an unwanted popup, and goes to the page
 * once the code is there. */
export function openPreview(previewUrl: string, to: string) {
  const tab = window.open('', '_blank')
  previewEntry(previewUrl, to).then(
    (url) => {
      if (tab) tab.location.href = url
      else window.location.href = url
    },
    () => tab?.close(),
  )
}

export interface UpdateStatus {
  current: string
  /** A newer release, when there is one. */
  available: string | null
  /** The installation can update from the panel. */
  from_panel: boolean
  /** An update was asked for and has not started yet. */
  requested: boolean
  /** Installed with Nix: updates come from the system configuration. */
  nix: boolean
}

export const updates = {
  status: () => call<UpdateStatus>('GET', '/api/update'),
  request: () => call<{ requested: string }>('POST', '/api/update'),
}
