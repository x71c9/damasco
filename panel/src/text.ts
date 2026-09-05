/** Capitalizes a server message for display ("wrong email" → "Wrong email"). */
export function sentence(message: string): string {
  return message.charAt(0).toUpperCase() + message.slice(1)
}
