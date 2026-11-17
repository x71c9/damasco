import { Suspense, lazy } from 'react'
import { Button } from '../components/Button'
import { Field } from '../components/Field'
import { Icon } from '../components/icons'
import { MediaChooser } from '../media/MediaChooser'
import type { PMNode } from './richtext'

const TextEditor = lazy(() => import('./TextEditor'))

export interface Block {
  id: string
  type: 'text' | 'media'
  body?: PMNode
  media?: unknown
  caption?: string | null
}

function newId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(4))
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')
}

interface BlocksEditorProps {
  blocks: Block[]
  onChange: (blocks: Block[]) => void
  editable: boolean
  /** The problem the server found in a field, such as `blocks[2].body`. */
  problem: (field: string) => string | null
}

/** The text and media blocks of a page or a component: each can move up
 * and down or go, and new ones come at the end. */
export function BlocksEditor({ blocks, onChange, editable, problem }: BlocksEditorProps) {
  const changeBlock = (index: number, changes: Partial<Block>) =>
    onChange(blocks.map((block, i) => (i === index ? { ...block, ...changes } : block)))
  const moveBlock = (index: number, by: number) => {
    const next = [...blocks]
    const [block] = next.splice(index, 1)
    next.splice(index + by, 0, block)
    onChange(next)
  }

  return (
    <>
      {blocks.length === 0 && <p className="editor-note">No content yet: add a block.</p>}
      <ol className="editor-blocks">
        {blocks.map((block, index) => {
          const blockProblem =
            problem(`blocks[${index}]`) ?? problem(`blocks[${index}].body`) ?? problem(`blocks[${index}].caption`)
          return (
            <li
              key={block.id}
              id={`block-${block.id}`}
              className={`editor-block${blockProblem ? ' editor-block-invalid' : ''}`}
            >
              <div className="editor-block-head">
                <span>{block.type === 'text' ? 'Text' : 'Media'}</span>
                {blockProblem && <span className="field-error">{blockProblem}</span>}
                <span className="editor-block-actions">
                  <button
                    type="button"
                    className="icon-button"
                    aria-label="Move up"
                    title="Move up"
                    disabled={!editable || index === 0}
                    onClick={() => moveBlock(index, -1)}
                  >
                    <Icon name="chevronUp" size={16} strokeWidth={2} />
                  </button>
                  <button
                    type="button"
                    className="icon-button"
                    aria-label="Move down"
                    title="Move down"
                    disabled={!editable || index === blocks.length - 1}
                    onClick={() => moveBlock(index, 1)}
                  >
                    <Icon name="chevronDown" size={16} strokeWidth={2} />
                  </button>
                  <button
                    type="button"
                    className="icon-button"
                    aria-label="Remove the block"
                    title="Remove the block"
                    disabled={!editable}
                    onClick={() => onChange(blocks.filter((_, i) => i !== index))}
                  >
                    <Icon name="close" size={16} strokeWidth={2} />
                  </button>
                </span>
              </div>
              {block.type === 'text' ? (
                <Suspense fallback={<p className="editor-note">Loading the editor…</p>}>
                  <TextEditor
                    label={`Text block ${index + 1}`}
                    value={block.body}
                    editable={editable}
                    onChange={(body) => changeBlock(index, { body })}
                  />
                </Suspense>
              ) : (
                <div className="editor-media">
                  <MediaChooser
                    purpose="a picture for this block"
                    value={(block.media as { media: string } | null) ?? null}
                    editable={editable}
                    onChange={(media) => changeBlock(index, { media })}
                  />
                  <Field
                    id={`caption-${block.id}`}
                    label="Caption"
                    value={block.caption ?? ''}
                    disabled={!editable}
                    onChange={(event) => changeBlock(index, { caption: event.target.value || null })}
                  />
                </div>
              )}
            </li>
          )
        })}
      </ol>
      <div className="editor-add">
        <span>Add a block</span>
        <Button
          small
          icon="plus"
          disabled={!editable}
          onClick={() => onChange([...blocks, { id: newId(), type: 'text', body: { type: 'doc', content: [] } }])}
        >
          Text
        </Button>
        <Button
          small
          icon="plus"
          disabled={!editable}
          onClick={() => onChange([...blocks, { id: newId(), type: 'media', media: null, caption: null }])}
        >
          Media
        </Button>
      </div>
    </>
  )
}
