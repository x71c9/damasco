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

/** Address of a picture's file: axioma puts the files of the pictures in
 * every release, under /_axioma/media/. */
export function mediaUrl(item: Media): string {
  return `/_axioma/media/${item.sha256}.${extensions[item.type] ?? 'bin'}`
}
