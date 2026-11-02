import { useCallback, useEffect, useState } from 'react'
import { media as mediaApi, type MediaItem } from '../api'
import { madeBy, useChanges } from '../changes/context'
import { Icon } from '../components/icons'
import { MediaDetail } from './MediaDetail'
import { MediaGrid } from './MediaGrid'
import '../components/Page.css'
import '../pages/Pages.css'

/** The pictures of the site: upload, describe, remove. */
export function MediaPage() {
  const [items, setItems] = useState<MediaItem[] | null>(null)
  const [locked, setLocked] = useState<string | null>(null)
  const [open, setOpen] = useState<MediaItem | null>(null)
  const { changes, refresh } = useChanges()
  const deleted = changes?.pending.media.filter((item) => item.status === 'deleted') ?? []

  const load = useCallback(() => {
    mediaApi.list().then(
      (result) => {
        setItems(result.media)
        setLocked(result.locked)
      },
      () => {},
    )
  }, [])
  const changed = () => {
    load()
    refresh()
  }
  useEffect(() => {
    load()
    const timer = setInterval(load, 5000)
    return () => clearInterval(timer)
  }, [load])

  return (
    <section className="page-frame" aria-labelledby="media-title">
      <div className="page-card">
        <header className="page-head">
          <div>
            <h1 id="media-title">Media</h1>
            <p>The pictures of the site. Pages show them; the assistant can place them too.</p>
          </div>
        </header>
        {locked && (
          <p className="pages-locked" role="status">
            <Icon name="history" size={16} />
            Pictures are read-only while {locked}. They open again when it finishes.
          </p>
        )}
        <MediaGrid items={items} onPick={setOpen} onUploaded={changed} editable={!locked} />
        {deleted.length > 0 && (
          <section className="pages-deleted" aria-labelledby="media-deleted-title">
            <h2 id="media-deleted-title">Removed, still online</h2>
            <p>These pictures stay on the site until the next publication.</p>
            <ul>
              {deleted.map((item) => (
                <li key={item.id}>
                  <span className="pages-deleted-title">{item.title || 'Unnamed picture'}</span>
                  <span className="pages-deleted-when">Removed {madeBy(item, changes?.people)}</span>
                </li>
              ))}
            </ul>
          </section>
        )}
      </div>
      {open && (
        <MediaDetail
          item={items?.find((item) => item.id === open.id) ?? open}
          editable={!locked}
          onClose={() => setOpen(null)}
          onChanged={changed}
        />
      )}
    </section>
  )
}
