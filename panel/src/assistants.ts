import type { IconName } from './components/icons'
/** The texts of each assistant the panel can connect. Which assistants an
 * installation offers, and what they can do, comes from the server. */

export type ProviderId = 'claude' | 'codex'

export interface AssistantText {
  id: ProviderId
  /** The name people know the assistant by. */
  name: string
  badge: string
  company: string
  /** The plans that include it. */
  plans: string
  /** Where the subscription's sign-in happens. */
  site: string
  siteUrl: string
  /** How the subscription is described on the choice. */
  subscription: string
  keyPlaceholder: string
  keyConsole: { label: string; url: string }
  usageUrl: string
  /** Labels of the ways the CLI reports it is signed in. */
  methods: Record<string, string>
  /** The method of an API key. */
  keyMethod: string
  /** The modes of the CLI, by the name the CLI gives them. */
  modes: Record<string, { label: string; detail: string; icon: IconName }>
}

export const assistants: Record<ProviderId, AssistantText> = {
  claude: {
    id: 'claude',
    name: 'Claude',
    badge: 'C',
    company: 'Anthropic',
    plans: 'Claude Pro or Max',
    site: 'claude.ai',
    siteUrl: 'https://claude.ai',
    subscription: 'Claude Pro or Max, signing in on claude.ai',
    keyPlaceholder: 'sk-ant-…',
    keyConsole: { label: 'Anthropic Console', url: 'https://console.anthropic.com/settings/keys' },
    usageUrl: 'https://console.anthropic.com/settings/usage',
    methods: { 'claude.ai': 'Claude subscription, Pro or Max', api_key_helper: 'API key' },
    keyMethod: 'api_key_helper',
    modes: {
      acceptEdits: { icon: 'pencil', label: 'Accept edits', detail: 'Changes files on its own, asks before other commands' },
      manual: { icon: 'hand', label: 'Ask first', detail: 'Asks before each change and command' },
      auto: { icon: 'bolt', label: 'Auto', detail: 'Claude decides what needs asking' },
      plan: { icon: 'plan', label: 'Plan', detail: 'Proposes a plan first, changes nothing until it is approved' },
      dontAsk: { icon: 'circleMinus', label: 'Don’t ask', detail: 'Refuses what is not already allowed, without asking' },
    },
  },
  codex: {
    id: 'codex',
    name: 'ChatGPT',
    badge: 'G',
    company: 'OpenAI',
    plans: 'ChatGPT Plus, Pro or Business',
    site: 'chatgpt.com',
    siteUrl: 'https://chatgpt.com',
    subscription: 'ChatGPT Plus, Pro or Business, with a code on chatgpt.com',
    keyPlaceholder: 'sk-proj-…',
    keyConsole: { label: 'OpenAI Platform', url: 'https://platform.openai.com/api-keys' },
    usageUrl: 'https://platform.openai.com/usage',
    methods: { chatgpt: 'ChatGPT subscription', apiKey: 'API key' },
    keyMethod: 'apiKey',
    modes: {
      default: { icon: 'bolt', label: 'Default', detail: 'Works on the request' },
      plan: { icon: 'plan', label: 'Plan', detail: 'Proposes a plan first, then works on it when asked' },
    },
  },
}

export function textOf(id: string | null | undefined): AssistantText {
  return assistants[(id as ProviderId) ?? 'claude'] ?? assistants.claude
}
