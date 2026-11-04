-- Every video id Tubarr ever put into a release, a pin or a written grab, as a whole video or one
-- part of a joined one. /play serves only these. Rows are never deleted: a release the matcher
-- replaces, a mapping removed or a grab Sonarr clears from history can leave a .strm behind that
-- still has to play.
CREATE TABLE released_videos (
    video_id TEXT PRIMARY KEY NOT NULL
) WITHOUT ROWID;

-- Grabs and pins hold a joined upload's ids as one string, comma separated. Video ids never hold
-- a quote, so the string turns into a JSON array by replacing the commas. Anything that is not
-- valid JSON then is skipped rather than failing the write.
CREATE TRIGGER released_videos_release AFTER INSERT ON releases
BEGIN
    INSERT OR IGNORE INTO released_videos (video_id)
    SELECT NEW.video_id
    UNION SELECT value FROM json_each(CASE WHEN json_valid(NEW.part_ids) THEN NEW.part_ids ELSE '[]' END);
END;

CREATE TRIGGER released_videos_pin AFTER INSERT ON pins
BEGIN
    INSERT OR IGNORE INTO released_videos (video_id)
    SELECT value FROM json_each(CASE WHEN json_valid('["' || replace(NEW.video_id, ',', '","') || '"]')
        THEN '["' || replace(NEW.video_id, ',', '","') || '"]' ELSE '[]' END);
END;

CREATE TRIGGER released_videos_grab AFTER INSERT ON grabs WHEN NEW.status = 'completed'
BEGIN
    INSERT OR IGNORE INTO released_videos (video_id)
    SELECT value FROM json_each(CASE WHEN json_valid('["' || replace(NEW.video_id, ',', '","') || '"]')
        THEN '["' || replace(NEW.video_id, ',', '","') || '"]' ELSE '[]' END);
END;

-- What earlier versions released, pinned and grabbed. Releases replaced or removed before this
-- version and grabs already cleared from history are gone from here; their ids come back from the
-- play index files at startup.
INSERT OR IGNORE INTO released_videos (video_id)
SELECT video_id FROM releases
UNION SELECT p.value FROM releases r, json_each(CASE WHEN json_valid(r.part_ids) THEN r.part_ids ELSE '[]' END) p
UNION SELECT j.value FROM pins s, json_each(CASE WHEN json_valid('["' || replace(s.video_id, ',', '","') || '"]')
    THEN '["' || replace(s.video_id, ',', '","') || '"]' ELSE '[]' END) j
UNION SELECT j.value FROM grabs g, json_each(CASE WHEN json_valid('["' || replace(g.video_id, ',', '","') || '"]')
    THEN '["' || replace(g.video_id, ',', '","') || '"]' ELSE '[]' END) j
WHERE g.status = 'completed';
