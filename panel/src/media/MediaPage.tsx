import { useCallback, useEffect, useState } from 'react'
import { media as mediaApi, type MediaItem } from '../api'
import { madeAt, useChanges } from '../changes/context'
import { PageNotice } from '../notices/Notices'
import { useNotices } from '../notices/context'
import { lockedNotice } from '../notices/locked'
import { dropParams, param } from '../navigation'
import { MediaDetail } from './MediaDetail'
import { MediaGrid } from './MediaGrid'
import { useMediaUpload } from './useMediaUpload'
import { DropVeil } from '../drop/DropVeil'
import { useFileDrop } from '../drop/useFileDrop'
import { sentence } from '../text'
import './Media.css'
import '../components/Page.css'
import '../pages/Pages.css'

/** The pictures of the site: upload, describe, remove. */
export function MediaPage() {
  const [items, setItems] = useState<MediaItem[] | null>(null)
  const [locked, setLocked] = useState<string | null>(null)
  // The picture open in its dialog. The address can name one
  // (`?picture=…`), from a button of the assistant.
  const [openId, setOpenId] = useState<string | null>(() => param('picture'))
  const open = items?.find((item) => item.id === openId) ?? null
  useEffect(() => {
    dropParams()
    const onPop = () => {
      const named = param('picture')
      if (named) {
        setOpenId(named)
        dropParams()
      }
    }
    window.addEventListener('popstate', onPop)
    return () => window.removeEventListener('popstate', onPop)
  }, [])
  const { changes, refresh } = useChanges()
  const { openChat } = useNotices()
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

  const { busy, upload } = useMediaUpload(changed)
  const { zoneRef, over } = useFileDrop<HTMLElement>(!locked, upload)

  return (
    <section className="page-frame media-drop" aria-labelledby="media-title" ref={zoneRef}>
      {over && (
        <DropVeil
          refused={Boolean(locked)}
          title={locked ? 'Pictures can’t be added now' : 'Drop to upload'}
          hint={locked ? `${sentence(locked)}.` : 'JPEG, PNG, WebP, GIF or AVIF, up to 20 MB each'}
        />
      )}
      <div className="page-card">
        <header className="page-head">
          <div>
            <h1 id="media-title">Media</h1>
            <p>The pictures of the site. Pages show them; the assistant can place them too.</p>
          </div>
        </header>
        {locked && <PageNotice notice={lockedNotice(locked, openChat)} />}
        <MediaGrid
          items={items}
          onPick={(item) => setOpenId(item.id)}
          onFiles={upload}
          busy={busy}
          dropHint="or drop them anywhere on this page"
          editable={!locked}
        />
        {deleted.length > 0 && (
          <section className="pages-deleted" aria-labelledby="media-deleted-title">
            <h2 id="media-deleted-title">Removed, still online</h2>
            <p>These pictures stay on the site until the next publication.</p>
            <ul>
              {deleted.map((item) => (
                <li key={item.id}>
                  <span className="pages-deleted-title">{item.title || 'Unnamed picture'}</span>
                  <span className="pages-deleted-when">Removed {madeAt(item)}</span>
                </li>
              ))}
            </ul>
          </section>
        )}
      </div>
      {open && (
        <MediaDetail
          item={open}
          editable={!locked}
          onClose={() => setOpenId(null)}
          onChanged={changed}
        />
      )}
    </section>
  )
}
