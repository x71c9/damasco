-- What each turn of the assistant used, as its CLI reports it: the tokens,
-- and the price when the CLI computes one (Claude Code does).
CREATE TABLE turn_usage (
    id INTEGER PRIMARY KEY,
    conversation INTEGER NOT NULL REFERENCES conversation (id),
    provider TEXT NOT NULL CHECK (provider IN ('claude', 'codex')),
    -- Every token read, the cached ones included.
    input_tokens INTEGER NOT NULL,
    cached_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    -- US dollars at the API's prices.
    cost REAL,
    at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;

CREATE INDEX turn_usage_at ON turn_usage (provider, at);
