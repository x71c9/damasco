import { useEffect, useRef, useState } from 'react'
import { pages, type PageSummary, type SiteInfo } from '../api'
import { useChanges } from '../changes/context'
import type { Theme } from '../theme'
import { Icon, Mark, type IconName } from './icons'
import type { Section } from './NavRail'
import { ThemeButton } from './ThemeButton'
import { SiteMenu } from '../sites/SiteMenu'
import './Header.css'

export type Device = 'computer' | 'tablet' | 'phone'
export type MobileView = 'chat' | 'preview'
/** Why Publish waits: the site has no domain yet, the assistant is at work
 * on it, or the terminal is open on it. */
export type PublishBlock = 'domain' | 'turn' | 'terminal'

/** What the Publish button says while it waits. */
const blockTitles: Record<PublishBlock, string> = {
  domain: 'The site has no domain yet: add one in Settings › Sites',
  turn: 'The assistant is working on the site: publish when it finishes',
  terminal: 'The terminal is open on the site: close it to publish',
}

const devices: { id: Device; label: string }[] = [
  { id: 'computer', label: 'Computer' },
  { id: 'tablet', label: 'Tablet' },
  { id: 'phone', label: 'Phone' },
]

interface HeaderProps {
  /** Website shows the preview controls; other sections show their name. */
  section: Section
  /** The site the panel works on, and every site, to change site. */
  site: SiteInfo
  sites: SiteInfo[]
  /** The installation allows a new site. */
  canCreateSite: boolean
  device: Device
  onDevice: (device: Device) => void
  onReload: () => void
  /** Opens the list of changes to publish. */
  onPublish: () => void
  /** Why the site cannot be published now, if it cannot. */
  publishBlock: PublishBlock | null
  /** Open the preview, and the site online, in a new tab at the page the
   * preview shows. */
  onOpenPreview: () => void
  /** Null until the site has a domain and a version online: no Live then. */
  onOpenLive: (() => void) | null
  mobileView: MobileView
  onMobileView: (view: MobileView) => void
  theme: Theme
  onToggleTheme: () => void
  /** Shows the site: the panel's home. */
  onHome: () => void
  /** The page the preview opens; null for the home. */
  page: ShownPage | null
  onPage: (page: ShownPage | null) => void
}

/** A page of the site the preview can open. */
export interface ShownPage {
  title: string
  /** Its public address, as `/about/`. */
  address: string
}

/** The pages of the site, to open one in the preview. The list is read
 * each time it opens, so new pages are there. Links followed inside the
 * preview do not change it: it names the page the preview opened. */
function PageMenu({ page, onPage }: { page: ShownPage | null; onPage: (page: ShownPage | null) => void }) {
  const [open, setOpen] = useState(false)
  const [list, setList] = useState<PageSummary[] | null>(null)
  const [failed, setFailed] = useState(false)
  const root = useRef<HTMLDivElement>(null)
  const title = page?.title ?? 'Home'

  useEffect(() => {
    if (!open) return
    const close = (event: MouseEvent | KeyboardEvent) => {
      if (event instanceof KeyboardEvent ? event.key === 'Escape' : !root.current?.contains(event.target as Node)) {
        setOpen(false)
      }
    }
    document.addEventListener('mousedown', close)
    document.addEventListener('keydown', close)
    return () => {
      document.removeEventListener('mousedown', close)
      document.removeEventListener('keydown', close)
    }
  }, [open])

  const toggle = () => {
    if (!open) {
      setFailed(false)
      pages.list().then(
        (result) => setList(result.pages),
        () => setFailed(true),
      )
    }
    setOpen((value) => !value)
  }

  // How deep each page sits, from the page it is inside.
  const depth = (summary: PageSummary): number => {
    let level = 0
    let inside = summary.inside
    while (inside && level < 8) {
      level += 1
      const parent: string | null = inside
      inside = list?.find((other) => other.id === parent)?.inside ?? null
    }
    return level
  }
  const current = page?.address ?? '/'

  return (
    <div className="header-pages" ref={root}>
      <button
        className="header-crumb"
        aria-label={`Page in the preview: ${title}. Change page`}
        title="The page the preview opens"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={toggle}
      >
        {title}
        <span className="header-chevron">
          <Icon name="chevronDown" size={14} strokeWidth={2} />
        </span>
      </button>
      {open && (
        <div className="header-page-menu" role="menu" aria-label="Pages">
          {failed ? (
            <p className="header-page-note">The pages could not be read. Open the menu again to retry.</p>
          ) : list === null ? (
            <p className="header-page-note">Reading the pages…</p>
          ) : list.length === 0 ? (
            <p className="header-page-note">The site has no pages yet.</p>
          ) : (
            list.map((summary) => (
              <button
                key={summary.id}
                className="header-page-item"
                role="menuitemradio"
                aria-checked={summary.address === current}
                style={{ paddingLeft: `calc(var(--space-3) + ${depth(summary) * 14}px)` }}
                onClick={() => {
                  setOpen(false)
                  onPage(summary.address === '/' ? null : { title: summary.title, address: summary.address })
                }}
              >
                <span className="header-page-title">{summary.title}</span>
                <span className="header-page-address">{summary.address}</span>
              </button>
            ))
          )}
        </div>
      )}
    </div>
  )
}

export function Header({
  section,
  site,
  sites,
  canCreateSite,
  device,
  onDevice,
  onReload,
  onPublish,
  publishBlock,
  onOpenPreview,
  onOpenLive,
  mobileView,
  onMobileView,
  theme,
  onToggleTheme,
  onHome,
  page,
  onPage,
}: HeaderProps) {
  const website = section === 'website'
  // Each page and picture counts once, the code and schema once.
  const pending = useChanges().changes?.count ?? 0
  return (
    <header className="header">
      {/* The mark and the site's name lead to the site, the panel's home. */}
      <button className="header-home" aria-label="The site" title="The site" onClick={onHome}>
        <Mark size={24} label="damasco" />
      </button>
      <SiteMenu current={site} list={sites} canCreate={canCreateSite} />
      <span className="header-separator" aria-hidden="true">
        /
      </span>
      {website ? (
        <>
          <PageMenu page={page} onPage={onPage} />
          <button className="icon-button header-reload" aria-label="Reload the preview" title="Reload" onClick={onReload}>
            <Icon name="reload" size={16} />
          </button>
        </>
      ) : (
        <span className="header-section">
          {
            {
              pages: 'Pages',
              components: 'Components',
              media: 'Media',
              assistant: 'Settings',
              sites: 'Settings',
              instructions: 'Settings',
              builds: 'Settings',
            }[section as Exclude<Section, 'website'>]
          }
        </span>
      )}

      <span className="header-spacer" />

      {website && (
      <div className="header-views" role="group" aria-label="View">
        {(['chat', 'preview'] as const).map((view) => (
          <button
            key={view}
            aria-pressed={mobileView === view}
            onClick={() => onMobileView(view)}
          >
            {view === 'chat' ? 'Chat' : 'Preview'}
          </button>
        ))}
      </div>
      )}

      {website && (
      <div className="header-devices" role="group" aria-label="Preview width">
        {devices.map(({ id, label }) => (
          <button
            key={id}
            className="icon-button"
            aria-label={label}
            title={label}
            aria-pressed={device === id}
            onClick={() => onDevice(id)}
          >
            <Icon name={id satisfies IconName} size={17} />
          </button>
        ))}
      </div>
      )}
      {website && <span className="header-divider" aria-hidden="true" />}
      {/* The frame shows the preview; these open the preview and the site
          online in a new tab. Live appears once the site has a domain and a
          version online. */}
      {website && (
        <div className="header-views header-open" role="group" aria-label="Open in a new tab">
          <button title="Open the preview in a new tab" aria-label="Open the preview in a new tab" onClick={onOpenPreview}>
            <Icon name="eye" size={15} />
            <span className="header-open-label">Preview</span>
          </button>
          {onOpenLive && (
            <button title="Open the site online in a new tab" aria-label="Open the site online in a new tab" onClick={onOpenLive}>
              <Icon name="website" size={15} />
              <span className="header-open-label">Live</span>
            </button>
          )}
        </div>
      )}

      <ThemeButton theme={theme} onToggle={onToggleTheme} />

      {/* Without a domain the button looks disabled but still opens a
          dialog that says how to add one: a hover alone does not reach a
          phone. While the assistant or the terminal works on the site, it
          is disabled and its title says why. */}
      <button
        className="publish"
        disabled={pending === 0 || publishBlock === 'turn' || publishBlock === 'terminal'}
        aria-disabled={publishBlock === 'domain' || undefined}
        aria-haspopup="dialog"
        onClick={onPublish}
        aria-label={
          pending === 0
            ? 'Publish: no changes to publish'
            : publishBlock
              ? `Publish: ${blockTitles[publishBlock]}`
              : `Publish ${pending} ${pending === 1 ? 'change' : 'changes'}`
        }
        title={
          pending === 0
            ? 'No changes to publish'
            : publishBlock
              ? blockTitles[publishBlock]
              : `${pending} ${pending === 1 ? 'change' : 'changes'} not online yet`
        }
      >
        Publish
        <span className="publish-count">{pending}</span>
      </button>
    </header>
  )
}
