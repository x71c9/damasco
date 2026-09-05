-- People with access to the panel, their one-time links and sessions, and
-- the activity log.

CREATE TABLE person (
    id INTEGER PRIMARY KEY,
    -- Chosen by the person when redeeming the setup link.
    login TEXT UNIQUE COLLATE NOCASE,
    name TEXT,
    role TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'editor')),
    -- Argon2id PHC string; NULL until the setup link is redeemed.
    password_hash TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    revoked_at TEXT
) STRICT;

-- At most one owner with access.
CREATE UNIQUE INDEX person_one_owner ON person (role)
    WHERE role = 'owner' AND revoked_at IS NULL;

-- One-time links to set a password (setup) or replace it (reset). Only the
-- SHA-256 of the token is stored.
CREATE TABLE link (
    token_hash BLOB PRIMARY KEY,
    person_id INTEGER NOT NULL REFERENCES person (id),
    purpose TEXT NOT NULL CHECK (purpose IN ('setup', 'reset')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    expires_at TEXT NOT NULL,
    used_at TEXT
) STRICT;

-- Panel sessions. Only the SHA-256 of the cookie value is stored.
CREATE TABLE session (
    token_hash BLOB PRIMARY KEY,
    person_id INTEGER NOT NULL REFERENCES person (id),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    last_seen_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    expires_at TEXT NOT NULL
) STRICT;

CREATE INDEX session_person ON session (person_id);

-- Activity log: append-only, kept without expiry. It records identifiers,
-- never personal values such as names or passwords.
CREATE TABLE activity (
    id INTEGER PRIMARY KEY,
    at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    -- NULL when the action comes from the installation itself (command line).
    actor_id INTEGER REFERENCES person (id),
    environment TEXT NOT NULL CHECK (environment IN ('installation', 'preview', 'production')),
    action TEXT NOT NULL,
    subject TEXT
) STRICT;

CREATE TRIGGER activity_no_update BEFORE UPDATE ON activity
BEGIN
    SELECT RAISE(ABORT, 'the activity log is append-only');
END;

CREATE TRIGGER activity_no_delete BEFORE DELETE ON activity
BEGIN
    SELECT RAISE(ABORT, 'the activity log is append-only');
END;
