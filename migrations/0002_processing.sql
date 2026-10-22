ALTER TABLE files ADD COLUMN status TEXT NOT NULL DEFAULT 'ready' CHECK (status IN ('processing', 'ready', 'failed'));
ALTER TABLE files ADD COLUMN processing_error TEXT;
ALTER TABLE files ADD COLUMN expected_sha256 TEXT;
CREATE INDEX files_processing ON files(status, user_id, created_at);
