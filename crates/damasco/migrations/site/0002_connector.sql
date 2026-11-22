-- The connectors of the assistant's account (the MCP servers added in
-- claude.ai) a person allowed it to use, as allowed_host for hosts.
CREATE TABLE allowed_connector (
    name TEXT PRIMARY KEY,
    approved_by INTEGER,
    approved_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;
