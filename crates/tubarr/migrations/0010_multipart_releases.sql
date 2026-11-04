-- Episodes and films YouTube split into parts are released as one: video_id holds the first part
-- and part_ids every part in play order as a JSON array. NULL for a single upload.
ALTER TABLE releases ADD COLUMN part_ids TEXT CHECK (part_ids IS NULL OR json_valid(part_ids));

-- The same first part may head a single release and a joined one on the same target.
DROP INDEX releases_target;
CREATE UNIQUE INDEX releases_target ON releases (mapping_id, video_id, coalesce(part_ids, ''), coalesce(season, -1), coalesce(episode, -1), coalesce(air_date, ''));

-- Bytes of video plus audio of the formats Tubarr serves, only when yt-dlp gave both exactly.
-- A joined release's size is the sum over its parts.
ALTER TABLE videos ADD COLUMN size INTEGER CHECK (size > 0);
