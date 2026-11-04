-- The channel's IANA time zone ("America/Chicago"), for the air date of an upload. It follows DST,
-- which utc_offset_mins cannot, and wins over it when both are set. Null keeps the offset.
ALTER TABLE mappings ADD COLUMN timezone TEXT;
