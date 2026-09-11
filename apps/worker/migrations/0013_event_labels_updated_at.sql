-- When `google_calendars.event_labels` was last successfully fetched
-- (`calendars.get`). NULL = never stamped (pre-migration / never fetched).
-- Written only by set_event_labels; calendarList upsert must never touch it.
ALTER TABLE google_calendars ADD COLUMN event_labels_updated_at TEXT;
