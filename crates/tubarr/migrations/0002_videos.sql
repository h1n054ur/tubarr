-- Video metadata from yt-dlp, kept for good. Stream addresses expire, so they stay in
-- resolve_cache, which now holds only the stream half of a lookup.
CREATE TABLE videos (
    video_id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    duration_secs INTEGER,
    -- YYYYMMDD as yt-dlp gives it
    upload_date TEXT,
    channel_id TEXT,
    -- subtitle tracks without their addresses: [{"lang", "automatic", "ext"}]
    subtitles TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(subtitles)),
    updated_at INTEGER NOT NULL
);

-- Rows written before the split hold the old shape. They expire within hours anyway.
DELETE FROM resolve_cache;
