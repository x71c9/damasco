import { getCollection } from 'astro:content'

export type Page = Awaited<ReturnType<typeof allPages>>[number]

export async function allPages() {
  return (await getCollection('pages')).map((entry) => entry.data)
}

/** Public path of a page, from the slugs of the page and the pages it is inside. */
export function pathOf(page: Page, byId: Map<string, Page>): string {
  const slugs: string[] = []
  let current: Page | undefined = page
  const seen = new Set<string>()
  while (current && !seen.has(current.id)) {
    seen.add(current.id)
    if (current.slug) slugs.unshift(current.slug)
    current = current.inside ? byId.get(current.inside) : undefined
  }
  return '/' + slugs.map(encodeURIComponent).join('/') + (slugs.length ? '/' : '')
}

/** Pages shown in the site menu, in their order. */
export function menu(pages: Page[]): Page[] {
  return pages.filter((page) => page.menu).sort((a, b) => a.order - b.order)
}
