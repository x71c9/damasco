-- Hosts the assistant may reach besides the built-in list and the
-- configuration, approved by a person in the chat.

CREATE TABLE allowed_host (
    host TEXT PRIMARY KEY,
    approved_by INTEGER REFERENCES person (id),
    approved_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;
