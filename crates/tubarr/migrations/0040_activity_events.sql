-- The Activity page's searches, arr pushes and play starts, kept across restarts until housekeeping
-- drops them after `[activity] retention`. `data` is the event as the admin API shows it (JSON,
-- `kind` included), so a new field needs no migration. Newest first is rowid order.
CREATE TABLE activity_events (
    id INTEGER PRIMARY KEY,
    at INTEGER NOT NULL,
    kind TEXT NOT NULL,
    data TEXT NOT NULL
);

CREATE INDEX activity_events_kind ON activity_events (kind);
CREATE INDEX activity_events_at ON activity_events (at);
