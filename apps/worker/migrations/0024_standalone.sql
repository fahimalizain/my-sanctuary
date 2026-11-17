-- Standalone (zero-Google) local development mode (issue #138 / ADR 0012).
--
-- Every user is either a Google user or a standalone user. `google` is the
-- default so every existing row keeps today's behaviour and an un-migrated
-- code path can never invent a standalone user.
--
-- This column is read by `USER_GET_*_SQL` / the login upsert (`SELECT *`), by
-- `API_KEY_AUTHENTICATE_SQL` (aliased `user_integration_type`), and by the
-- cron filters (`... AND user_id IN (SELECT id FROM users WHERE
-- integration_type = 'google')`). Deploy the migration BEFORE the code that
-- references it: an un-migrated database fails API-key authentication and
-- errors every cron tick.
--
-- Standalone usernames/passwords live in their own table (not on `users`) so
-- Google identities are untouched and `users.email` keeps its UNIQUE slot for
-- the synthetic `<username>@standalone.invalid` address.

ALTER TABLE users ADD COLUMN integration_type TEXT NOT NULL DEFAULT 'google';

-- Username/password credentials for standalone users. `password_hash` is the
-- PBKDF2-HMAC-SHA256 string from api-core (`pbkdf2-sha256$<iterations>$…`);
-- plaintext passwords are never stored. One row per user, and `username` is
-- UNIQUE across the deployment so login can resolve an identity from the
-- username alone. Timestamps are app-stamped RFC 3339 UTC, matching
-- 0021_rules.sql / 0023_api_keys.sql (no `datetime('now')` defaults).
CREATE TABLE IF NOT EXISTS local_credentials (
  user_id TEXT PRIMARY KEY,
  username TEXT NOT NULL UNIQUE,
  password_hash TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_local_credentials_username
  ON local_credentials(username);
