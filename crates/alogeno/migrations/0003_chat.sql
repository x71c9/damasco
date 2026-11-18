-- Conversations with the assistant and what the chat shows of them.

CREATE TABLE conversation (
    id INTEGER PRIMARY KEY,
    -- Session id of the CLI, a UUID; the CLI keeps the conversation itself
    -- in the agent's home and resumes it with this id.
    session TEXT NOT NULL UNIQUE,
    -- The start of the first message.
    title TEXT NOT NULL,
    -- 1 once the CLI has created the session, so later turns resume it.
    started INTEGER NOT NULL DEFAULT 0 CHECK (started IN (0, 1)),
    created_by INTEGER REFERENCES person (id),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;

-- One line of the chat: a message of a person, an answer or the reasoning
-- of the assistant, one of its actions, a notice or an error.
CREATE TABLE chat_item (
    id INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL REFERENCES conversation (id),
    kind TEXT NOT NULL CHECK (kind IN ('person', 'answer', 'thinking', 'activity', 'notice', 'error')),
    text TEXT NOT NULL,
    -- Second line of an action, such as the command it ran.
    detail TEXT,
    -- State of an action: running, done, failed or stopped.
    state TEXT CHECK (state IN ('running', 'done', 'failed', 'stopped')),
    -- The person who wrote a message.
    author_id INTEGER REFERENCES person (id),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;

CREATE INDEX chat_item_conversation ON chat_item (conversation_id, id);
