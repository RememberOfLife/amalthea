-- A recovered publication keeps its temporary link until its processing job
-- verifies the bytes and durably cleans it up. File rows remain the job queue.
ALTER TABLE files ADD COLUMN cleanup_upload INTEGER NOT NULL DEFAULT 0 CHECK (cleanup_upload IN (0, 1));
