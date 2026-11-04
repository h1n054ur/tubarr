-- The match state refresh looks up notes by video alone, which the (mapping_id, video_id) key
-- cannot serve, so every video scanned the whole table while the refresh held the write lock.
CREATE INDEX unmatched_video ON unmatched (video_id);
