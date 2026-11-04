-- Who set a mapping's language: the arr sync (its original language), a configured channel
-- (`[channels]`), or the user. Sonarr has no Urdu and reports such shows as English, so a show
-- still on the arr's English or Unknown takes its first channel's language. A user-set language
-- is never changed by Tubarr.
ALTER TABLE mappings ADD COLUMN language_source TEXT NOT NULL DEFAULT 'user'
    CHECK (language_source IN ('arr', 'channel', 'user'));
-- Rows the sync made earlier that still carry the arr's default.
UPDATE mappings SET language_source = 'arr'
    WHERE arr_id IS NOT NULL AND lower(language) IN ('english', 'unknown');
