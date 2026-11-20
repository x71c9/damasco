import { useEffect, useRef, useState, type SubmitEvent } from 'react'
import { ApiError, sites, type SiteInfo } from '../api'
import { Button } from '../components/Button'
import { Dialog, DialogActions } from '../components/Dialog'
import { Field } from '../components/Field'
import { Icon } from '../components/icons'
import './Sites.css'

/** Opens the panel on the site `slug`, from its start. */
function openSiteAt(slug: string) {
  window.location.assign(`/s/${encodeURIComponent(slug)}/`)
}

/** The site's name in the header, and the menu that changes site or makes
 * a new one. */
export function SiteMenu({ current, list }: { current: SiteInfo; list: SiteInfo[] }) {
  const [open, setOpen] = useState(false)
  const [creating, setCreating] = useState(false)
  const root = useRef<HTMLDivElement>(null)

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

  return (
    <div className="header-pages site-menu" ref={root}>
      <button
        className="header-crumb header-site"
        aria-label={`Site: ${current.name}. Change site`}
        title="The site the panel works on"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        {current.name}
        <span className="header-chevron">
          <Icon name="chevronDown" size={14} strokeWidth={2} />
        </span>
      </button>
      {open && (
        <div className="header-page-menu" role="menu" aria-label="Sites">
          {list.map((site) => (
            <button
              key={site.id}
              className="header-page-item"
              role="menuitemradio"
              aria-checked={site.id === current.id}
              onClick={() => {
                setOpen(false)
                if (site.id !== current.id) openSiteAt(site.slug)
              }}
            >
              <span className="header-page-title">{site.name}</span>
              <span className="header-page-address">{site.slug}</span>
            </button>
          ))}
          <button
            className="header-page-item site-menu-new"
            role="menuitem"
            onClick={() => {
              setOpen(false)
              setCreating(true)
            }}
          >
            <span className="header-page-title">
              <Icon name="plus" size={15} /> New site
            </span>
          </button>
        </div>
      )}
      {creating && <CreateSiteDialog onClose={() => setCreating(false)} />}
    </div>
  )
}

/** The slug proposed for a name: lowercase letters, digits and single
 * hyphens, at most 32 characters. */
function slugOf(name: string): string {
  return name
    .normalize('NFKD')
    .replace(/[̀-ͯ]/g, '')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 32)
    .replace(/-+$/, '')
}

const RESERVED = ['preview', 'admin', 'www', 'api']

/** The same rule as the server's, so that most mistakes show at once. */
function slugProblem(slug: string): string | null {
  if (!/^[a-z0-9]([a-z0-9-]{0,30}[a-z0-9])?$/.test(slug) || slug.includes('--')) {
    return '1 to 32 lowercase letters, digits and single hyphens, starting and ending with a letter or digit'
  }
  if (RESERVED.includes(slug)) return `“${slug}” is reserved`
  return null
}

/** Makes a new site from a name and its slug, which names its preview and
 * never changes. */
export function CreateSiteDialog({ onClose }: { onClose: () => void }) {
  const [name, setName] = useState('')
  const [slug, setSlug] = useState('')
  // The slug follows the name until it is edited by hand.
  const [slugEdited, setSlugEdited] = useState(false)
  const [errors, setErrors] = useState<{ name?: string; slug?: string }>({})
  const [busy, setBusy] = useState(false)

  const submit = async (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault()
    const found: { name?: string; slug?: string } = {}
    if (!name.trim()) found.name = 'Required'
    const problem = slugProblem(slug)
    if (problem) found.slug = problem
    setErrors(found)
    if (found.name || found.slug) return
    setBusy(true)
    try {
      const created = await sites.create(name.trim(), slug)
      openSiteAt(created.slug)
    } catch (err) {
      setErrors({ slug: err instanceof ApiError ? err.message : String(err) })
      setBusy(false)
    }
  }

  return (
    <Dialog title="New site" subtitle="An empty site, with its own preview and chat." onClose={onClose}>
      <form className="site-form" onSubmit={submit} noValidate>
        <Field
          id="site-name"
          label="Name"
          maxLength={80}
          error={errors.name}
          value={name}
          onChange={(event) => {
            setName(event.target.value)
            if (!slugEdited) setSlug(slugOf(event.target.value))
            setErrors((current) => ({ ...current, name: undefined }))
          }}
        />
        <Field
          id="site-slug"
          label="Short name"
          note="Names its preview; it cannot change later"
          autoCapitalize="none"
          spellCheck={false}
          maxLength={32}
          error={errors.slug}
          value={slug}
          onChange={(event) => {
            setSlug(event.target.value.toLowerCase())
            setSlugEdited(true)
            setErrors((current) => ({ ...current, slug: undefined }))
          }}
        />
        <DialogActions>
          <Button type="button" variant="quiet" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" variant="primary" disabled={busy}>
            Create the site
          </Button>
        </DialogActions>
      </form>
    </Dialog>
  )
}
