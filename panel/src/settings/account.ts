import type { AgentAccount } from '../api'
import { textOf } from '../assistants'

/** How the assistant is connected, in a few words. */
export function connectedWith(account: AgentAccount): string {
  const text = textOf(account.provider)
  const method = account.method ? text.methods[account.method] : undefined
  if (account.method === text.keyMethod) {
    return account.key_ending ? `API key ending in ${account.key_ending}` : 'API key'
  }
  if (method) return account.email ? `${method}, ${account.email}` : method
  return 'Signed in'
}

/** Whether the assistant uses an API key. */
export function usesKey(account: AgentAccount): boolean {
  return account.method === textOf(account.provider).keyMethod
}
