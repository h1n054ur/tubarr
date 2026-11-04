-- Shows and daily shows: next to each imported episode's .strm, Tubarr keeps a second one named
-- "<same name> - Audio only.strm" that plays the episode's audio alone, which Jellyfin lists as
-- another version of the episode. Off (0) by default.
ALTER TABLE mappings ADD COLUMN audio_version INTEGER NOT NULL DEFAULT 0 CHECK (audio_version IN (0, 1));
