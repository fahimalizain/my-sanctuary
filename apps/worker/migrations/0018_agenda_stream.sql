-- The agenda becomes an infinite stream of days (issue #99, slice 1): GET
-- /api/agenda is a pure range read and never writes, and a task now holds at
-- most ONE membership row across all days (its planned slot), instead of one
-- row per date. Three steps:
--   1. drop task-kind rows whose task is gone,
--   2. collapse the remaining duplicates to the greatest local_date,
--   3. pin the invariant with a partial unique index.

-- 1) Orphans: a task-kind row whose ref_id names no LIVING tasks row.
-- Missing and soft-deleted tasks both go — the range read omits them anyway,
-- and the stream must not keep resurrectable slots around.
DELETE FROM agenda_items
WHERE kind = 'task'
  AND ref_id NOT IN (SELECT id FROM tasks WHERE deleted_at IS NULL);

-- 2) Duplicates: keep the greatest local_date per (user_id, ref_id) — the
-- latest planned day wins — and delete the other task-kind rows. YYYY-MM-DD
-- sorts chronologically, and UNIQUE (user_id, local_date, kind, ref_id) makes
-- same-date ties impossible. The nested SELECT is required: SQLite refuses a
-- DELETE whose WHERE subquery reads the table being deleted from in the same
-- statement, so the derived table materializes the keep set first.
DELETE FROM agenda_items
WHERE kind = 'task'
  AND id NOT IN (
    SELECT id FROM (
      SELECT a.id
      FROM agenda_items a
      WHERE a.kind = 'task'
        AND a.local_date = (
          SELECT MAX(b.local_date)
          FROM agenda_items b
          WHERE b.kind = 'task'
            AND b.user_id = a.user_id
            AND b.ref_id = a.ref_id
        )
    )
  );

-- 3) One task-kind row per (user, task). The old per-date UNIQUE stays for
-- occurrence rows (a daily snooze may legitimately put two rows of the same
-- routine on one day).
CREATE UNIQUE INDEX IF NOT EXISTS idx_agenda_items_one_per_task
  ON agenda_items(user_id, ref_id) WHERE kind = 'task';
