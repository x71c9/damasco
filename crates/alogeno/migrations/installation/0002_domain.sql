-- The domains of the sites. A domain with redirect_to sends its visitors to
-- that domain of the same site: the www added together with a domain.
CREATE TABLE domain (
    host TEXT PRIMARY KEY,
    site_id TEXT NOT NULL REFERENCES site (id) ON DELETE CASCADE,
    redirect_to TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;

CREATE INDEX domain_site ON domain (site_id);
