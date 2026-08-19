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

-- Ranks are PARTITIONed BY user_id: the board rank is per user, so every
-- user's first OPEN task must be 0 no matter how many OPEN tasks other users
-- have, and rows from different users never share a sequence.
--
-- Backlog (OPEN): oldest first — created_at ASC — so the front of the list
-- matches "the task I've had longest". Ranks run 0..n for each user.
WITH ranked AS (
	SELECT id, ROW_NUMBER() OVER (PARTITION BY user_id ORDER BY created_at ASC, id ASC) - 1 AS rn
	FROM tasks WHERE status = 'OPEN' AND deleted_at IS NULL
)
UPDATE tasks SET sort_order = (SELECT rn FROM ranked WHERE ranked.id = tasks.id)
WHERE id IN (SELECT id FROM ranked);

-- Done (COMPLETED) and Discarded (DISCARDED): most recently updated first —
-- updated_at DESC — because today's "recently finished on top" is the only
-- ordering the Lists page ever showed, and the rank should not reshuffle it.
-- Done and Discarded are INDEPENDENT piles: a user's 5 done + 3 discarded
-- tasks must rank 0..4 and 0..2, not one shared 0..7 sequence, so each status
-- gets its own statement (never a CASE ORDER BY inside one window).
--
-- COMPLETED:
WITH ranked AS (
	SELECT id, ROW_NUMBER() OVER (PARTITION BY user_id ORDER BY updated_at DESC, id ASC) - 1 AS rn
	FROM tasks WHERE status = 'COMPLETED' AND deleted_at IS NULL
)
UPDATE tasks SET sort_order = (SELECT rn FROM ranked WHERE ranked.id = tasks.id)
WHERE id IN (SELECT id FROM ranked);

-- DISCARDED:
WITH ranked AS (
	SELECT id, ROW_NUMBER() OVER (PARTITION BY user_id ORDER BY updated_at DESC, id ASC) - 1 AS rn
	FROM tasks WHERE status = 'DISCARDED' AND deleted_at IS NULL
)
UPDATE tasks SET sort_order = (SELECT rn FROM ranked WHERE ranked.id = tasks.id)
WHERE id IN (SELECT id FROM ranked);

-- IN_PROGRESS stays 0: at most one task runs per user, so rank is irrelevant
-- there. PLANNED: no rows exist yet.
