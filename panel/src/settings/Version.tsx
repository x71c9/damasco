import { useEffect, useState } from 'react'
import { fetchInstallation, updates, type UpdateStatus } from '../api'
import { BlockedMark, Mark, WaitingMark, Working } from '../components/icons'

/** How long the panel waits for the new version before giving up. */
const WAIT_MS = 10 * 60 * 1000

/** A version as the panel writes it: v0.1.2. */
const shown = (version: string) => `v${version.replace(/^v/, '')}`

/**
 * The version of damasco at the foot of the settings menu. Its mark is the
 * state of the installation, as the petals are everywhere in the panel: four
 * full petals when nothing is pending, a dot in the centre when a newer
 * release waits, petals lighting up in turn while it installs, outline only
 * when the update did not finish. The owner updates
 * it from here; the update runs as root outside the panel, and the panel
 * reloads when the new version answers.
 */
export function Version({ version }: { version: string }) {
  const [status, setStatus] = useState<UpdateStatus | null>(null)
  const [updating, setUpdating] = useState(false)
  const [problem, setProblem] = useState<string | null>(null)

  useEffect(() => {
    updates.status().then(
      (found) => {
        setStatus(found)
        if (found.requested) setUpdating(true)
      },
      () => setStatus(null),
    )
  }, [])

  useEffect(() => {
    if (!updating) return
    const started = Date.now()
    const timer = window.setInterval(() => {
      fetchInstallation().then(
        (installation) => {
          if (installation.version !== version) window.location.reload()
        },
        // While damasco restarts the request fails: keep waiting.
        () => undefined,
      )
      if (Date.now() - started > WAIT_MS) {
        window.clearInterval(timer)
        setUpdating(false)
        setProblem('The update did not finish. Its log on the server: journalctl -u damasco-update')
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

  const next = status?.available ? shown(status.available) : null
  const number = <span className="version-number">{shown(version)}</span>

  let mark = <Mark size={20} label="Up to date" />
  let line = number
  if (updating && next) {
    mark = <Working size={20} label="Updating" />
    line = (
      <>
        Updating to <span className="version-number">{next}</span>…
      </>
    )
  } else if (problem) {
    mark = <BlockedMark size={20} label="Update not finished" />
  } else if (next) {
    mark = <WaitingMark size={20} label="A newer version is available" />
    // The button names the new version: the line keeps the current one.
  } else if (status) {
    line = <>{number}, the latest</>
  }

  return (
    // The mark sits last, at the height of the account avatar beside it in
    // the rail; what the state adds grows upwards.
    <div className="version">
      {problem && (
        <p className="version-problem" role="alert">
          {problem}
        </p>
      )}
      {next && !updating && status?.from_panel && (
        <button className="button button-accent version-update" onClick={update}>
          {problem ? 'Try again' : `Update to ${next}`}
        </button>
      )}
      {next && !updating && status && !status.from_panel && (
        <p className="version-hint">
          {status.nix ? (
            <>
              {next} available, from the system flake:
              <code className="version-command">nix flake update damasco</code>
            </>
          ) : (
            <>
              {next} available, to install on the server:
              <code className="version-command">sudo damasco update</code>
            </>
          )}
        </p>
      )}
      <div className="version-head">
        {mark}
        <p className="version-text">
          <span className="version-name">damasco</span>
          <span className="version-line" aria-live="polite">
            {line}
          </span>
        </p>
      </div>
    </div>
  )
}
