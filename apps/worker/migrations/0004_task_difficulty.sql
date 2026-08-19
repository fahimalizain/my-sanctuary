-- Persisted task difficulty (easy|medium|hard), orthogonal to priority and
-- duration. Required from here on; the column default backfills existing rows
-- (and any INSERT that omits the column) to 'easy'.
ALTER TABLE tasks ADD COLUMN difficulty TEXT NOT NULL DEFAULT 'easy';
