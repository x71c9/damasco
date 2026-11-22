import { chat, type ChatAttachment } from '../api'
import { bytesText } from '../media/size'
import { Icon } from './icons'
import type { Draft } from './useAttachments'
import './Attachments.css'

/** The types the server shows as pictures; any other file is downloaded. */
const PICTURES = ['image/jpeg', 'image/png', 'image/webp', 'image/gif', 'image/avif']

function isPicture(file: ChatAttachment): boolean {
  return PICTURES.includes(file.mime)
}

/** The files of the message being written, each with the way to take it
 * out. */
export function AttachmentDrafts({
  drafts,
  onRemove,
}: {
  drafts: Draft[]
  onRemove: (key: number) => void
}) {
  if (drafts.length === 0) return null
  return (
    <ul className="attachments-draft" aria-label="Files attached">
      {drafts.map(({ key, name, saved }) => {
        const picture = saved && isPicture(saved)
        return (
          <li key={key} className={picture ? 'attachment-picture' : 'attachment-file'} title={name}>
            {picture ? (
              <img src={chat.attachmentUrl(saved.id)} alt={name} />
            ) : (
              <>
                <Icon name="pages" size={17} />
                <span className="attachment-text">
                  <span className="attachment-name">{name}</span>
                  <span className="attachment-size">{saved ? bytesText(saved.bytes) : 'Uploading…'}</span>
                </span>
              </>
            )}
            <button
              type="button"
              className="attachment-remove"
              aria-label={`Remove ${name}`}
              title="Remove"
              onClick={() => onRemove(key)}
            >
              <Icon name="close" size={12} strokeWidth={2} />
            </button>
          </li>
        )
      })}
    </ul>
  )
}

/** The files of a message sent: pictures open larger, others download. */
export function SentAttachments({ files }: { files: ChatAttachment[] }) {
  return (
    <ul className="attachments-sent" aria-label="Files of the message">
      {files.map((file) =>
        isPicture(file) ? (
          <li key={file.id} className="attachment-picture">
            <a href={chat.attachmentUrl(file.id)} target="_blank" rel="noreferrer" title={file.name}>
              <img src={chat.attachmentUrl(file.id)} alt={file.name} />
            </a>
          </li>
        ) : (
          <li key={file.id} className="attachment-file">
            <a href={chat.attachmentUrl(file.id)} download={file.name} title={file.name}>
              <Icon name="pages" size={17} />
              <span className="attachment-text">
                <span className="attachment-name">{file.name}</span>
                <span className="attachment-size">{bytesText(file.bytes)}</span>
              </span>
            </a>
          </li>
        ),
      )}
    </ul>
  )
}
