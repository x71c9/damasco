import type { Device } from './Header'
import './Preview.css'

const widths: Record<Device, string> = {
  computer: '100%',
  tablet: '768px',
  phone: '390px',
}

interface PreviewProps {
  /** null: no address; undefined: not known yet. */
  url: string | null | undefined
  device: Device
  reloadKey: number
}

/** The preview of the unpublished site, inside the petal frame. */
export function Preview({ url, device, reloadKey }: PreviewProps) {
  return (
    <section className="preview" aria-label="Preview of the site">
      <div className="preview-stage">
        <div className="preview-page" style={{ width: widths[device] }}>
          {url ? (
            <iframe key={reloadKey} className="preview-frame" src={url} title="Preview of the site" />
          ) : url === null ? (
            <p className="preview-unavailable">The preview address is not available.</p>
          ) : null}
        </div>
      </div>
    </section>
  )
}
