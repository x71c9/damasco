import type { PageSummary } from '../api'

/** Where a component appears, in words: "On every page", "On Home and
 * News", "On the pages inside Projects", or both of the last two. */
export function whereText(
  where: { everywhere: boolean; pages: string[]; inside: string[] },
  list: PageSummary[],
): string {
  if (where.everywhere) return 'On every page'
  const title = (id: string) => list.find((page) => page.id === id)?.title ?? 'a page'
  const names = (ids: string[]) =>
    ids.length <= 1
      ? ids.map(title).join('')
      : `${ids.slice(0, -1).map(title).join(', ')} and ${title(ids[ids.length - 1])}`
  const parts = [
    ...(where.pages.length ? [`On ${names(where.pages)}`] : []),
    ...(where.inside.length ? [`${where.pages.length ? 'on' : 'On'} the pages inside ${names(where.inside)}`] : []),
  ]
  return parts.length ? parts.join(', and ') : 'On no page yet'
}
