-- Videos the watcher found on mapped channels, waiting for the matcher. Metadata from a full
-- lookup lives in `videos`; this table only holds what the feed or channel list showed.
CREATE TABLE channel_videos (
    video_id TEXT PRIMARY KEY,
    channel_id TEXT NOT NULL,
    title TEXT NOT NULL,
    -- Unix seconds. Exact from RSS, a rough guess ("3 days ago") from a channel list.
    published_at INTEGER,
    published_approx INTEGER NOT NULL DEFAULT 0 CHECK (published_approx IN (0, 1)),
    duration_secs INTEGER,
    -- where the video was seen first
    source TEXT NOT NULL CHECK (source IN ('rss', 'listing')),
    first_seen INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    -- new: not looked at yet, matched: became a release, ignored: fits no mapping
    match_state TEXT NOT NULL DEFAULT 'new' CHECK (match_state IN ('new', 'matched', 'ignored')),
    match_updated_at INTEGER
);
CREATE INDEX channel_videos_state ON channel_videos (match_state, first_seen);
CREATE INDEX channel_videos_channel ON channel_videos (channel_id, published_at);

-- When each channel's full list was last read, so a restart does not list every channel again.
CREATE TABLE channel_scans (
    channel_id TEXT PRIMARY KEY,
    last_listing_at INTEGER NOT NULL
);
