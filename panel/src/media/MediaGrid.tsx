import { useRef, useState, type DragEvent } from 'react'
import { ApiError, media as mediaApi, type MediaItem } from '../api'
import { Button } from '../components/Button'
import { bytesText } from './size'
import './Media.css'

interface MediaGridProps {
  items: MediaItem[] | null
  onPick: (item: MediaItem) => void
  /** After an upload: the new pictures. */
  onUploaded: (items: MediaItem[]) => void
  editable: boolean
  /** The picture chosen now, marked. */
  chosen?: string | null
}

/** Pictures as tiles, with the way to upload new ones: the button or files
 * dropped on the grid. */
export function MediaGrid({ items, onPick, onUploaded, editable, chosen }: MediaGridProps) {
  const input = useRef<HTMLInputElement>(null)
  const [busy, setBusy] = useState(false)
  const [problem, setProblem] = useState<string | null>(null)
  const [over, setOver] = useState(false)

  const upload = async (files: FileList | File[]) => {
    setBusy(true)
    setProblem(null)
    const added: MediaItem[] = []
    for (const file of Array.from(files)) {
      try {
        added.push(await mediaApi.upload(file))
      } catch (err) {
        const message = err instanceof ApiError ? err.message : String(err)
        setProblem(`${file.name}: ${message}`)
      }
    }
    setBusy(false)
    if (added.length) onUploaded(added)
  }

  const onDrop = (event: DragEvent<HTMLDivElement>) => {
    event.preventDefault()
    setOver(false)
    if (editable && event.dataTransfer.files.length) upload(event.dataTransfer.files)
  }

  return (
    <div
      className={`media-grid-wrap${over ? ' media-over' : ''}`}
      onDragOver={(event) => {
        event.preventDefault()
        if (editable) setOver(true)
      }}
      onDragLeave={() => setOver(false)}
      onDrop={onDrop}
    >
      <div className="media-upload">
        <Button icon="plus" disabled={!editable || busy} onClick={() => input.current?.click()}>
          {busy ? 'Uploading…' : 'Upload pictures'}
        </Button>
        <span>or drop them here · JPEG, PNG, WebP, GIF, AVIF, up to 20 MB</span>
        <input
          ref={input}
          type="file"
          accept="image/jpeg,image/png,image/webp,image/gif,image/avif"
          multiple
          hidden
          onChange={(event) => {
            if (event.target.files?.length) upload(event.target.files)
            event.target.value = ''
          }}
        />
      </div>
      {problem && (
        <p className="media-problem" role="alert">
          {problem}
        </p>
      )}
      {items && items.length === 0 && <p className="media-empty">No pictures yet.</p>}
      <ul className="media-grid">
        {items?.map((item) => (
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
            </button>
          </li>
        ))}
      </ul>
    </div>
  )
}
