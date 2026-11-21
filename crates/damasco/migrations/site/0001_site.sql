-- The database of one site, data_dir/sites/<id>/site.db: its conversations
-- with the assistant, the hosts the assistant may reach for it, its
-- publications and the sessions of its preview. Columns naming who did
-- something hold the owner's id (1), without a foreign key: the owner lives
-- in installation.db.

CREATE TABLE conversation (
    id INTEGER PRIMARY KEY,
    -- Session id of the CLI, a UUID; the CLI keeps the conversation itself
    -- in the site's folder of the agent's home and resumes it with this id.
    session TEXT NOT NULL UNIQUE,
    -- The start of the first message.
    title TEXT NOT NULL,
    -- 1 once the CLI has created the session, so later turns resume it.
    started INTEGER NOT NULL DEFAULT 0 CHECK (started IN (0, 1)),
    created_by INTEGER,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    provider TEXT NOT NULL DEFAULT 'claude' CHECK (provider IN ('claude', 'codex')),
    -- The CLI's own id of the session, when it differs from `session`.
    cli_session TEXT,
    mode TEXT,
    model TEXT
) STRICT;

CREATE TABLE chat_item (
    id INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL REFERENCES conversation (id),
    kind TEXT NOT NULL CHECK (kind IN (
        'person', 'answer', 'thinking', 'activity', 'notice', 'error', 'permission', 'question', 'link'
    )),
    text TEXT NOT NULL,
    -- Second line of an action, such as the command it ran; for a link,
    -- where it leads, as JSON.
    detail TEXT,
    -- running, done, failed or stopped for an action; waiting, allowed,
    -- denied or answered for a request.
    state TEXT CHECK (state IN (
        'running', 'done', 'failed', 'stopped', 'waiting', 'allowed', 'denied', 'answered'
    )),
    -- Who wrote a message, or answered a request.
    author_id INTEGER,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    -- The version of the site the line refers to.
    version TEXT
) STRICT;

CREATE INDEX chat_item_conversation ON chat_item (conversation_id, id);

CREATE TABLE allowed_host (
    host TEXT PRIMARY KEY,
    approved_by INTEGER,
    approved_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;

CREATE TABLE publication (
    id INTEGER PRIMARY KEY,
    -- The release that went online, in the site's releases.
    release TEXT NOT NULL,
    -- The saved version of the site it was built from.
    commit_id TEXT NOT NULL,
    -- The release that was online before, if any.
    previous_release TEXT,
    person_id INTEGER,
    -- How many changes went online, as the panel counts them.
    changes INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;

CREATE TABLE preview_code (
    token_hash BLOB PRIMARY KEY,
    person_id INTEGER NOT NULL,
    expires_at TEXT NOT NULL,
    used_at TEXT
) STRICT;

CREATE TABLE preview_session (
    token_hash BLOB PRIMARY KEY,
    person_id INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    expires_at TEXT NOT NULL
) STRICT;
