-- Emailed one-time tokens must expire server-side (unix seconds, NULL = no
-- token pending). Tokens issued before this column existed have a NULL expiry
-- and are therefore rejected, which fails safe.
ALTER TABLE users ADD COLUMN reset_token_expires_at INTEGER;

ALTER TABLE users ADD COLUMN email_change_expires_at INTEGER;
