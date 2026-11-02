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
  /** The frame finished loading a page. */
  onLoad?: () => void
  /** What the frame shows, for screen readers. */
  label?: string
}

/** The preview of the unpublished site, inside the petal frame. */
export function Preview({ url, device, reloadKey, onLoad, label = 'Preview of the site' }: PreviewProps) {
  return (
    <section className="preview" aria-label="Preview of the site">
      <div className="preview-stage">
        <div className="preview-page" style={{ width: widths[device] }}>
          {url ? (
            <iframe key={reloadKey} className="preview-frame" src={url} title={label} onLoad={onLoad} />
          ) : url === null ? (
            <p className="preview-unavailable">The preview address is not available.</p>
          ) : null}
        </div>
      </div>
    </section>
  )
}
