-- Old hashes cannot be reversed. Recover legacy share tokens when their original
-- URL is next used; keep those links valid in the meantime.
ALTER TABLE links ADD COLUMN token TEXT;
ALTER TABLE invitations ADD COLUMN token TEXT;
