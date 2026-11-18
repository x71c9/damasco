-- One row describing this installation.
CREATE TABLE installation (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    created_at TEXT NOT NULL
) STRICT;

INSERT INTO installation (id, created_at)
VALUES (1, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'));
