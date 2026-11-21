/**
 * Rich text between the two forms it takes: the ProseMirror document
 * damasco stores (paragraph, heading 2–4, bulletList, orderedList, listItem,
 * blockquote, hardBreak, text; marks bold, italic, code, link) and the
 * blocks BlockNote edits. Internal links are stored as `{ page: id }` and
 * edited as the address `page:<id>`.
 */

export interface PMMark {
  type: 'bold' | 'italic' | 'code' | 'link'
  attrs?: { href?: string; page?: string }
}

export interface PMNode {
  type: string
  content?: PMNode[]
  text?: string
  marks?: PMMark[]
  attrs?: Record<string, unknown>
}

interface Styles {
  bold?: boolean
  italic?: boolean
  code?: boolean
}

interface StyledText {
  type: 'text'
  text: string
  styles: Styles
}

interface LinkContent {
  type: 'link'
  href: string
  content: StyledText[]
}

type Inline = StyledText | LinkContent

/** A block as BlockNote reads and writes it, limited to what damasco keeps. */
export interface EditorBlock {
  type: 'paragraph' | 'heading' | 'bulletListItem' | 'numberedListItem' | 'quote'
  props?: { level?: number }
  content?: Inline[]
  children?: EditorBlock[]
}

export const emptyDoc: PMNode = { type: 'doc', content: [] }

const PAGE = 'page:'

function linkTarget(mark: PMMark): string | null {
  if (mark.attrs?.page) return PAGE + mark.attrs.page
  return mark.attrs?.href ?? null
}

/** ProseMirror inline nodes to BlockNote inline content. */
function toInline(nodes: PMNode[] = []): Inline[] {
  const out: Inline[] = []
  for (const node of nodes) {
    if (node.type === 'hardBreak') {
      const last = out[out.length - 1]
      if (last?.type === 'text') last.text += '\n'
      else out.push({ type: 'text', text: '\n', styles: {} })
      continue
    }
    if (node.type !== 'text' || !node.text) continue
    const styles: Styles = {}
    let link: string | null = null
    for (const mark of node.marks ?? []) {
      if (mark.type === 'link') link = linkTarget(mark)
      else styles[mark.type] = true
    }
    const text: StyledText = { type: 'text', text: node.text, styles }
    const last = out[out.length - 1]
    if (link) {
      if (last?.type === 'link' && last.href === link) last.content.push(text)
      else out.push({ type: 'link', href: link, content: [text] })
    } else {
      out.push(text)
    }
  }
  return out
}

/** BlockNote inline content to ProseMirror inline nodes. */
function fromInline(content: Inline[] = []): PMNode[] {
  const out: PMNode[] = []
  const push = (text: StyledText, link: string | null) => {
    const marks: PMMark[] = []
    if (text.styles.bold) marks.push({ type: 'bold' })
    if (text.styles.italic) marks.push({ type: 'italic' })
    if (text.styles.code) marks.push({ type: 'code' })
    if (link) {
      marks.push({
        type: 'link',
        attrs: link.startsWith(PAGE) ? { page: link.slice(PAGE.length) } : { href: link },
      })
    }
    text.text.split('\n').forEach((part, index) => {
      if (index > 0) out.push({ type: 'hardBreak' })
      if (part) out.push(marks.length ? { type: 'text', text: part, marks } : { type: 'text', text: part })
    })
  }
  for (const item of content) {
    if (item.type === 'link') item.content.forEach((text) => push(text, item.href))
    else push(item, null)
  }
  return out
}

function paragraph(content: Inline[] | undefined): PMNode {
  const inline = fromInline(content)
  return inline.length ? { type: 'paragraph', content: inline } : { type: 'paragraph' }
}

/** The text of the first paragraph of a list item, its nested lists as
 * children, and its other paragraphs as items of their own. */
function listItems(list: PMNode, type: EditorBlock['type']): EditorBlock[] {
  const out: EditorBlock[] = []
  for (const item of list.content ?? []) {
    const [first, ...rest] = item.content ?? []
    const children: EditorBlock[] = []
    const extra: EditorBlock[] = []
    for (const node of rest) {
      if (node.type === 'bulletList') children.push(...listItems(node, 'bulletListItem'))
      else if (node.type === 'orderedList') children.push(...listItems(node, 'numberedListItem'))
      else extra.push(...toBlocks([node]))
    }
    out.push({
      type,
      content: toInline(first?.type === 'paragraph' ? first.content : []),
      children,
    })
    out.push(...extra.map((block) => ({ ...block, type })))
  }
  return out
}

function toBlocks(nodes: PMNode[]): EditorBlock[] {
  const out: EditorBlock[] = []
  for (const node of nodes) {
    switch (node.type) {
      case 'paragraph':
        out.push({ type: 'paragraph', content: toInline(node.content) })
        break
      case 'heading': {
        const level = Math.min(4, Math.max(2, Number(node.attrs?.level ?? 2)))
        out.push({ type: 'heading', props: { level }, content: toInline(node.content) })
        break
      }
      case 'bulletList':
        out.push(...listItems(node, 'bulletListItem'))
        break
      case 'orderedList':
        out.push(...listItems(node, 'numberedListItem'))
        break
      case 'blockquote':
        for (const inner of node.content ?? []) {
          out.push({ type: 'quote', content: toInline(inner.content) })
        }
        break
    }
  }
  return out
}

/** A stored document as blocks for the editor. */
export function docToBlocks(doc: PMNode | null | undefined): EditorBlock[] {
  return toBlocks(doc?.content ?? [])
}

function listOf(blocks: EditorBlock[]): PMNode {
  const ordered = blocks[0].type === 'numberedListItem'
  return {
    type: ordered ? 'orderedList' : 'bulletList',
    content: blocks.map((block) => ({
      type: 'listItem',
      content: [paragraph(block.content), ...fromBlocks(block.children ?? [])],
    })),
  }
}

function fromBlocks(blocks: EditorBlock[]): PMNode[] {
  const out: PMNode[] = []
  let index = 0
  while (index < blocks.length) {
    const block = blocks[index]
    if (block.type === 'bulletListItem' || block.type === 'numberedListItem') {
      // Consecutive items of the same kind are one list.
      const run: EditorBlock[] = []
      while (index < blocks.length && blocks[index].type === block.type) run.push(blocks[index++])
      out.push(listOf(run))
      continue
    }
    if (block.type === 'quote') {
      const run: PMNode[] = []
      while (index < blocks.length && blocks[index].type === 'quote') {
        run.push(paragraph(blocks[index++].content))
      }
      out.push({ type: 'blockquote', content: run })
      continue
    }
    if (block.type === 'heading') {
      const level = Math.min(4, Math.max(2, Number(block.props?.level ?? 2)))
      const inline = fromInline(block.content)
      out.push(inline.length ? { type: 'heading', attrs: { level }, content: inline } : { type: 'heading', attrs: { level } })
    } else {
      out.push(paragraph(block.content))
    }
    // Blocks nested under a paragraph or a heading follow it.
    out.push(...fromBlocks(block.children ?? []))
    index += 1
  }
  return out
}

/** The editor's blocks as the document damasco stores. Empty paragraphs at
 * the end, which the editor always keeps, are left out. */
export function blocksToDoc(blocks: EditorBlock[]): PMNode {
  const content = fromBlocks(blocks)
  while (content.length && content[content.length - 1].type === 'paragraph' && !content[content.length - 1].content) {
    content.pop()
  }
  return { type: 'doc', content }
}
