// The site's content: every page is a JSON file in content/pages, written by
// axioma. This schema mirrors the page format of axioma.
import { defineCollection } from 'astro:content'
import { glob } from 'astro/loaders'
import { z } from 'astro/zod'

const media = z.object({ media: z.string() })

const block = z.discriminatedUnion('type', [
  z.object({ id: z.string(), type: z.literal('text'), body: z.any() }),
  z.object({
    id: z.string(),
    type: z.literal('media'),
    media: media.nullable(),
    caption: z.string().nullable().optional(),
  }),
])

const pages = defineCollection({
  loader: glob({
    pattern: '*.json',
    base: './content/pages',
    generateId: ({ entry }) => entry.replace(/\.json$/, ''),
  }),
  schema: z.object({
    id: z.string(),
    type: z.string(),
    lang: z.string(),
    inside: z.string().nullable(),
    order: z.number(),
    slug: z.string(),
    title: z.string(),
    menu: z.boolean(),
    fields: z.record(z.string(), z.unknown()),
    blocks: z.array(block),
    seo: z.object({
      title: z.string().nullable(),
      description: z.string().nullable(),
      image: media.nullable(),
      hidden: z.boolean(),
    }),
  }),
})

export const collections = { pages }
