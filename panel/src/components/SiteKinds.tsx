import './SiteKinds.css'

type Kind = 'pages' | 'components' | 'media'

const labels: Record<Kind, string> = {
  pages: 'Pages',
  components: 'Components',
  media: 'Media',
}

/** On a phone, above the lists of the Site tab: its kinds of content. The
 * rail of computers shows them as icons under the globe. */
export function SiteKinds({ kinds, current, onOpen }: { kinds: Kind[]; current: Kind; onOpen: (kind: Kind) => void }) {
  return (
    <nav className="site-kinds" aria-label="Content of the site">
      {kinds.map((kind) => (
        <button key={kind} aria-current={kind === current ? 'page' : undefined} onClick={() => onOpen(kind)}>
          {labels[kind]}
        </button>
      ))}
    </nav>
  )
}
