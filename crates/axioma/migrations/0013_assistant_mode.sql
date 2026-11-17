-- The mode last chosen for each assistant: new conversations start in it.
CREATE TABLE assistant_mode (
    provider TEXT PRIMARY KEY CHECK (provider IN ('claude', 'codex')),
    mode TEXT NOT NULL
) STRICT;
