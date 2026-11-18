import { useEffect, useState } from 'react'
import { fetchInstallation, updates, type UpdateStatus } from '../api'

/** How long the panel waits for the new version before giving up. */
const WAIT_MS = 10 * 60 * 1000

/**
 * The version of alogeno at the foot of the settings menu and, for those who
 * manage the installation, the update to a newer release. The update runs as
 * root outside the panel; the panel waits for the new version to answer and
 * reloads.
 */
export function Version({ version, manage }: { version: string; manage: boolean }) {
  const [status, setStatus] = useState<UpdateStatus | null>(null)
  const [updating, setUpdating] = useState(false)
  const [problem, setProblem] = useState<string | null>(null)

  useEffect(() => {
    if (!manage) return
    updates.status().then(
      (found) => {
        setStatus(found)
        if (found.requested) setUpdating(true)
      },
      () => setStatus(null),
    )
  }, [manage])

  useEffect(() => {
    if (!updating) return
    const started = Date.now()
    const timer = window.setInterval(() => {
      fetchInstallation().then(
        (installation) => {
          if (installation.version !== version) window.location.reload()
        },
        // While alogeno restarts the request fails: keep waiting.
        () => undefined,
      )
      if (Date.now() - started > WAIT_MS) {
        window.clearInterval(timer)
        setUpdating(false)
        setProblem('The update did not finish: see journalctl -u alogeno-update on the server.')
      }
    }, 3000)
    return () => window.clearInterval(timer)
  }, [updating, version])

  const update = () => {
    setProblem(null)
    updates.request().then(
      () => setUpdating(true),
      (error: unknown) => setProblem(error instanceof Error ? error.message : String(error)),
    )
  }

  return (
    <div className="settings-version">
      <p>alogeno {version}</p>
      {updating ? (
        <p>Updating…</p>
      ) : (
        status?.available &&
        (status.from_panel ? (
          <button className="settings-update" onClick={update}>
            Update to {status.available.replace(/^v/, '')}
          </button>
        ) : (
          <p>{status.available.replace(/^v/, '')} available: run sudo alogeno update</p>
        ))
      )}
      {problem && <p role="alert">{problem}</p>}
    </div>
  )
}
