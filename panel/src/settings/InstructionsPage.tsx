import { useEffect, useState } from 'react'
import { ApiError, instructions, type Installation, type Instructions, type SiteInfo } from '../api'
import { Button } from '../components/Button'
import { PageNotice } from '../notices/Notices'
import { useNotices } from '../notices/context'
import { sentence } from '../text'
import '../components/Page.css'
import './Assistant.css'
import './Instructions.css'

/** Settings › Instructions: what the assistant is always told, for every
 * site and for the one the panel works on. */
export function InstructionsPage({ site, installation }: { site: SiteInfo; installation: Installation | null }) {
  return (
    <section className="page-frame" aria-labelledby="instructions-title">
      <div className="page-card">
        <header className="page-head">
          <div>
            <h1 id="instructions-title">Instructions</h1>
            <p>
              What the assistant always follows, with no need to repeat it in the chat, like a CLAUDE.md or
              an AGENTS.md. Changes hold from the next message, also in conversations already started.
            </p>
          </div>
        </header>

        {installation && (
          <InstructionsBox
            scope="all"
            id="all"
            title="Every site"
            note="Language, tone, preferences and whatever else holds for all the sites of this installation."
            placeholder={'- Texts in Italian, informal tone\n- No stock photos\n- Short replies'}
            // With one site only its own are enough, unless some are there.
            onlyWithText={installation.single_site}
          />
        )}
        <InstructionsBox
          key={site.id}
          scope="site"
          id={site.id}
          title={installation?.single_site ? 'This site' : `Only ${site.name}`}
          note={
            installation?.single_site
              ? 'Language, tone, preferences and whatever else holds in every conversation.'
              : 'Added to those for every site; where they differ, these win. The menu next to the logo changes site.'
          }
          placeholder={'- Architecture studio, clients are private owners\n- Dark green and cream only\n- Every project has a place and a year'}
        />
      </div>
    </section>
  )
}

interface Draft {
  text: string
  /** The saved text it started from. */
  was: string
}

/** A draft outlives a change of section and a reload of the tab. */
function storedDraft(key: string): Draft | null {
  try {
    const draft = JSON.parse(sessionStorage.getItem(key) ?? 'null') as Draft | null
    return typeof draft?.text === 'string' && typeof draft.was === 'string' ? draft : null
  } catch {
    return null
  }
}

function storeDraft(key: string, draft: Draft | null) {
  try {
    if (draft) sessionStorage.setItem(key, JSON.stringify(draft))
    else sessionStorage.removeItem(key)
  } catch {
    // Not remembered: the draft lasts as long as the section is open.
  }
}

function InstructionsBox({
  scope,
  id,
  title,
  note,
  placeholder,
  onlyWithText = false,
}: {
  scope: 'all' | 'site'
  id: string
  title: string
  note: string
  placeholder: string
  onlyWithText?: boolean
}) {
  const key = `damasco.instructions.${id}`
  const { notify } = useNotices()
  const [saved, setSaved] = useState<Instructions | null>(null)
  const [draft, setDraft] = useState<Draft | null>(() => storedDraft(key))
  const [problem, setProblem] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)

  useEffect(() => {
    instructions.read(scope).then(setSaved, (err: unknown) =>
      setProblem(err instanceof Error ? sentence(err.message) : String(err)),
    )
  }, [scope])

  const keep = (next: Draft | null) => {
    setDraft(next)
    storeDraft(key, next)
  }

  if (onlyWithText && !saved?.text && !draft) return null

  const text = draft?.text ?? saved?.text ?? ''
  const dirty = saved !== null && draft !== null && draft.text !== saved.text
  // Saved elsewhere after this draft started.
  const moved = dirty && draft.was !== saved.text
  const length = [...text].length
  const tooLong = saved !== null && length > saved.limit

  const save = async () => {
    if (!dirty || saving || tooLong) return
    setSaving(true)
    setProblem(null)
    try {
      // After the warning, saving replaces the version it warned about.
      const next = await instructions.save(scope, draft.text, saved.text)
      setSaved(next)
      keep(null)
      notify({ kind: 'done', title: 'Instructions saved', text: 'The assistant follows them from the next message.' })
    } catch (err) {
      if (err instanceof ApiError && err.status === 409) {
        try {
          setSaved(await instructions.read(scope))
        } catch {
          setProblem(sentence(err.message))
        }
      } else {
        setProblem(err instanceof Error ? sentence(err.message) : String(err))
      }
    }
    setSaving(false)
  }

  return (
    <section className="assistant-box instructions-box" aria-labelledby={`instructions-${scope}-title`}>
      <h2 id={`instructions-${scope}-title`}>
        <label htmlFor={`instructions-${scope}`}>{title}</label>
      </h2>
      <p className="assistant-note">{note}</p>
      {moved && (
        <PageNotice
          notice={{
            kind: 'person',
            title: 'These were changed elsewhere',
            text: 'Saving replaces that version with the text below.',
            action: { label: 'Load that version', run: () => keep(null) },
          }}
        />
      )}
      {problem && <PageNotice notice={{ kind: 'problem', title: 'Something went wrong', text: problem }} />}
      <textarea
        id={`instructions-${scope}`}
        className="instructions-text"
        rows={7}
        placeholder={placeholder}
        value={text}
        disabled={saved === null || saving}
        aria-invalid={tooLong || undefined}
        onChange={(event) => keep({ text: event.target.value, was: draft?.was ?? saved?.text ?? '' })}
        onKeyDown={(event) => {
          if ((event.ctrlKey || event.metaKey) && event.key === 's') {
            event.preventDefault()
            void save()
          }
        }}
      />
      <div className="instructions-foot">
        <span className="instructions-count" data-invalid={tooLong || undefined}>
          {dirty ? 'Not saved yet · ' : ''}
          {length.toLocaleString()}
          {saved && ` / ${saved.limit.toLocaleString()}`}
        </span>
        {dirty && (
          <Button variant="quiet" small disabled={saving} onClick={() => keep(null)}>
            Discard
          </Button>
        )}
        <Button variant="primary" small disabled={!dirty || saving || tooLong} onClick={save}>
          {saving ? 'Saving…' : 'Save'}
        </Button>
      </div>
    </section>
  )
}
