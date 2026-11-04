-- Channel search backfill per show mapping: finds back episodes a channel's newest uploads no
-- longer reach. `key` is the title, channels and aliases searched for: when it changes the
-- backfill starts over. `done` is set once the matcher has released every wanted episode.
CREATE TABLE mapping_backfill (
    mapping_id INTEGER PRIMARY KEY REFERENCES mappings (id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    last_run_at INTEGER,
    last_error TEXT,
    -- videos the last run found
    last_found INTEGER,
    done INTEGER NOT NULL DEFAULT 0 CHECK (done IN (0, 1))
);
