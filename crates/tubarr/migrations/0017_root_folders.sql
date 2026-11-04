-- Root folder routing: each title lives under the Sonarr or Radarr root folder of its Jellyfin
-- library (Pakistani dramas, Indian dramas, Indian films apart from the English ones).
-- The user's override: NULL follows the channel and language config, 'none' turns routing off,
-- anything else is an absolute root folder path.
ALTER TABLE mappings ADD COLUMN root_folder TEXT;

-- What routing knows per mapping. `current` is the arr's rootFolderPath at the last sync.
-- `attempted` is the wanted folder of the last move Tubarr asked for: it never asks twice for the
-- same folder, so a move the user undid in the arr stays undone until the wanted folder changes.
CREATE TABLE root_folder_state (
    mapping_id INTEGER PRIMARY KEY REFERENCES mappings (id) ON DELETE CASCADE,
    current TEXT,
    attempted TEXT,
    -- moved, missing (the arr has no such root folder) or failed: <why>
    result TEXT,
    result_at INTEGER
);
