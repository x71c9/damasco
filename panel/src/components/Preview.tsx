import type { Device } from './Header'
import { Icon } from './icons'
import './Preview.css'

const widths: Record<Device, string> = {
  computer: '100%',
  tablet: '768px',
  phone: '390px',
}

/** On a phone the header has no room for the preview's buttons: they sit
 * above the frame. */
export interface PreviewTools {
  onReload: () => void
  onOpenPreview: () => void
  /** Null until the site has a domain and a version online. */
  onOpenLive: (() => void) | null
}

interface PreviewProps {
  /** null: no address; undefined: not known yet. */
  url: string | null | undefined
  device: Device
  reloadKey: number
  /** The frame finished loading a page. */
  onLoad?: () => void
  /** What the frame shows, for screen readers. */
  label?: string
  tools?: PreviewTools
}

/** What the site in the frame may do: run its scripts, keep its own
 * origin (the session cookie of the preview), send its forms and open
 * links in new tabs, but never navigate the panel itself. Tabs it opens
 * keep these limits, so they cannot navigate the panel either. */
const sandbox = 'allow-scripts allow-same-origin allow-forms allow-popups'

/** The preview of the unpublished site, inside the petal frame. */
export function Preview({ url, device, reloadKey, onLoad, label = 'Preview of the site', tools }: PreviewProps) {
  return (
    <section className="preview" aria-label="Preview of the site">
      {tools && (
        <div className="preview-tools">
          <button className="icon-button" aria-label="Reload the preview" title="Reload" onClick={tools.onReload}>
            <Icon name="reload" size={17} />
          </button>
          <span className="preview-tools-spacer" />
          <div className="header-views header-open" role="group" aria-label="Open in a new tab">
            <button aria-label="Open the preview in a new tab" onClick={tools.onOpenPreview}>
              <Icon name="eye" size={15} />
              Preview
            </button>
            {tools.onOpenLive && (
              <button aria-label="Open the site online in a new tab" onClick={tools.onOpenLive}>
                <Icon name="website" size={15} />
                Live
              </button>
            )}
          </div>
        </div>
      )}
      <div className="preview-stage">
        <div className="preview-page" style={{ width: widths[device] }}>
          {url ? (
            <iframe
              key={reloadKey}
              className="preview-frame"
              src={url}
              title={label}
              sandbox={sandbox}
              onLoad={onLoad}
            />
          ) : url === null ? (
            <p className="preview-unavailable">The preview address is not available.</p>
          ) : null}
        </div>
      </div>
    </section>
  )
}
