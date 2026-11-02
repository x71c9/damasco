-- Publications: which release went online, built from which version of the
-- site, by whom, and which release it replaced. Kept to say when the site
-- was last published and to go back to the release before.

CREATE TABLE publication (
    id INTEGER PRIMARY KEY,
    -- The release that went online, in data_dir/releases.
    release TEXT NOT NULL,
    -- The saved version of the site it was built from.
    commit_id TEXT NOT NULL,
    -- The release that was online before, if any.
    previous_release TEXT,
    person_id INTEGER REFERENCES person (id),
    -- How many changes went online, as the panel counts them.
    changes INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;
