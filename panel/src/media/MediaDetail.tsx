import { useState } from 'react'
import { ApiError, media as mediaApi, type MediaItem } from '../api'
import { Button } from '../components/Button'
import { Dialog, DialogActions } from '../components/Dialog'
import { Field } from '../components/Field'
import { bytesText } from './size'
import './Media.css'

/** One picture: its description, name, where it is used, removal. */
export function MediaDetail({
  item,
  editable,
  onClose,
  onChanged,
}: {
  item: MediaItem
  editable: boolean
  onClose: () => void
  onChanged: () => void
}) {
  const [alt, setAlt] = useState(item.alt)
  const [name, setName] = useState(item.name)
  const [problem, setProblem] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const used = item.used_on ?? []
  const changed = alt !== item.alt || name !== item.name

  const act = async (work: () => Promise<unknown>, close: boolean) => {
    setBusy(true)
    setProblem(null)
    try {
      await work()
      onChanged()
      if (close) onClose()
    } catch (err) {
      setProblem(err instanceof ApiError ? err.message : String(err))
    }
    setBusy(false)
  }

  return (
    <Dialog title={item.name} onClose={onClose}>
      <div className="media-detail">
        <img src={mediaApi.fileUrl(item.id)} alt={item.alt} />
        <p className="media-used">
          {item.width && item.height ? `${item.width} × ${item.height} px · ` : ''}
          {bytesText(item.bytes)}
          {' · '}
          {used.length ? `used on ${used.join(', ')}` : 'not used on any page'}
        </p>
        <div className="field">
          <div className="field-head">
            <label htmlFor="media-alt">Description</label>
            <span className="field-note">{alt.length} / 500</span>
          </div>
          <textarea
            id="media-alt"
            rows={3}
            maxLength={500}
            placeholder="What the picture shows"
            value={alt}
            disabled={!editable}
            onChange={(event) => setAlt(event.target.value)}
          />
          <p className="media-used">
            Read aloud to people who can’t see the picture, and used by search engines. One or two
            plain sentences.
          </p>
        </div>
        <Field
          id="media-name"
          label="Name"
          value={name}
          disabled={!editable}
          onChange={(event) => setName(event.target.value)}
        />
        {problem && (
          <p className="media-problem" role="alert">
            {problem.charAt(0).toUpperCase() + problem.slice(1)}
          </p>
        )}
      </div>
      <DialogActions>
        <Button
          variant="danger"
          disabled={!editable || busy || used.length > 0}
          title={used.length ? 'Take it out of the pages using it first' : undefined}
          onClick={() => act(() => mediaApi.remove(item.id), true)}
        >
          Remove
        </Button>
        <Button
          variant="primary"
          disabled={!editable || busy || !changed}
          onClick={() => act(() => mediaApi.update(item.id, { alt, name }), true)}
        >
          Save
        </Button>
      </DialogActions>
    </Dialog>
  )
}
