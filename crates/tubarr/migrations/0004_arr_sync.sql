-- What the Sonarr and Radarr sync keeps per mapping. Rows are never deleted when a title loses
-- its tag or leaves the arr, since pins and grabs point at them: they are marked inactive.
ALTER TABLE mappings ADD COLUMN active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1));
-- Sonarr series id or Radarr movie id. NULL for a mapping made by hand.
ALTER TABLE mappings ADD COLUMN arr_id INTEGER;
ALTER TABLE mappings ADD COLUMN year INTEGER;
-- tt1234567 as the arr has it
ALTER TABLE mappings ADD COLUMN imdb_id TEXT;
