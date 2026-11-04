-- Shows and daily shows: next to each imported episode's .strm, Tubarr saves the YouTube video's
-- thumbnail as "<same name>-thumb.jpg", which Jellyfin shows as the episode image. For podcasts,
-- whose episodes rarely have stills on TheTVDB. Off (0) by default, on for the shows already
-- matched by the number in their titles, which only the podcast preset turns on.
ALTER TABLE mappings ADD COLUMN episode_thumbnails INTEGER NOT NULL DEFAULT 0 CHECK (episode_thumbnails IN (0, 1));
UPDATE mappings SET episode_thumbnails = 1 WHERE title_number = 1 AND kind = 'show';
