import { useEffect, useState } from 'react'
import { media as mediaApi, type MediaItem } from '../api'
import { Button } from '../components/Button'
import { Dialog } from '../components/Dialog'
import { MediaGrid } from './MediaGrid'
import './Media.css'

/** The request under way, shared by the choosers of a page. It is not kept:
 * pictures also come from Media and from the assistant. */
let loading: Promise<MediaItem[]> | null = null
function loadMedia(again = false): Promise<MediaItem[]> {
  if (loading && !again) return loading
  const request = mediaApi
    .list()
    .then((result) => result.media)
    .finally(() => {
      if (loading === request) loading = null
    })
  loading = request
  return request
}

type Reference = { media: string } | null

interface MediaChooserProps {
  value: Reference
  onChange: (value: Reference) => void
  editable: boolean
  /** What the picture is for, in the dialog's title. */
  purpose: string
}

/** A picture of a page: its thumbnail, and the way to choose, change or
 * remove it. */
export function MediaChooser({ value, onChange, editable, purpose }: MediaChooserProps) {
  const [items, setItems] = useState<MediaItem[] | null>(null)
  const [picking, setPicking] = useState(false)
  // Read again for another picture, which may be a new one, and whenever
  // the pictures are shown to choose from.
  const wanted = value?.media
  useEffect(() => {
    loadMedia().then(setItems, () => setItems((known) => known ?? []))
  }, [wanted, picking])
  const chosen = value ? items?.find((item) => item.id === value.media) : undefined

  return (
    <div className="media-choice">
      {value && chosen ? (
        <>
          <img src={mediaApi.fileUrl(chosen.id)} alt={chosen.alt} />
          <span className="media-choice-text">
            <span className="media-choice-name">{chosen.name}</span>
            <span className="media-choice-meta">
              {chosen.alt ? chosen.alt : 'No description yet: add one under Media.'}
            </span>
          </span>
        </>
      ) : value ? (
        <span className="media-choice-text">
          <span className="media-choice-meta">A picture that is no longer in the library.</span>
        </span>
      ) : (
        <span className="media-choice-text">
          <span className="media-choice-meta">No picture chosen.</span>
        </span>
      )}
      <span className="media-choice-actions">
        <Button small disabled={!editable} onClick={() => setPicking(true)}>
          {value ? 'Change' : 'Choose a picture'}
        </Button>
        {value && (
          <Button variant="quiet" small disabled={!editable} onClick={() => onChange(null)}>
            Remove
          </Button>
        )}
      </span>
      {picking && (
        <Dialog title={`Choose ${purpose}`} onClose={() => setPicking(false)}>
          <div className="media-picker">
            <MediaGrid
              items={items}
              chosen={value?.media ?? null}
              editable={editable}
              onPick={(item) => {
                onChange({ media: item.id })
                setPicking(false)
              }}
              onUploaded={(added) => {
                loadMedia(true).then(setItems, () => {})
                if (added.length === 1) {
                  onChange({ media: added[0].id })
                  setPicking(false)
                }
              }}
            />
          </div>
        </Dialog>
      )}
    </div>
  )
}
