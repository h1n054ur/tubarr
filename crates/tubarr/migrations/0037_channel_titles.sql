-- Channel names, saved when a handle or link typed in the UI is resolved to its channel id, so the
-- mapping drawer can show the name next to the id. Channels entered as ids have no row.
CREATE TABLE channel_titles (
    channel_id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
