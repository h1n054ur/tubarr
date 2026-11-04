-- Videos the matcher looked at for a mapping but did not release, with the reason, for the UI.
-- One row per mapping and video. The matcher rewrites a mapping's rows on every run.
CREATE TABLE unmatched (
    id INTEGER PRIMARY KEY,
    mapping_id INTEGER NOT NULL REFERENCES mappings (id) ON DELETE CASCADE,
    video_id TEXT NOT NULL,
    -- pending: waiting for metadata (duration, height), re-checked on the next run
    -- duplicate: another upload of the same episode won
    -- unsure: the title or the facts do not say for certain, a pin settles it
    -- partial: one part of a split episode, joined parts are not released yet
    state TEXT NOT NULL CHECK (state IN ('pending', 'duplicate', 'unsure', 'partial')),
    -- the episode the title points at, when it names one
    season INTEGER,
    episode INTEGER,
    part INTEGER,
    reason TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (mapping_id, video_id)
);
