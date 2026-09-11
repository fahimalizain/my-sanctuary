-- Deterministic poison quarantine (issue #60).
-- Retains replay payload for operator/repair. Never expose payload,
-- tokens, channel ids, or raw Google bodies on the GET envelope or in
-- last_error_code / diagnostics / logs.
-- event_coverage: complete | degraded

ALTER TABLE google_calendars ADD COLUMN event_coverage TEXT NOT NULL DEFAULT 'complete';

CREATE TABLE IF NOT EXISTS calendar_event_quarantine (
  calendar_id TEXT NOT NULL,
  google_event_id TEXT NOT NULL DEFAULT '',
  phase TEXT NOT NULL,
  error_class TEXT NOT NULL,
  replay_payload TEXT NOT NULL,
  first_seen_at TEXT NOT NULL,
  last_attempt_at TEXT NOT NULL,
  attempt_count INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (calendar_id, google_event_id, phase)
);
