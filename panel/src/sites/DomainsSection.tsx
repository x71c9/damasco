import { useCallback, useEffect, useState, type SubmitEvent } from 'react'
import {
  ApiError,
  domains as domainsApi,
  siblingUrl,
  type DnsRecord,
  type DomainCheck,
  type Installation,
  type SiteDomain,
  type SiteInfo,
} from '../api'
import { Button } from '../components/Button'
import { Field } from '../components/Field'
import { BlockedMark, Icon, Mark, WaitingMark, Working } from '../components/icons'
import './Domains.css'

/** What a check found, said with its next step. */
const CHECKS: Record<DomainCheck, string> = {
  active: 'Active: HTTPS reaches this installation.',
  no_certificate:
    'Reaches this installation, but the certificate is not there yet. Check again in a minute; if it lasts, look at the log of the reverse proxy.',
  other_server:
    'Another server answers: the DNS points elsewhere, or a proxy or a CDN is active on the domain.',
  unreachable: 'Not reachable yet: create the records below at the registrar of the domain.',
}

type Checked = { result: DomainCheck; addresses: string[] } | 'checking' | { problem: string }

/** Settings › Sites › a site: the domains that publish it. A domain added
 * here is served at once; the records and the check tell what is missing. */
export function DomainsSection({
  site,
  installation,
  onChanged,
}: {
  site: SiteInfo
  installation: Installation | null
  /** The site's domains changed: its addresses in the list follow. */
  onChanged: () => void
}) {
  const [list, setList] = useState<SiteDomain[] | null>(null)
  const [problem, setProblem] = useState<string | null>(null)
  const [checks, setChecks] = useState<Record<string, Checked>>({})

  const check = useCallback(
    (host: string) => {
      setChecks((current) => ({ ...current, [host]: 'checking' }))
      domainsApi.check(site.id, host).then(
        (found) => setChecks((current) => ({ ...current, [host]: found })),
        (err: unknown) =>
          setChecks((current) => ({
            ...current,
            [host]: { problem: err instanceof Error ? err.message : String(err) },
          })),
      )
    },
    [site.id],
  )

  const load = useCallback(
    (checkAll: boolean) =>
      domainsApi.list(site.id).then(
        (found) => {
          setList(found.domains)
          setProblem(null)
          if (checkAll) found.domains.forEach((domain) => check(domain.host))
        },
        (err: unknown) => setProblem(err instanceof Error ? err.message : String(err)),
      ),
    [site.id, check],
  )

  // Opening the section checks every domain once; another site gets a
  // section of its own (`key`).
  useEffect(() => {
    void load(true)
  }, [load])

  const changed = (checkHosts: string[]) => {
    onChanged()
    void load(false).then(() => checkHosts.forEach(check))
  }

  const main = (list ?? []).filter((domain) => !domain.redirect_to)
  const wwwOf = (host: string) => (list ?? []).find((domain) => domain.redirect_to === host)
  const local = installation?.mode === 'local'

  return (
    <section className="assistant-box" aria-labelledby="site-domains-title">
      <h2 id="site-domains-title">Domains</h2>
      <p className="assistant-note">
        {local
          ? `In local development a domain is served as soon as it points to this computer, for example in /etc/hosts; ${site.name} is also published at its local address.`
          : 'A domain is published as soon as it is added. Its DNS records go at the registrar of the domain; then the check below turns active.'}
      </p>
      {installation?.mode === 'manual' && (
        <ManualProxy site={site} list={list ?? []} installation={installation} />
      )}

      {problem && (
        <p className="assistant-note domains-problem" role="alert">
          {problem}
        </p>
      )}
      {list === null && !problem ? (
        <p className="assistant-note">Loading…</p>
      ) : (
        main.length > 0 && (
          <ul className="domains-list">
            {main.map((domain) => (
              <DomainRow
                key={domain.host}
                site={site}
                domain={domain}
                www={wwwOf(domain.host)}
                checks={checks}
                onCheck={check}
                onRemoved={() => changed([])}
              />
            ))}
          </ul>
        )
      )}

      <AddDomain site={site} onAdded={(hosts) => changed(hosts)} />
    </section>
  )
}

/** One domain, its www, their records, checks and removal. */
function DomainRow({
  site,
  domain,
  www,
  checks,
  onCheck,
  onRemoved,
}: {
  site: SiteInfo
  domain: SiteDomain
  www: SiteDomain | undefined
  checks: Record<string, Checked>
  onCheck: (host: string) => void
  onRemoved: () => void
}) {
  const [busy, setBusy] = useState(false)
  const [problem, setProblem] = useState<string | null>(null)

  const remove = async () => {
    setBusy(true)
    setProblem(null)
    try {
      await domainsApi.remove(site.id, domain.host)
      onRemoved()
    } catch (err) {
      setProblem(err instanceof ApiError ? err.message : String(err))
      setBusy(false)
    }
  }

  const records = [...domain.records, ...(www?.records ?? [])]
  return (
    <li className="domains-row">
      <div className="domains-head">
        <a className="domains-host" href={siblingUrl(domain.host)} target="_blank" rel="noreferrer">
          {domain.host}
        </a>
        <div className="assistant-actions domains-actions">
          <Button onClick={() => [domain, www].forEach((d) => d && onCheck(d.host))}>
            Check again
          </Button>
          <Button variant="danger" disabled={busy} onClick={remove}>
            Remove
          </Button>
        </div>
      </div>
      <CheckLine host={domain.host} checked={checks[domain.host]} />
      {www && (
        <>
          <p className="assistant-note">
            <span className="domains-name">{www.host}</span> sends its visitors to {domain.host}.
          </p>
          <CheckLine host={www.host} checked={checks[www.host]} />
        </>
      )}
      <Records records={records} host={domain.host} />
      {problem && (
        <p className="assistant-note domains-problem" role="alert">
          {problem}
        </p>
      )}
    </li>
  )
}

function CheckLine({ host, checked }: { host: string; checked: Checked | undefined }) {
  if (!checked) return null
  if (checked === 'checking') {
    return (
      <p className="domains-check-line">
        <Working size={16} label={`Checking ${host}`} />
        <span>Checking {host}…</span>
      </p>
    )
  }
  if ('problem' in checked) {
    return (
      <p className="domains-check-line">
        <BlockedMark size={16} label="Check failed" />
        <span>The check of {host} did not run: {checked.problem}</span>
      </p>
    )
  }
  const mark =
    checked.result === 'active' ? (
      <Mark size={16} label="Active" />
    ) : checked.result === 'no_certificate' ? (
      <WaitingMark size={16} label="Waiting for the certificate" />
    ) : (
      <BlockedMark size={16} label="Not reaching this installation" />
    )
  const points =
    checked.result !== 'active' && checked.addresses.length > 0
      ? ` ${host} points to ${checked.addresses.join(', ')}.`
      : ''
  return (
    <p className={`domains-check-line domains-${checked.result}`} role="status">
      {mark}
      <span>
        {checked.result === 'active' ? <strong>{host}</strong> : host}: {CHECKS[checked.result]}
        {points}
      </span>
    </p>
  )
}

/** The records to create, each value with a button that copies it. */
function Records({ records, host }: { records: DnsRecord[]; host: string }) {
  // The addresses come from the panel's own name; without them, only the
  // www's CNAME is known.
  const unknown = !records.some((record) => record.name === host)
  return (
    <>
      {unknown && (
        <p className="assistant-note">
          The panel’s name does not resolve from this server, so its addresses cannot be shown
          here: {host} needs the same A and AAAA records as the panel.
        </p>
      )}
      {records.length > 0 && <RecordTable records={records} />}
    </>
  )
}

function RecordTable({ records }: { records: DnsRecord[] }) {
  return (
    <table className="domains-records">
      <caption className="visually-hidden">DNS records to create</caption>
      <thead>
        <tr>
          <th scope="col">Type</th>
          <th scope="col">Name</th>
          <th scope="col">Value</th>
        </tr>
      </thead>
      <tbody>
        {records.map((record) => (
          <tr key={`${record.kind} ${record.name} ${record.value}`}>
            <td>{record.kind}</td>
            <td className="domains-name">{record.name}</td>
            <td>
              <span className="domains-value">
                <span className="domains-name">{record.value}</span>
                <CopyButton text={record.value} />
              </span>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <button
      type="button"
      className="domains-copy"
      aria-label={copied ? 'Copied' : `Copy ${text}`}
      title={copied ? 'Copied' : 'Copy'}
      onClick={() => {
        navigator.clipboard.writeText(text).then(
          () => {
            setCopied(true)
            window.setTimeout(() => setCopied(false), 1500)
          },
          () => setCopied(false),
        )
      }}
    >
      <Icon name={copied ? 'circleCheck' : 'copy'} size={15} />
    </button>
  )
}

/** Adds a domain, with its www unless unticked. */
function AddDomain({ site, onAdded }: { site: SiteInfo; onAdded: (hosts: string[]) => void }) {
  const [host, setHost] = useState('')
  const [www, setWww] = useState(true)
  const [problem, setProblem] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const plain = host.trim().toLowerCase().replace(/^https?:\/\//, '').replace(/[/?#].*$/, '').replace(/\.$/, '')

  const submit = async (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!plain) {
      setProblem('Enter a domain, such as example.com')
      return
    }
    setBusy(true)
    try {
      const added = await domainsApi.add(site.id, host, www)
      setHost('')
      setProblem(null)
      onAdded(added.map((domain) => domain.host))
    } catch (err) {
      setProblem(err instanceof ApiError ? err.message : String(err))
    } finally {
      setBusy(false)
    }
  }

  return (
    <form className="domains-add" onSubmit={submit} noValidate>
      <Field
        id={`domain-${site.id}`}
        label="Add a domain"
        placeholder="example.com"
        autoCapitalize="none"
        spellCheck={false}
        inputMode="url"
        error={problem}
        value={host}
        onChange={(event) => {
          setHost(event.target.value)
          setProblem(null)
        }}
      />
      {!plain.startsWith('www.') && (
        <label className="domains-www">
          <input type="checkbox" checked={www} onChange={(event) => setWww(event.target.checked)} />
          Also {plain ? `www.${plain}` : 'its www'}, sending its visitors to it
        </label>
      )}
      <div className="assistant-actions">
        <Button type="submit" variant="primary" disabled={busy}>
          Add the domain
        </Button>
      </div>
    </form>
  )
}

/** A manual installation: the reverse proxy learns each name by hand. */
function ManualProxy({
  site,
  list,
  installation,
}: {
  site: SiteInfo
  list: SiteDomain[]
  installation: Installation
}) {
  const names = [site.preview_host, ...list.map((domain) => domain.host)]
  return (
    <div className="domains-manual">
      <p className="assistant-note">
        This installation does not tell the reverse proxy about new names: each one goes into its
        configuration by hand, forwarded to <span className="domains-name">{installation.listen}</span>{' '}
        with its Host header, as for the panel. For {site.name}:
      </p>
      <ul className="domains-names">
        {names.map((name) => (
          <li key={name} className="domains-name">
            {name}
          </li>
        ))}
      </ul>
    </div>
  )
}
