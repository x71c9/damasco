-- The files people attach to their messages in the chat. The files
-- themselves are outside the repository, in the site's
-- agent/attachments/<id>/<file>, which the assistant sees read-only.
CREATE TABLE attachment (
    id TEXT PRIMARY KEY,
    -- As the person's file is called.
    name TEXT NOT NULL,
    -- As it is called on disk: letters, digits, dots, dashes, underscores.
    file TEXT NOT NULL,
    mime TEXT NOT NULL,
    bytes INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;

-- The messages they are attached to. One never sent goes after a week.
CREATE TABLE chat_item_attachment (
    item_id INTEGER NOT NULL REFERENCES chat_item (id),
    attachment_id TEXT NOT NULL REFERENCES attachment (id),
    position INTEGER NOT NULL,
    PRIMARY KEY (item_id, attachment_id)
) STRICT;
