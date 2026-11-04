-- Who added an alias: the arr sync (Sonarr's alternate titles, refreshed on every sync) or the
-- user. A sync never removes a user alias.
ALTER TABLE mapping_aliases ADD COLUMN source TEXT NOT NULL DEFAULT 'user'
    CHECK (source IN ('arr', 'user'));
