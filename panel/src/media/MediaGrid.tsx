import { useRef } from 'react'
import { media as mediaApi, type MediaItem } from '../api'
import { PendingMark } from '../changes/changes'
import { useChanges } from '../changes/context'
import { Button } from '../components/Button'
import { bytesText } from './size'
import './Media.css'

interface MediaGridProps {
  items: MediaItem[] | null
  onPick: (item: MediaItem) => void
  /** Pictures chosen with the button. */
  onFiles: (files: File[]) => void
  /** Pictures are being uploaded. */
  busy: boolean
  /** Where they can be dropped, in words. */
  dropHint: string
  editable: boolean
  /** The picture chosen now, marked. */
  chosen?: string | null
}

/** Pictures as tiles, with the button to upload new ones. What holds the
 * grid takes the files dropped. */
export function MediaGrid({ items, onPick, onFiles, busy, dropHint, editable, chosen }: MediaGridProps) {
  const input = useRef<HTMLInputElement>(null)
  const { changes } = useChanges()
  const pendingOf = new Map((changes?.pending.media ?? []).map((item) => [item.id, item]))

  return (
    <div className="media-grid-wrap">
      <div className="media-upload">
        <Button icon="plus" disabled={!editable || busy} onClick={() => input.current?.click()}>
          {busy ? 'Uploading…' : 'Upload pictures'}
        </Button>
        <span>
          <span className="media-drop-hint">{dropHint} · </span>JPEG, PNG, WebP, GIF, AVIF, up to 20 MB
        </span>
        <input
          ref={input}
          type="file"
          accept="image/jpeg,image/png,image/webp,image/gif,image/avif"
          multiple
          hidden
          onChange={(event) => {
            if (event.target.files?.length) onFiles(Array.from(event.target.files))
            event.target.value = ''
          }}
        />
      </div>
      {items && items.length === 0 && <p className="media-empty">No pictures yet.</p>}
      <ul className="media-grid">
        {items?.map((item) => {
          const pending = pendingOf.get(item.id)
          return (
          <li key={item.id}>
            <button
              type="button"
              className={`media-tile${chosen === item.id ? ' media-chosen' : ''}`}
              onClick={() => onPick(item)}
              title={item.alt || item.name}
            >
              <img src={mediaApi.fileUrl(item.id)} alt="" loading="lazy" />
              <span className="media-tile-name">{item.name}</span>
              <span className="media-tile-meta">
                {item.width && item.height ? `${item.width} × ${item.height} · ` : ''}
                {bytesText(item.bytes)}
                {!item.alt && ' · no description'}
              </span>
              {pending && (
                <span className="media-tile-pending">
                  <PendingMark item={pending} compact />
                </span>
              )}
            </button>
          </li>
          )
        })}
      </ul>
    </div>
  )
}
