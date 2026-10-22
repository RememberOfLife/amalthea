CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    username TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    quota_bytes INTEGER NOT NULL CHECK (quota_bytes >= 0),
    created_at INTEGER NOT NULL
);
CREATE TABLE invitations (
    token_hash TEXT PRIMARY KEY,
    quota_bytes INTEGER NOT NULL CHECK (quota_bytes >= 0),
    expires_at INTEGER NOT NULL
);
CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL
);
CREATE INDEX sessions_expiry ON sessions(expires_at);
CREATE TABLE files (
    id TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    size INTEGER NOT NULL CHECK (size >= 0),
    sha256 TEXT NOT NULL,
    disk_stamp TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (user_id, name)
);
CREATE TABLE uploads (
    id TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    size INTEGER NOT NULL CHECK (size >= 0),
    sha256 TEXT NOT NULL,
    offset INTEGER NOT NULL DEFAULT 0 CHECK (offset >= 0),
    updated_at INTEGER NOT NULL,
    UNIQUE (user_id, name)
);
CREATE TABLE links (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL,
    expires_at INTEGER,
    revoked_at INTEGER,
    created_at INTEGER NOT NULL
);
CREATE TABLE access_logs (
    id INTEGER PRIMARY KEY,
    link_id TEXT NOT NULL REFERENCES links(id) ON DELETE CASCADE,
    event TEXT NOT NULL CHECK (event IN ('view', 'download')),
    at INTEGER NOT NULL,
    range_start INTEGER,
    range_end INTEGER
);
CREATE INDEX access_logs_link ON access_logs(link_id, at);
