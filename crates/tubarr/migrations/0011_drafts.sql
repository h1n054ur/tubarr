-- Drafts of TVDB and TMDB entries for titles missing there, made from what the watcher stored.
-- Kept so the UI can show them again. Hani enters them by hand, Tubarr posts nothing.
CREATE TABLE drafts (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('show', 'film')),
    title TEXT NOT NULL,
    -- the request that made it, so it can be made again once more episodes are out
    request TEXT NOT NULL CHECK (json_valid(request)),
    draft TEXT NOT NULL CHECK (json_valid(draft)),
    -- the copy-paste block in the site's form order
    text TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX drafts_created ON drafts (created_at);
