-- Sanitized watch coverage for the GET events sync envelope (issue #55).
-- Derived from google_calendars_watch_channels; never stores channel tokens
-- or resource ids. Values: missing | expiring | no_successor | covered
ALTER TABLE google_calendars ADD COLUMN watch_coverage TEXT NOT NULL DEFAULT 'missing';
