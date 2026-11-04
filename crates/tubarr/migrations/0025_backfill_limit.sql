-- How many back episodes a mapping releases: the newest N uploaded before the mapping was added.
-- Everything uploaded after it always is. NULL keeps every back episode, 0 releases none.
ALTER TABLE mappings ADD COLUMN backfill_limit INTEGER CHECK (backfill_limit >= 0);

-- A note state for the videos that limit leaves out: capped. SQLite cannot change a CHECK in
-- place, so the table is rebuilt with the same rows. Nothing references it.
CREATE TABLE unmatched_new (
    id INTEGER PRIMARY KEY,
    mapping_id INTEGER NOT NULL REFERENCES mappings (id) ON DELETE CASCADE,
    video_id TEXT NOT NULL,
    -- pending: waiting for metadata (duration, height), re-checked on the next run
    -- duplicate: another upload of the same episode won
    -- unsure: the title or the facts do not say for certain, a pin settles it
    -- partial: one part of a split episode, joined parts are not released yet
    -- capped: older than the back episodes the mapping's backfill limit keeps, never looked up
    state TEXT NOT NULL CHECK (state IN ('pending', 'duplicate', 'unsure', 'partial', 'capped')),
    -- the episode the title points at, when it names one
    season INTEGER,
    episode INTEGER,
    part INTEGER,
    reason TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    ignored INTEGER NOT NULL DEFAULT 0 CHECK (ignored IN (0, 1)),
    UNIQUE (mapping_id, video_id)
);
INSERT INTO unmatched_new (id, mapping_id, video_id, state, season, episode, part, reason,
    created_at, updated_at, ignored)
SELECT id, mapping_id, video_id, state, season, episode, part, reason, created_at, updated_at,
    ignored
FROM unmatched;
DROP TABLE unmatched;
ALTER TABLE unmatched_new RENAME TO unmatched;
CREATE INDEX unmatched_video ON unmatched (video_id);
