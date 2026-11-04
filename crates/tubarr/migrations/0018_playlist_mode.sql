-- What a mapping's playlist is for. `source`: its videos are candidates for the title matcher,
-- and titles give the episode number. `order`: the place in the playlist gives the number, for
-- shows whose titles carry none.
ALTER TABLE mappings ADD COLUMN playlist_mode TEXT NOT NULL DEFAULT 'source'
    CHECK (playlist_mode IN ('source', 'order'));
