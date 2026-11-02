-- The preview is private: the panel hands out a one-time code, which the
-- preview's host exchanges for a session of its own, kept in a cookie of
-- that host. Only the SHA-256 of codes and session tokens is stored.

CREATE TABLE preview_code (
    token_hash BLOB PRIMARY KEY,
    person_id INTEGER NOT NULL REFERENCES person (id),
    expires_at TEXT NOT NULL,
    used_at TEXT
) STRICT;

CREATE TABLE preview_session (
    token_hash BLOB PRIMARY KEY,
    person_id INTEGER NOT NULL REFERENCES person (id),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    expires_at TEXT NOT NULL
) STRICT;
