-- Shows numbered by a running count in the title ("#2569 - Guest"), as podcasts are: the number
-- in a video's title is looked up in TVDB's episode titles instead of read as the episode number.
-- Off (0) by default.
ALTER TABLE mappings ADD COLUMN title_number INTEGER NOT NULL DEFAULT 0 CHECK (title_number IN (0, 1));
