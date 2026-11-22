import { useState } from 'react'
import { ApiError, media as mediaApi, type MediaItem } from '../api'
import { useNotices } from '../notices/context'
import { sentence } from '../text'

/** As damasco's `media::LIMIT`. */
const LARGEST = 20 * 1024 * 1024

/** Uploads pictures to the site, one after the other, and tells those that
 * did not make it. */
export function useMediaUpload(onUploaded: (items: MediaItem[]) => void) {
  // Pictures can be dropped again while some are on their way.
  const [batches, setBatches] = useState(0)
  const { notify } = useNotices()

  const upload = async (files: File[], folders = false) => {
    if (folders) {
      notify({
        kind: 'problem',
        title: 'Couldn’t upload a folder',
        text: 'Open it and drop the pictures inside.',
      })
    }
    if (files.length === 0) return
    setBatches((count) => count + 1)
    const added: MediaItem[] = []
    for (const file of files) {
      const failed = (text: string) =>
        notify({ kind: 'problem', title: `Couldn’t upload “${file.name}”`, text: sentence(text) })
      if (file.size > LARGEST) {
        failed('the picture is larger than 20 MB')
        continue
      }
      try {
        added.push(await mediaApi.upload(file))
      } catch (err) {
        failed(err instanceof ApiError ? err.message : String(err))
      }
    }
    setBatches((count) => count - 1)
    if (added.length) onUploaded(added)
  }

  return { busy: batches > 0, upload }
}
