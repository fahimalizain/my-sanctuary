-- Cache of Google `calendars.get` `labelProperties.eventLabels` JSON.
--
-- Empty string = never fetched (cache miss; next sync/import fills it).
-- `"[]"` = fetched, no labels (holiday/reader calendars). Otherwise a JSON
-- array of `{"id","backgroundColor"}` entries (background colors canonicalized
-- to lowercase #rrggbb). Written by `calendars.get` on first import and when
-- the cache is empty; never written to Google.
ALTER TABLE google_calendars ADD COLUMN event_labels TEXT NOT NULL DEFAULT '';
