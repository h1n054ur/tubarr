-- A video marked for missing formats (no H.264 video or AAC audio yet, common on brand-new
-- uploads while YouTube is still encoding) is looked up again later. One that is gone or age
-- restricted is not. Existing rows are sorted by the wording of the missing-formats error.
ALTER TABLE unplayable_videos ADD COLUMN retry INTEGER NOT NULL DEFAULT 0;
UPDATE unplayable_videos SET retry = 1
    WHERE reason LIKE '%has no H.264 video%' OR reason LIKE '%has no AAC audio%';
