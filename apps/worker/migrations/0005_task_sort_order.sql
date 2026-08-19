-- tasks.sort_order carries the per-user, per-status board rank (ADR 0002):
-- Backlog = OPEN, Planned = PLANNED, In Progress, Done, Discarded. Nothing
-- writes an arbitrary rank until slice 2 (the move endpoint), so this
-- migration only adds the column, indexes the lookup every rank query uses,
-- and rewrites the existing piles to the ADR's canonical orders.
--
-- The column default 0 leaves soft-deleted rows untouched: they are invisible
-- to every read and never participate in a rank.
ALTER TABLE tasks ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_tasks_user_status_sort
	ON tasks(user_id, status, sort_order);

-- Backlog (OPEN): oldest first — created_at ASC — so the front of the list
-- matches "the task I've had longest". Ranks run 0..n per user.
WITH ranked AS (
	SELECT id, ROW_NUMBER() OVER (ORDER BY created_at ASC, id ASC) - 1 AS rn
	FROM tasks WHERE status = 'OPEN' AND deleted_at IS NULL
)
UPDATE tasks SET sort_order = (SELECT rn FROM ranked WHERE ranked.id = tasks.id)
WHERE id IN (SELECT id FROM ranked);

-- Done (COMPLETED) and Discarded (DISCARDED): most recently updated first —
-- updated_at DESC — because today's "recently finished on top" is the only
-- ordering the Lists page ever showed, and the rank should not reshuffle it.
-- Two separate statements (not a CASE ORDER BY) keep each ORDER BY
-- unambiguous.
WITH ranked AS (
	SELECT id, ROW_NUMBER() OVER (ORDER BY updated_at DESC, id ASC) - 1 AS rn
	FROM tasks WHERE status IN ('COMPLETED', 'DISCARDED') AND deleted_at IS NULL
)
UPDATE tasks SET sort_order = (SELECT rn FROM ranked WHERE ranked.id = tasks.id)
WHERE id IN (SELECT id FROM ranked);

-- IN_PROGRESS stays 0: at most one task runs per user, so rank is irrelevant
-- there. PLANNED: no rows exist yet.
