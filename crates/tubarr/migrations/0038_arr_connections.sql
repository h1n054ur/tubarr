-- Sonarr and Radarr connections saved from the Settings page, for installs whose config file has
-- no [sonarr] or [radarr]. The config file wins when it has one. The API key is kept as it is:
-- Tubarr must send it, so it cannot be hashed, and the admin API only ever says whether it is set.
CREATE TABLE arr_connections (
    app TEXT PRIMARY KEY CHECK (app IN ('sonarr', 'radarr')),
    url TEXT NOT NULL,
    api_key TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
