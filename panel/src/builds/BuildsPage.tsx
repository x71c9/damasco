import { useCallback, useEffect, useState } from 'react'
import { builds, type BuildEntry, type BuildHistory } from '../api'
import { when } from '../changes/context'
import { Button } from '../components/Button'
import { Dialog, DialogActions } from '../components/Dialog'
import { Working } from '../components/icons'
import { goTo } from '../navigation'
import { PageNotice } from '../notices/Notices'
import { useNotices } from '../notices/context'
import { lockedNotice } from '../notices/locked'
import { sentence } from '../text'
import '../components/Page.css'
import './Builds.css'

const PAGE = 50

const full = new Intl.DateTimeFormat(undefined, { dateStyle: 'full', timeStyle: 'short' })

const origins: Record<BuildEntry['origin'], string | null> = {
  assistant: 'Assistant',
  panel: 'Panel',
  other: null,
}

/** The builds of the site, newest first, and the way back to each, with no
 * need of the assistant. `version` changes as builds start and end. */
export function BuildsPage({ site, version }: { site: string; version: string }) {
  const { notify, openChat } = useNotices()
  const [limit, setLimit] = useState(PAGE)
  const [history, setHistory] = useState<BuildHistory | null>(null)
  const [problem, setProblem] = useState<string | null>(null)
  const [asking, setAsking] = useState<BuildEntry | null>(null)
  const [busy, setBusy] = useState(false)

  const load = useCallback(() => {
    builds.list(limit).then(
      (found) => {
        setHistory(found)
        setProblem(null)
      },
      (err: unknown) => setProblem(err instanceof Error ? sentence(err.message) : String(err)),
    )
  }, [limit])
  // Again as builds end, and for whether the assistant is at work.
  useEffect(() => {
    load()
    const timer = window.setInterval(load, 4000)
    return () => window.clearInterval(timer)
  }, [load, version])

  const restore = async (build: BuildEntry) => {
    setAsking(null)
    setBusy(true)
    try {
      await builds.restore(build.id)
      notify({
        kind: 'done',
        title: 'The site went back to that build',
        text: 'The preview follows in a moment. What is online changes with Publish.',
        action: { label: 'See the preview', run: () => goTo('/') },
      })
    } catch (err) {
      notify({
        kind: 'problem',
        title: 'Couldn’t go back to that build',
        text: err instanceof Error ? sentence(err.message) : String(err),
      })
    }
    setBusy(false)
    load()
  }

  const locked = history?.locked ?? null

  return (
    <section className="page-frame" aria-labelledby="builds-title">
      <div className="page-card">
        <header className="page-head">
          <div>
            <h1 id="builds-title">Builds</h1>
            <p>
              Every build of {site}, newest first. The site can go back to any of them; what is online
              changes only with Publish.
            </p>
          </div>
        </header>

        {locked && <PageNotice notice={lockedNotice(locked, openChat)} />}
        {problem && <PageNotice notice={{ kind: 'problem', title: 'Couldn’t read the builds', text: problem }} />}
        {history?.state === 'failed' && (
          <PageNotice
            notice={{
              kind: 'problem',
              title: 'The last build failed',
              text: history.error ? sentence(history.error) : undefined,
            }}
          />
        )}

        {history?.state === 'running' && (
          <p className="builds-running" role="status">
            <Working size={16} label="Building" />
            Building…
          </p>
        )}
        {!history && !problem && <p className="builds-note">Reading the builds…</p>}
        {history?.builds.length === 0 && history.state !== 'running' && (
          <p className="builds-note">No builds yet. The first one follows the first change of the site.</p>
        )}

        {history && history.builds.length > 0 && (
          <ol className="builds" aria-label="Builds">
            {history.builds.map((build) => {
              const at = new Date(build.at * 1000)
              const origin = origins[build.origin]
              return (
                <li key={build.id} className="builds-row" data-failed={build.failed || undefined}>
                  <span className="builds-text">
                    <span className="builds-title" title={build.title}>
                      {build.title}
                    </span>
                    <span className="builds-meta">
                      <time dateTime={at.toISOString()} title={full.format(at)}>
                        {when(build.at)}
                      </time>
                      {origin && <span>{origin}</span>}
                      {build.failed && <span className="builds-failed">Failed</span>}
                      {build.in_preview && <span className="builds-tag">In the preview</span>}
                      {build.online && <span className="builds-tag builds-online">Online</span>}
                    </span>
                  </span>
                  {build.current ? (
                    <span className="builds-current">The site now</span>
                  ) : (
                    <Button
                      small
                      disabled={busy || Boolean(locked)}
                      aria-label={`Go back to “${build.title}”, ${when(build.at)}`}
                      onClick={() => setAsking(build)}
                    >
                      Go back to this
                    </Button>
                  )}
                </li>
              )
            })}
          </ol>
        )}
        {history?.more && (
          <Button className="builds-more" onClick={() => setLimit((count) => count + PAGE)}>
            Show older builds
          </Button>
        )}
      </div>

      {asking && (
        <Dialog title="Go back to this build?" subtitle={when(asking.at)} onClose={() => setAsking(null)}>
          <p className="builds-asked">{asking.title}</p>
          <p className="dialog-text">
            Pages, media, design, code and the assistant’s notes about the site return to how they were then.
            It becomes a new build, so the later ones stay here to return to. What is online does not change
            until it is published.
          </p>
          {asking.failed && (
            <p className="builds-warning">This build failed: the preview cannot show it until it is fixed.</p>
          )}
          <DialogActions>
            <Button onClick={() => setAsking(null)} autoFocus>
              Cancel
            </Button>
            <Button variant="primary" onClick={() => restore(asking)}>
              Go back
            </Button>
          </DialogActions>
        </Dialog>
      )}
    </section>
  )
}
