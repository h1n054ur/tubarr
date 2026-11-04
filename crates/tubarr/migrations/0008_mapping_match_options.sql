-- Matching options a mapping kind needs beyond the title rules.
-- Daily shows: the channel's offset from UTC in minutes, to turn an upload time into a broadcast
-- date. NULL means Pakistan (+05:00). A fixed offset, since the channels served keep no DST.
ALTER TABLE mappings ADD COLUMN utc_offset_mins INTEGER CHECK (utc_offset_mins BETWEEN -720 AND 840);
-- Shows numbered by their place in an official playlist: the playlist id (PL...), the season it
-- holds, and the episode number of its first video.
ALTER TABLE mappings ADD COLUMN playlist_id TEXT;
ALTER TABLE mappings ADD COLUMN playlist_season INTEGER NOT NULL DEFAULT 1 CHECK (playlist_season >= 0);
ALTER TABLE mappings ADD COLUMN playlist_offset INTEGER NOT NULL DEFAULT 1 CHECK (playlist_offset >= 0);
