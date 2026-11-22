import { useEffect, useRef, useState, useSyncExternalStore } from 'react'
import { addZone, onZoneChange, zoneOver, type Zone } from './files'

/** Makes the element given `zoneRef` a place to drop files on. `accepts`
 * false refuses them, and the veil over it says why. */
export function useFileDrop<T extends Element>(accepts: boolean, take: Zone['take']) {
  const [element, zoneRef] = useState<T | null>(null)
  const latest = useRef({ accepts, take })
  useEffect(() => {
    latest.current = { accepts, take }
  })
  useEffect(() => {
    if (!element) return
    return addZone(element, {
      accepts: () => latest.current.accepts,
      take: (files, folders) => latest.current.take(files, folders),
    })
  }, [element])
  const over = useSyncExternalStore(onZoneChange, () => element !== null && zoneOver() === element)
  return { zoneRef, over }
}
