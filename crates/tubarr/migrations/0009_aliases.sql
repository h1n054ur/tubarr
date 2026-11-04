-- Other names a film goes by on YouTube, for the film matcher: transliterations ("Kaththi",
-- "Kathi") and dub titles ("Policeodu" for Theri). Compared like the mapping's own title.
CREATE TABLE mapping_aliases (
    mapping_id INTEGER NOT NULL REFERENCES mappings (id) ON DELETE CASCADE,
    alias TEXT NOT NULL CHECK (length(trim(alias)) > 0),
    PRIMARY KEY (mapping_id, alias)
);
