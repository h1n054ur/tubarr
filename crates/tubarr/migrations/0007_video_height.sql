-- Height of the format Tubarr serves, kept with the metadata so the matcher still knows it after
-- the stream half of a lookup expires.
ALTER TABLE videos ADD COLUMN height INTEGER CHECK (height > 0);

-- Lookups still cached know it already.
UPDATE videos SET height = (
    SELECT nullif(json_extract(r.formats, '$.video.height'), 0) FROM resolve_cache r
    WHERE r.video_id = videos.video_id
);
