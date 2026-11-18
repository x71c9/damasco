-- Links of the assistant into the panel ("go to the field"): a chat line of
-- kind 'link', whose detail holds where it leads.

CREATE TABLE chat_item_new (
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
    -- The person who wrote a message, or who answered a request.
    author_id INTEGER REFERENCES person (id),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    -- The version of the site the line refers to (see 0006).
    version TEXT
) STRICT;

INSERT INTO chat_item_new (id, conversation_id, kind, text, detail, state, author_id, created_at, version)
    SELECT id, conversation_id, kind, text, detail, state, author_id, created_at, version FROM chat_item;

DROP TABLE chat_item;
ALTER TABLE chat_item_new RENAME TO chat_item;

CREATE INDEX chat_item_conversation ON chat_item (conversation_id, id);
