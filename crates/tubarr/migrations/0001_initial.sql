-- Times are Unix seconds (UTC). Lists are JSON arrays of strings.

-- One row per Sonarr series or Radarr film that Tubarr serves from YouTube.
CREATE TABLE mappings (
    id INTEGER PRIMARY KEY,
    -- show: numbered episodes, daily: matched by air date, film: one video
    kind TEXT NOT NULL CHECK (kind IN ('show', 'daily', 'film')),
    tvdb_id INTEGER UNIQUE,
    tmdb_id INTEGER UNIQUE,
    -- the title as Sonarr or Radarr has it, used in release names
    title TEXT NOT NULL,
    -- English language name for the Newznab language attribute (Urdu, Hindi)
    language TEXT NOT NULL,
    -- official YouTube channel ids (UC...) allowed as sources
    channel_ids TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(channel_ids)),
    -- regex with a named group `episode`; NULL means the default numbering rule
    episode_regex TEXT,
    -- title words that rule a video out (Teaser, Promo, Highlights)
    excludes TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(excludes)),
    min_duration_secs INTEGER CHECK (min_duration_secs > 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK ((kind = 'film' AND tmdb_id IS NOT NULL) OR (kind <> 'film' AND tvdb_id IS NOT NULL))
);

-- A video Hani picked by hand. Wins over the matcher for that episode or film.
-- Exactly one target shape: season + episode (show), air_date (daily), or none (film).
CREATE TABLE pins (
    id INTEGER PRIMARY KEY,
    mapping_id INTEGER NOT NULL REFERENCES mappings (id) ON DELETE CASCADE,
    season INTEGER,
    episode INTEGER,
    air_date TEXT,
    video_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    CHECK (
        (season IS NOT NULL AND episode IS NOT NULL AND air_date IS NULL)
        OR (season IS NULL AND episode IS NULL AND air_date IS NOT NULL)
        OR (season IS NULL AND episode IS NULL AND air_date IS NULL)
    )
);
-- NULLs never collide in a plain UNIQUE, so the target is folded into non-NULL values.
CREATE UNIQUE INDEX pins_target ON pins (mapping_id, coalesce(season, -1), coalesce(episode, -1), coalesce(air_date, ''));

-- Stream lookups (yt-dlp format info) kept until their addresses expire.
CREATE TABLE resolve_cache (
    video_id TEXT PRIMARY KEY,
    formats TEXT NOT NULL CHECK (json_valid(formats)),
    expires_at INTEGER NOT NULL
);

-- Download jobs handed out through the SABnzbd API. nzo_id is the job id Sonarr and Radarr track.
CREATE TABLE grabs (
    nzo_id TEXT PRIMARY KEY,
    release_name TEXT NOT NULL,
    video_id TEXT NOT NULL,
    -- SABnzbd category, tv or movies
    category TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('completed', 'failed')),
    -- folder holding the .strm and .srt, as the arr sees it
    storage_path TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX grabs_created ON grabs (created_at);

-- Event log for the Activity page: searches, grabs, plays, errors.
CREATE TABLE activity (
    id INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL,
    kind TEXT NOT NULL,
    video_id TEXT,
    message TEXT NOT NULL
);
CREATE INDEX activity_created ON activity (created_at);
