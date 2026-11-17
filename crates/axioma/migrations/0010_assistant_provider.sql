-- More than one assistant: the one in use, and the one of each
-- conversation.

-- The assistant the chat uses: claude or codex. Changing it starts a new
-- conversation.
ALTER TABLE installation ADD COLUMN assistant TEXT NOT NULL DEFAULT 'claude'
    CHECK (assistant IN ('claude', 'codex'));

-- The assistant of the conversation; older ones are Claude's.
ALTER TABLE conversation ADD COLUMN provider TEXT NOT NULL DEFAULT 'claude'
    CHECK (provider IN ('claude', 'codex'));

-- The CLI's own id of the conversation, when it chooses it (Codex's
-- thread). Claude takes axioma's `session` as its id.
ALTER TABLE conversation ADD COLUMN cli_session TEXT;
