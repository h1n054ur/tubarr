-- Official playlists as the watcher last listed them, for shows numbered by playlist place.
-- Each listing replaces the playlist's rows in full.
CREATE TABLE playlist_items (
    playlist_id TEXT NOT NULL,
    -- 1 for the first video, in the playlist's own order
    position INTEGER NOT NULL CHECK (position > 0),
    video_id TEXT NOT NULL,
    title TEXT NOT NULL,
    duration_secs INTEGER,
    listed_at INTEGER NOT NULL,
    PRIMARY KEY (playlist_id, position)
);

-- When each playlist was last listed. A row here with no items is a listed, empty playlist.
CREATE TABLE playlist_scans (
    playlist_id TEXT PRIMARY KEY,
    last_listing_at INTEGER NOT NULL
);

-- Playlists a draft asked for that no mapping names yet. The watcher lists each once, then drops
-- the request.
CREATE TABLE playlist_requests (
    playlist_id TEXT PRIMARY KEY,
    requested_at INTEGER NOT NULL
);
