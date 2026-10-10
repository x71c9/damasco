-- Requests of the assistant that wait for a person: permissions now,
-- questions next. SQLite cannot change a CHECK constraint, so the table is
-- rebuilt with the new kinds and states.

CREATE TABLE chat_item_new (
    id INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL REFERENCES conversation (id),
    kind TEXT NOT NULL CHECK (kind IN (
        'person', 'answer', 'thinking', 'activity', 'notice', 'error', 'permission', 'question'
    )),
    text TEXT NOT NULL,
    -- Second line of an action, such as the command it ran.
    detail TEXT,
    -- running, done, failed or stopped for an action; waiting, allowed,
    -- denied or answered for a request.
    state TEXT CHECK (state IN (
        'running', 'done', 'failed', 'stopped', 'waiting', 'allowed', 'denied', 'answered'
    )),
    -- The person who wrote a message, or who answered a request.
    author_id INTEGER REFERENCES person (id),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;

INSERT INTO chat_item_new (id, conversation_id, kind, text, detail, state, author_id, created_at)
    SELECT id, conversation_id, kind, text, detail, state, author_id, created_at FROM chat_item;

DROP TABLE chat_item;
ALTER TABLE chat_item_new RENAME TO chat_item;

CREATE INDEX chat_item_conversation ON chat_item (conversation_id, id);
