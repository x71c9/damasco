import { Button } from '../components/Button'
import { Dialog, DialogActions } from '../components/Dialog'
import { goTo } from '../navigation'
import './Publish.css'

interface NoDomainDialogProps {
  /** The slug of the site, to open its page in Settings › Sites. */
  slug: string
  onClose: () => void
}

/** Publish on a site without a domain: a publication would reach no
 * address, so the dialog says where to add one instead of listing the
 * changes. */
export function NoDomainDialog({ slug, onClose }: NoDomainDialogProps) {
  const open = () => {
    onClose()
    goTo(`/settings/sites/${encodeURIComponent(slug)}`)
  }
  return (
    <Dialog title="The site needs a domain" className="publish-dialog" onClose={onClose}>
      <p className="dialog-text">
        A publication goes online on the site’s domain, and this site has none yet. Add one in
        Settings › Sites: the page lists the DNS records to create at the registrar of the domain.
      </p>
      <p className="dialog-text">The changes wait in the preview until then.</p>
      <DialogActions>
        <Button onClick={onClose}>Close</Button>
        <Button variant="primary" onClick={open} autoFocus>
          Add a domain
        </Button>
      </DialogActions>
    </Dialog>
  )
}
