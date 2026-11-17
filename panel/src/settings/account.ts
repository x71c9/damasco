import type { AgentAccount } from '../api'

/** How the assistant is connected, in a few words. */
export function connectedWith(account: AgentAccount): string {
  if (account.method === 'claude.ai') return 'Claude subscription, Pro or Max'
  if (account.method === 'api_key_helper') {
    return account.key_ending ? `API key ending in ${account.key_ending}` : 'API key'
  }
  return 'Signed in'
}
