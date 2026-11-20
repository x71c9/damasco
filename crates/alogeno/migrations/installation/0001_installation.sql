-- The installation database, installation.db: the owner, the sessions, the
-- sites and their domains, the assistant in use, what it consumed, and the
-- activity log of the whole installation. Each site keeps the rest in its
-- own database (migrations/site).

CREATE TABLE installation (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    created_at TEXT NOT NULL,
    -- The assistant in use: one sign-in serves every site.
    assistant TEXT NOT NULL DEFAULT 'claude' CHECK (assistant IN ('claude', 'codex')),
    -- Random, created with the installation; /_alogeno/health names it, so
    -- that a check of a domain can tell this installation from any other
    -- server.
    instance_id TEXT NOT NULL
) STRICT;

INSERT INTO installation (id, created_at, instance_id)
VALUES (1, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), lower(hex(randomblob(16))));

-- The owner, the only account. The row exists from the first owner link;
-- the password arrives when the link is redeemed.
CREATE TABLE person (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    -- Argon2id PHC string; NULL until the setup link is redeemed.
    password_hash TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;

CREATE TABLE link (
    token_hash BLOB PRIMARY KEY,
    person_id INTEGER NOT NULL REFERENCES person (id),
    purpose TEXT NOT NULL CHECK (purpose IN ('setup', 'reset')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    expires_at TEXT NOT NULL,
    used_at TEXT
) STRICT;

CREATE TABLE session (
    token_hash BLOB PRIMARY KEY,
    person_id INTEGER NOT NULL REFERENCES person (id),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    last_seen_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    expires_at TEXT NOT NULL
) STRICT;

-- The sites, each in data_dir/sites/<id> with its own database.
CREATE TABLE site (
    -- ULID, never changes; names the site's folder.
    id TEXT PRIMARY KEY,
    -- Names the preview (<slug>.preview.<panel>) and the panel's addresses;
    -- never changes either.
    slug TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;

-- The activity log of the installation and of its sites. A site's entries
-- name it; they stay after the site is deleted, as the log never changes.
CREATE TABLE activity (
    id INTEGER PRIMARY KEY,
    at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    -- The site the entry is about, NULL for the installation itself.
    site_id TEXT,
    -- The owner (1), or NULL when the action comes from alogeno itself.
    actor_id INTEGER,
    environment TEXT NOT NULL CHECK (environment IN ('installation', 'preview', 'production')),
    action TEXT NOT NULL,
    subject TEXT
) STRICT;

CREATE INDEX activity_site ON activity (site_id, id);

CREATE TRIGGER activity_no_update BEFORE UPDATE ON activity
BEGIN
    SELECT RAISE(ABORT, 'the activity log is append-only');
END;

CREATE TRIGGER activity_no_delete BEFORE DELETE ON activity
BEGIN
    SELECT RAISE(ABORT, 'the activity log is append-only');
END;

-- The mode last chosen for each assistant: new conversations start in it.
CREATE TABLE assistant_mode (
    provider TEXT PRIMARY KEY CHECK (provider IN ('claude', 'codex')),
    mode TEXT NOT NULL
) STRICT;

-- What each turn consumed, for every site: the subscription is one.
CREATE TABLE turn_usage (
    id INTEGER PRIMARY KEY,
    site_id TEXT NOT NULL,
    -- The conversation in the site's database.
    conversation INTEGER NOT NULL,
    provider TEXT NOT NULL CHECK (provider IN ('claude', 'codex')),
    -- Every token read, the cached ones included.
    input_tokens INTEGER NOT NULL,
    cached_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    -- US dollars at the API's prices.
    cost REAL,
    at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;

CREATE INDEX turn_usage_at ON turn_usage (provider, at);
