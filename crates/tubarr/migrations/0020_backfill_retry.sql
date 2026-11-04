-- When a backfill that failed for a passing reason (network, 5xx, timeout) gets its one early
-- retry. Cleared by the next run, so a second failure waits the full interval.
ALTER TABLE mapping_backfill ADD COLUMN retry_at INTEGER;
