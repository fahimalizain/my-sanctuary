-- App-owned leaf category. NULL = unclassified (not the untracked sink).
-- No FK: category delete must not fail event sync; a later reclassify job clears stale ids.
ALTER TABLE calendar_events ADD COLUMN category_id TEXT;

-- Google organizer.self = true, OR organizer absent → 1. Organizer present without self → 0.
-- Existing rows default to 1 (unknown organizer was dropped from raw_json; treat as "no organizer" until the next Google payload).
ALTER TABLE calendar_events ADD COLUMN is_self_organized INTEGER NOT NULL DEFAULT 1;

-- Google eventType. '' = unknown (legacy rows). Reports will treat ''/'default'/'fromGmail' as included.
ALTER TABLE calendar_events ADD COLUMN event_type TEXT NOT NULL DEFAULT '';

CREATE INDEX IF NOT EXISTS idx_events_category ON calendar_events(category_id);
