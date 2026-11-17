import { getCollection } from 'astro:content'
import type { Page } from './pages'

export type Component = Awaited<ReturnType<typeof allComponents>>[number]

export async function allComponents() {
  return (await getCollection('components')).map((entry) => entry.data)
}

/** Whether `component` appears on `page`: everywhere, on the pages chosen,
 * or on any page inside the pages in `inside`. */
export function appearsOn(component: Component, page: Page, byId: Map<string, Page>): boolean {
  if (component.everywhere) return true
  if (component.pages.some((chosen) => chosen.page === page.id)) return true
  const parents = new Set(component.inside.map((chosen) => chosen.page))
  const seen = new Set<string>()
  let current = page.inside ? byId.get(page.inside) : undefined
  while (current && !seen.has(current.id)) {
    if (parents.has(current.id)) return true
    seen.add(current.id)
    current = current.inside ? byId.get(current.inside) : undefined
  }
  return false
}

/** The component with `key`, when it appears on `page`. */
export async function component(
  key: string,
  page: Page,
  byId: Map<string, Page>,
): Promise<Component | undefined> {
  const found = (await allComponents()).find((item) => item.key === key)
  return found && appearsOn(found, page, byId) ? found : undefined
}
