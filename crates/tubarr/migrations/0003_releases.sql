-- Videos the matcher accepted for an episode or film. The Newznab API answers from this table.
-- Exactly one target shape, as in pins: season + episode (show), air_date (daily), or none (film).
CREATE TABLE releases (
    id INTEGER PRIMARY KEY,
    mapping_id INTEGER NOT NULL REFERENCES mappings (id) ON DELETE CASCADE,
    video_id TEXT NOT NULL,
    season INTEGER,
    episode INTEGER,
    -- YYYY-MM-DD, for daily shows
    air_date TEXT,
    -- the film's year as Radarr has it, written into film release names
    year INTEGER,
    -- IMDb id digits (no tt), for films when known
    imdb_id INTEGER,
    -- the YouTube title, shown in the UI only: release names come from the mapping's title
    video_title TEXT NOT NULL,
    -- English language name (Urdu, Hindi)
    language TEXT NOT NULL,
    -- video height of the best format (1080, 720, 480), the resolution in the release name
    height INTEGER NOT NULL CHECK (height > 0),
    -- bytes of video plus audio when yt-dlp reported them, never a guess
    size INTEGER CHECK (size > 0),
    -- YouTube upload time
    published_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    CHECK (
        (season IS NOT NULL AND episode IS NOT NULL AND air_date IS NULL)
        OR (season IS NULL AND episode IS NULL AND air_date IS NOT NULL)
        OR (season IS NULL AND episode IS NULL AND air_date IS NULL)
    )
);
CREATE UNIQUE INDEX releases_target ON releases (mapping_id, video_id, coalesce(season, -1), coalesce(episode, -1), coalesce(air_date, ''));
CREATE INDEX releases_video ON releases (video_id);
CREATE INDEX releases_published ON releases (published_at);
