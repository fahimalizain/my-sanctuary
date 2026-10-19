-- Distinguishes "classified (including a deliberate no-match)" from
-- "never classified". The reclassify verification must only treat the latter
-- as pending work: an event whose title matches no pattern stays
-- category_id = NULL forever and must not rewind the scan.
--
-- Existing rows default to 1: the id walk still re-evaluates every row from its
-- cursor; this flag only gates the verification/restart checks.
ALTER TABLE calendar_events ADD COLUMN category_classified INTEGER NOT NULL DEFAULT 1;
