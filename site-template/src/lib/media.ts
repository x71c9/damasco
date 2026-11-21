import { getCollection } from 'astro:content'

export type Media = Awaited<ReturnType<typeof allMedia>>[number]

export async function allMedia() {
  return (await getCollection('media')).map((entry) => entry.data)
}

const extensions: Record<string, string> = {
  'image/jpeg': 'jpg',
  'image/png': 'png',
  'image/gif': 'gif',
  'image/webp': 'webp',
  'image/avif': 'avif',
}

/** Address of a picture's file: damasco puts the files of the pictures in
 * every release, under /_damasco/media/. */
export function mediaUrl(item: Media): string {
  return `/_damasco/media/${item.sha256}.${extensions[item.type] ?? 'bin'}`
}
