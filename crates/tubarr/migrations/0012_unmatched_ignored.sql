-- Hani can hide an unmatched note he has looked at. The matcher's upsert never touches this
-- column, so the flag holds for as long as the matcher keeps noting that video.
ALTER TABLE unmatched ADD COLUMN ignored INTEGER NOT NULL DEFAULT 0 CHECK (ignored IN (0, 1));
