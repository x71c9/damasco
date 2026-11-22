import { useRef, useState } from 'react'
import { ApiError, chat, type ChatAttachment } from '../api'
import { sentence } from '../text'

/** As damasco's `attachments::LIMIT` and `attachments::PER_MESSAGE`. */
const LARGEST = 20 * 1024 * 1024
const MOST = 10

/** A file in the message being written: uploading until `saved` is there. */
export interface Draft {
  key: number
  name: string
  saved: ChatAttachment | null
}

/** The files of a paste; none when what was copied is text that brings its
 * own picture along, as a word processor or a spreadsheet does. */
export function pastedFiles(data: DataTransfer): File[] {
  const files = Array.from(data.files)
  if (files.length === 0) return []
  const text = data.getData('text/plain').trim()
  const formatted = data.types.includes('text/html') || data.types.includes('text/rtf')
  // A picture copied from a web page may bring its address as text.
  const address = /^\S+:\/\/\S+$/.test(text)
  return text && formatted && !address ? [] : files
}

/** The files of the message being written. Each is uploaded as it is
 * attached, so sending is immediate. */
export function useAttachments() {
  const [drafts, setDrafts] = useState<Draft[]>([])
  const [problem, setProblem] = useState<string | null>(null)
  const next = useRef(0)

  const add = (files: File[], folders = false) => {
    const problems: string[] = []
    if (folders) problems.push('Folders can’t be attached: drop the files inside.')
    const small = files.filter((file) => {
      if (file.size <= LARGEST) return true
      problems.push(`“${file.name}” is larger than 20 MB.`)
      return false
    })
    const room = Math.max(0, MOST - drafts.length)
    if (small.length > room) problems.push(`A message takes up to ${MOST} files.`)
    setProblem(problems.join(' ') || null)

    for (const file of small.slice(0, room)) {
      const key = next.current++
      setDrafts((all) => [...all, { key, name: file.name || 'file', saved: null }])
      chat.attach(file).then(
        (saved) => setDrafts((all) => all.map((draft) => (draft.key === key ? { ...draft, saved } : draft))),
        (err: unknown) => {
          setDrafts((all) => all.filter((draft) => draft.key !== key))
          const why = err instanceof ApiError ? ` ${sentence(err.message)}` : ''
          setProblem(`Couldn’t attach “${file.name}”.${why}`)
        },
      )
    }
  }

  /** Takes out `keys`: removed by the person, or sent. */
  const remove = (keys: number[]) => {
    setDrafts((all) => all.filter((draft) => !keys.includes(draft.key)))
    setProblem(null)
  }

  /** Puts back the files of a message sent before, in place of these. */
  const restore = (saved: ChatAttachment[]) => {
    setDrafts(saved.map((file) => ({ key: next.current++, name: file.name, saved: file })))
    setProblem(null)
  }

  return {
    drafts,
    problem,
    uploading: drafts.some((draft) => !draft.saved),
    add,
    remove,
    restore,
  }
}
