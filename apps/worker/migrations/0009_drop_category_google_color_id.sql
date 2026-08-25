-- Color identity is the hex (`task_categories.color`); Google event writes
-- resolve `eventLabelId` from the calendar's cached event labels at write time
-- (see `google_calendars.event_labels` / slice 4), so the derived
-- `google_color_id` column is no longer stored or served.
--
-- D1 (SQLite) supports `DROP COLUMN`.
ALTER TABLE task_categories DROP COLUMN google_color_id;
