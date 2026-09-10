-- Issue #60: generation membership for authoritative calendar rebuild.
-- Tracks Google event ids seen during a completed merge-full walk so the
-- terminal fenced sweep can soft-delete absent ids. No Google pageToken,
-- nextSyncToken, channel secrets, or raw bodies — only opaque ids and
-- RFC 3339 TEXT timestamps.
CREATE TABLE IF NOT EXISTS calendar_replica_seen (
	calendar_id TEXT NOT NULL,
	run_id TEXT NOT NULL,
	google_event_id TEXT NOT NULL,
	created_at TEXT NOT NULL,
	PRIMARY KEY (calendar_id, run_id, google_event_id)
);

CREATE INDEX IF NOT EXISTS idx_replica_seen_calendar_run
	ON calendar_replica_seen (calendar_id, run_id);
