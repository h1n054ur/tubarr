-- What each mapping's last matcher run saw: a digest of every row the run reads or writes for it
-- (the mapping, its aliases and pins, its channels' videos and their metadata, its playlist, its
-- releases and notes) and of Sonarr's episode list. A run whose digest is unchanged leaves the
-- mapping alone. A new table, so no existing row is rewritten or locked. A mapping without a row
-- is matched in full on the next run, which then writes one.
CREATE TABLE IF NOT EXISTS match_inputs (
    mapping_id INTEGER PRIMARY KEY REFERENCES mappings (id) ON DELETE CASCADE,
    digest INTEGER NOT NULL,
    matched_at INTEGER NOT NULL
);
