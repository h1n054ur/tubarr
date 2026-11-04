-- Episode files whose audio only version Jellyfin should merge into, or split from, the episode.
-- Kept so a restart picks them up again. A row goes once its job is done or given up on.
CREATE TABLE jellyfin_merge_queue (
    -- The episode's main .strm as Jellyfin sees it.
    path TEXT PRIMARY KEY NOT NULL,
    action TEXT NOT NULL CHECK (action IN ('merge', 'split')),
    -- Unix milliseconds. A job queued again for the same path gets a new one.
    queued_at_ms INTEGER NOT NULL,
    -- Unix milliseconds of the next try.
    next_at_ms INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT
);
