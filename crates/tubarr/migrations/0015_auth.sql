-- People who sign in to the web UI. A user has a password, an OpenID Connect link, or both.
CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    username TEXT NOT NULL UNIQUE COLLATE NOCASE,
    -- argon2id PHC string. NULL for users who only sign in through OpenID Connect.
    password_hash TEXT,
    display_name TEXT,
    -- Only admin is used for now.
    role TEXT NOT NULL DEFAULT 'admin' CHECK (role IN ('admin', 'viewer')),
    oidc_issuer TEXT,
    oidc_subject TEXT,
    created_at INTEGER NOT NULL,
    last_login_at INTEGER,
    disabled INTEGER NOT NULL DEFAULT 0 CHECK (disabled IN (0, 1)),
    CHECK ((oidc_issuer IS NULL) = (oidc_subject IS NULL)),
    UNIQUE (oidc_issuer, oidc_subject)
);

-- Signed-in browsers. Only a SHA-256 of the cookie value is stored, so a copy of the database
-- does not hand out sessions.
CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    -- The absolute end, whatever the activity.
    expires_at INTEGER NOT NULL
);

CREATE INDEX sessions_user ON sessions (user_id);
