// Renders the rich text of alogeno (ProseMirror JSON limited to paragraphs,
// headings, lists, quotes and the bold, italic, code and link marks) to HTML.

type Mark = { type: string; attrs?: { href?: string; page?: string } }
type Node = { type: string; text?: string; marks?: Mark[]; attrs?: { level?: number }; content?: Node[] }

const escape = (text: string) =>
  text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;')

export function renderRichText(doc: unknown, linkTo: (page: string) => string): string {
  const node = doc as Node | null
  if (!node || typeof node !== 'object') return ''
  return render(node, linkTo)
}

function render(node: Node, linkTo: (page: string) => string): string {
  const inner = () => (node.content ?? []).map((child) => render(child, linkTo)).join('')
  switch (node.type) {
    case 'doc':
      return inner()
    case 'paragraph':
      return `<p>${inner()}</p>`
    case 'heading': {
      const level = Math.min(6, Math.max(2, node.attrs?.level ?? 2))
      return `<h${level}>${inner()}</h${level}>`
    }
    case 'bulletList':
      return `<ul>${inner()}</ul>`
    case 'orderedList':
      return `<ol>${inner()}</ol>`
    case 'listItem':
      return `<li>${inner()}</li>`
    case 'blockquote':
      return `<blockquote>${inner()}</blockquote>`
    case 'hardBreak':
      return '<br>'
    case 'text':
      return (node.marks ?? []).reduce((html, mark) => wrap(html, mark, linkTo), escape(node.text ?? ''))
    default:
      return inner()
  }
}

function wrap(html: string, mark: Mark, linkTo: (page: string) => string): string {
  switch (mark.type) {
    case 'bold':
      return `<strong>${html}</strong>`
    case 'italic':
      return `<em>${html}</em>`
    case 'code':
      return `<code>${html}</code>`
    case 'link': {
      const href = mark.attrs?.page ? linkTo(mark.attrs.page) : (mark.attrs?.href ?? '#')
      return `<a href="${escape(href)}">${html}</a>`
    }
    default:
      return html
  }
}
