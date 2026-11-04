-- Videos waiting for a background read of their sample tables, kept so a restart picks them up
-- again. A row goes once its tables are on disk, or after its last failed attempt.
CREATE TABLE prebuild_queue (
    video_id TEXT PRIMARY KEY NOT NULL,
    queued_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT
);
