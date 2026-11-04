-- Videos a lookup showed Tubarr cannot serve (no H.264 or AAC stream it can join, gone, age
-- restricted). The matcher leaves them out, so their releases leave the Newznab feed. A later
-- lookup that works clears the row.
CREATE TABLE unplayable_videos (
    video_id TEXT PRIMARY KEY,
    reason TEXT NOT NULL,
    checked_at INTEGER NOT NULL
);

-- Why a grab failed, shown to Sonarr and Radarr as SABnzbd's fail_message.
ALTER TABLE grabs ADD COLUMN fail_message TEXT;
