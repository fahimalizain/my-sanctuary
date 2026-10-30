-- API keys (Settings → API keys).
--
-- Hash-only storage: the plaintext `sk_…` secret is shown once at create time
-- and never stored. `key_hash` is the lowercase SHA-256 hex of the whole
-- secret, so authentication is one indexed equality lookup and a leaked table
-- never yields a usable credential. `key_prefix` keeps the first 11
-- characters (`sk_` + 8) for display after creation.
--
-- Expiry is two columns on purpose. `expires_on` is the civil date the user
-- chose and is display only; `expires_at` is the authoritative RFC 3339 UTC
-- instant — the start of the NEXT local day in the user's zone, so a key is
-- valid through the whole chosen day. NULL `expires_at` = never expires.
--
-- `last_used_at` is RFC 3339 UTC (NULL = never used) and is touched at most
-- once per hour by the authenticating caller.
--
-- Soft delete: a revoked key keeps its row (`deleted_at` stamped) so its
-- prefix stays reserved and the audit trail survives; every read filters
-- `deleted_at IS NULL`. Timestamps are app-stamped (no `datetime('now')`
-- defaults), matching 0021_rules.sql.
CREATE TABLE IF NOT EXISTS api_keys (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  name TEXT NOT NULL,
  key_hash TEXT NOT NULL UNIQUE,
  key_prefix TEXT NOT NULL,
  expires_on TEXT,   -- YYYY-MM-DD the user chose; display only
  expires_at TEXT,   -- RFC 3339 UTC instant; authoritative, NULL = never
  last_used_at TEXT, -- RFC 3339 UTC, NULL = never used
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  deleted_at TEXT,
  FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_api_keys_user_living
  ON api_keys(user_id) WHERE deleted_at IS NULL;
