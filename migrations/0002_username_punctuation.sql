-- Rebuild invitations to widen the username CHECK without changing deployed
-- migration checksums or losing existing invitation tokens and reservations.
CREATE TABLE invitations_new (
    token_hash TEXT PRIMARY KEY,
    token TEXT NOT NULL UNIQUE,
    username TEXT COLLATE NOCASE CHECK (
        username IS NULL OR (
            length(username) BETWEEN 1 AND 64
            AND username NOT LIKE '.%'
            AND username NOT GLOB '*[^A-Za-z0-9_.@+-]*'
            AND instr(username, char(0)) = 0
        )
    ),
    quota_bytes INTEGER NOT NULL CHECK (quota_bytes >= 0),
    expires_at INTEGER NOT NULL
);
INSERT INTO invitations_new (token_hash, token, username, quota_bytes, expires_at)
SELECT token_hash, token, username, quota_bytes, expires_at FROM invitations;
DROP TABLE invitations;
ALTER TABLE invitations_new RENAME TO invitations;
CREATE INDEX invitations_username ON invitations(username, expires_at);
