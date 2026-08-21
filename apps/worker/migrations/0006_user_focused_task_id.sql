-- users.focused_task_id is the one-focus lock (task-focus feature, slice 1):
-- at most one living IN_PROGRESS task of the user is "focused". Nothing writes
-- it yet (slice 3 adds the /focus verbs); this migration only adds the column.
--
-- Nullable TEXT: NULL / absent = nothing focused. Tasks are soft-deleted, so no
-- FK is declared — a dangling pointer is allowed and treated as null on read.
ALTER TABLE users ADD COLUMN focused_task_id TEXT;
