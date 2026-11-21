import { useState } from 'react'
import { ApiError, siblingUrl, sites, type Installation, type SiteInfo } from '../api'
import { Button } from '../components/Button'
import { Dialog, DialogActions } from '../components/Dialog'
import { Field } from '../components/Field'
import '../components/Page.css'
import '../settings/Assistant.css'
import { DomainsSection } from './DomainsSection'
import { CreateSiteDialog } from './SiteMenu'
import './Sites.css'

/** Settings › Sites: every site of the installation; one of them, with
 * its addresses, opens below the list. */
export function SitesPage({
  list,
  current,
  open,
  onOpen,
  installation,
  onChanged,
}: {
  list: SiteInfo[]
  /** The site the panel works on. */
  current: SiteInfo
  /** The site shown in detail, by slug, if any. */
  open: string | null
  onOpen: (slug: string | null) => void
  installation: Installation | null
  /** A site's addresses changed: the list is read again. */
  onChanged: () => void
}) {
  const [creating, setCreating] = useState(false)
  const shown = list.find((site) => site.slug === open) ?? null

  return (
    <section className="page-frame" aria-labelledby="sites-title">
      <div className="page-card">
        <header className="page-head">
          <div>
            <h1 id="sites-title">Sites</h1>
            <p>
              Each site has its own pages, preview and chat. One sign-in to the assistant serves
              them all; it works on one site at a time.
            </p>
          </div>
          <Button variant="primary" onClick={() => setCreating(true)}>
            New site
          </Button>
        </header>

        <InstallationWarnings installation={installation} />

        <ul className="sites-list">
          {list.map((site) => (
            <li key={site.id}>
              <button
                className="sites-row"
                aria-current={site.slug === open ? 'true' : undefined}
                onClick={() => onOpen(site.slug === open ? null : site.slug)}
              >
                <span className="sites-row-text">
                  <span className="sites-row-name">
                    {site.name}
                    {site.id === current.id && <span className="sites-here">open in the panel</span>}
                  </span>
                  <span className="assistant-note">{site.slug}</span>
                </span>
                <span className="assistant-note">
                  {site.site_hosts.length > 0 ? site.site_hosts.join(', ') : 'No domain yet'}
                </span>
              </button>
            </li>
          ))}
        </ul>

        {shown && (
          <SiteDetail
            site={shown}
            current={current}
            only={list.length === 1}
            installation={installation}
            onChanged={onChanged}
          />
        )}
      </div>
      {creating && <CreateSiteDialog onClose={() => setCreating(false)} />}
    </section>
  )
}

/** One site: its addresses, its domains and how to delete it. */
function SiteDetail({
  site,
  current,
  only,
  installation,
  onChanged,
}: {
  site: SiteInfo
  current: SiteInfo
  only: boolean
  installation: Installation | null
  onChanged: () => void
}) {
  const [deleting, setDeleting] = useState(false)
  const [renaming, setRenaming] = useState(false)
  const preview = siblingUrl(site.preview_host)
  return (
    <div className="sites-detail">
      <section className="assistant-box" aria-labelledby="site-addresses-title">
        <h2 id="site-addresses-title">{site.name}</h2>
        <p className="assistant-note">
          Preview, for the owner only:{' '}
          <a href={preview} target="_blank" rel="noreferrer">
            {site.preview_host}
          </a>
        </p>
        {site.site_hosts.map((host) => (
          <p className="assistant-note" key={host}>
            Published at{' '}
            <a href={siblingUrl(host)} target="_blank" rel="noreferrer">
              {host}
            </a>
          </p>
        ))}
        <div className="assistant-actions">
          {site.id !== current.id && (
            <Button onClick={() => window.location.assign(`/s/${encodeURIComponent(site.slug)}/`)}>
              Open {site.name}
            </Button>
          )}
          <Button onClick={() => setRenaming(true)}>Rename</Button>
        </div>
      </section>

      <DomainsSection key={site.id} site={site} installation={installation} onChanged={onChanged} />

      <section className="assistant-box assistant-danger" aria-labelledby="site-delete-title">
        <h2 id="site-delete-title">Delete the site</h2>
        <p className="assistant-note">
          {only
            ? 'The only site of the installation stays: create another one first.'
            : 'Its pages, pictures, publications and conversations are removed for good. Backups of the installation keep a copy.'}
        </p>
        <div className="assistant-actions">
          <Button variant="danger" disabled={only} onClick={() => setDeleting(true)}>
            Delete {site.name}
          </Button>
        </div>
      </section>

      {deleting && <DeleteSiteDialog site={site} current={current} onClose={() => setDeleting(false)} />}
      {renaming && (
        <RenameSiteDialog
          site={site}
          onClose={() => setRenaming(false)}
          onRenamed={() => {
            setRenaming(false)
            onChanged()
          }}
        />
      )}
    </div>
  )
}

/** A new name for the site. Its short name, and so its addresses, stay. */
function RenameSiteDialog({
  site,
  onClose,
  onRenamed,
}: {
  site: SiteInfo
  onClose: () => void
  onRenamed: () => void
}) {
  const [name, setName] = useState(site.name)
  const [problem, setProblem] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const save = async () => {
    setBusy(true)
    try {
      await sites.rename(site.id, name)
      onRenamed()
    } catch (err) {
      setProblem(err instanceof ApiError ? err.message : String(err))
      setBusy(false)
    }
  }

  const changed = name.trim() !== '' && name.trim() !== site.name
  return (
    <Dialog
      title={`Rename ${site.name}`}
      subtitle={`The name shows in the panel. The short name, ${site.slug}, and the addresses stay.`}
      onClose={onClose}
    >
      <form
        className="site-form"
        onSubmit={(event) => {
          event.preventDefault()
          if (changed && !busy) void save()
        }}
      >
        <Field
          id="rename-site-name"
          label="Name"
          maxLength={80}
          autoFocus
          error={problem}
          value={name}
          onChange={(event) => {
            setName(event.target.value)
            setProblem(null)
          }}
        />
        <DialogActions>
          <Button variant="quiet" type="button" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" type="submit" disabled={busy || !changed}>
            Save the name
          </Button>
        </DialogActions>
      </form>
    </Dialog>
  )
}

/** Deleting asks for the site's short name, typed: it cannot be undone. */
function DeleteSiteDialog({
  site,
  current,
  onClose,
}: {
  site: SiteInfo
  current: SiteInfo
  onClose: () => void
}) {
  const [typed, setTyped] = useState('')
  const [problem, setProblem] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const remove = async () => {
    setBusy(true)
    try {
      await sites.remove(site.id)
      // The panel opens again on a site that still exists.
      const target = site.id === current.id ? '/' : `/s/${encodeURIComponent(current.slug)}/settings/sites`
      window.location.assign(target)
    } catch (err) {
      setProblem(err instanceof ApiError ? err.message : String(err))
      setBusy(false)
    }
  }

  return (
    <Dialog title={`Delete ${site.name}`} subtitle="This cannot be undone." onClose={onClose}>
      <Field
        id="delete-site-slug"
        label={`Type ${site.slug} to confirm`}
        autoCapitalize="none"
        spellCheck={false}
        error={problem}
        value={typed}
        onChange={(event) => {
          setTyped(event.target.value)
          setProblem(null)
        }}
      />
      <DialogActions>
        <Button variant="quiet" onClick={onClose}>
          Cancel
        </Button>
        <Button variant="danger" disabled={busy || typed !== site.slug} onClick={remove}>
          Delete the site
        </Button>
      </DialogActions>
    </Dialog>
  )
}

/** The free space below which Settings warns: a build and the runtimes
 * take several hundred megabytes. */
const LOW_SPACE = 2 * 1024 ** 3

/** What the whole installation lacks: the previews' DNS record, disk space. */
function InstallationWarnings({ installation }: { installation: Installation | null }) {
  if (!installation) return null
  const lowSpace = installation.free_bytes !== null && installation.free_bytes < LOW_SPACE
  if (installation.wildcard && !lowSpace) return null
  return (
    <div className="sites-warnings">
      {!installation.wildcard && (
        <p className="sites-warning" role="status">
          The previews’ names do not reach this installation yet. They need a DNS record{' '}
          <span className="domains-name">*.preview.{installation.panel}</span>{' '}
          {installation.addresses.length > 0
            ? `with the panel’s addresses: ${installation.addresses.join(', ')}.`
            : 'with the same addresses as the panel.'}
        </p>
      )}
      {lowSpace && (
        <p className="sites-warning" role="status">
          Little free space for the data: {gib(installation.free_bytes ?? 0)} left. Builds and the
          assistant may fail when the disk is full.
        </p>
      )}
    </div>
  )
}

function gib(bytes: number): string {
  return `${(bytes / 1024 ** 3).toFixed(1)} GiB`
}
