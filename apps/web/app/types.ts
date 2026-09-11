export interface Task {
  id: string;
  title: string;
  duration: number;
  priority: 'high' | 'medium' | 'low';
  difficulty?: TaskDifficulty;
  completed?: boolean;
}

export interface TimeBlock {
  id: string;
  listId: string;
  listName: string;
  listColor: string;
  startTime: string;
  endTime: string;
  tasks: Task[];
}

export interface List {
  id: string;
  name: string;
  color: string;
}

// A task event that appears directly on the timeline (not inside a time block)
export interface TaskEvent extends Task {
  startTime: string;
  endTime: string;
}

// Union type for timeline items - can be a full time block or a task event
export type TimelineItem = TimeBlock | TaskEvent;

// A Google Calendar event synced from the backend
export interface CalendarEvent {
  id: string;
  calendar_id: string;
  google_event_id: string;
  title: string;
  description: string;
  start_time: string; // ISO 8601
  end_time: string; // ISO 8601
  last_synced_at: string;
  /** Matched category color from the API (`#rrggbb`). May be absent from older workers. */
  color?: string;
  /** Replica bool; missing/undefined means false. All-day is out of GET projection today. */
  is_all_day?: boolean;
  /** IANA zone; empty / missing = unset. */
  start_time_zone?: string;
  end_time_zone?: string;
  /** JSON array of RRULE strings as stored on the replica, e.g. `["RRULE:FREQ=DAILY"]`. Empty / missing = one-shot. Do not type this as string[]. */
  recurrence?: string;
  /** Google recurringEventId for instances; empty / missing = master or one-shot. */
  recurring_event_id?: string;
}

export type CalendarSyncAggregateStatus =
  | 'ready'
  | 'degraded'
  | 'authorization_required';

export type CalendarReplicaState =
  | 'never_initialized'
  | 'ready'
  | 'retrying'
  | 'rebuilding'
  | 'authorization_required'
  | 'disabled';

export type CalendarWatchCoverage =
  | 'missing'
  | 'expiring'
  | 'no_successor'
  | 'covered';

export interface CalendarSyncHealth {
  calendar_id: string;
  state: CalendarReplicaState;
  initial_sync_complete: boolean;
  last_success_at: string | null;
  last_attempt_at: string | null;
  stale: boolean;
  error_code: string | null;
  retry_after_seconds: number | null;
  projection: 'timed_masters_and_exceptions' | string;
  cache_revision: number;
  /** Sanitized watch coverage; never channel secrets. */
  watch_coverage: CalendarWatchCoverage;
  /** Sanitized replica event coverage; never replay payloads. Optional for older fixtures. */
  event_coverage?: 'complete' | 'degraded';
  /** Independent operator warning (1h stale / escalated / auth). Optional for older fixtures. */
  operator_warning?: 'none' | 'stale' | 'escalated' | 'authorization_required';
}

export interface CalendarEventsSync {
  status: CalendarSyncAggregateStatus;
  calendars: CalendarSyncHealth[];
}

// The envelope returned by GET /api/calendar/events
export interface CalendarEventsResponse {
  events: CalendarEvent[];
  source: 'cache' | 'window' | 'mixed' | string;
  sync: CalendarEventsSync;
}

// Response from POST /api/calendar/calendars/:id/repair
export type CalendarRepairStatus = 'queued' | 'in_progress' | 'cooldown';

export interface CalendarRepairResponse {
  status: CalendarRepairStatus;
  retry_after_seconds: number | null;
}

// Request body for POST /api/calendar/events
export interface NewCalendarEventInput {
  /** Local `GoogleCalendar.id` (not the Google calendar id). */
  calendar_id: string;
  summary: string;
  description?: string;
  /** RFC 3339 dateTime. */
  start: string;
  /** RFC 3339 dateTime. */
  end: string;
}

// Envelope for POST /api/calendar/events and PATCH /api/calendar/events/:id
export interface CreateEventResponse {
  event: CalendarEvent;
  source: string;
}

// Request body for PATCH /api/calendar/events/:id — at least one field required
export interface PatchCalendarEventInput {
  start?: string;
  end?: string;
  summary?: string;
}

// Envelope for DELETE /api/calendar/events/:id
export interface DeleteCalendarEventResponse {
  success: boolean;
}

// A calendar from GET /api/calendar/calendars (picker-safe view).
export interface GoogleCalendar {
  id: string;
  google_calendar_id: string;
  summary: string;
  time_zone: string;
  is_primary: boolean;
  access_role: string;
  sync_enabled: boolean;
}

// The envelope returned by GET /api/calendar/calendars
export interface CalendarsResponse {
  calendars: GoogleCalendar[];
}

// A task list as stored in the backend (`task_lists` row shape, snake_case).
// The UI `List` type above is a view of this used by the mock timeline.
export interface TaskList {
  id: string;
  user_id: string;
  name: string;
  color: string;
  sort_order: number;
  created_at: string;
  updated_at: string;
}

// The envelope returned by GET /api/lists
export interface TaskListsResponse {
  lists: TaskList[];
}

// Request body for POST /api/lists
export interface NewListInput {
  name: string;
  color: string;
}

// Request body for PATCH /api/lists/:id — every field optional
export interface UpdateListInput {
  name?: string;
  color?: string;
}

// The envelope returned by POST /api/lists and PATCH /api/lists/:id
export interface TaskListResponse {
  list: TaskList;
}

// The envelope returned by DELETE /api/lists/:id
export interface DeleteListResponse {
  success: boolean;
}

// A title-matching regex pattern attached to a category
// (`task_category_patterns` row shape, snake_case).
export interface TaskCategoryPattern {
  id: string;
  category_id: string;
  regex: string;
  google_calendar_id: string | null;
  sort_order: number;
  created_at: string;
  updated_at: string;
}

// A category in the one-level taxonomy, as returned by the API
// (`task_categories` row shape + patterns + inherited_list_id).
// - Roots have `list_id` set; children store `list_id: null` and inherit
//   the parent root's list via `inherited_list_id`.
// - `untracked` is a system-seeded, undeletable root with `list_id: null`
//   and `is_untracked: true` — never shown under a list.
export interface Category {
  id: string;
  user_id: string;
  list_id: string | null;
  parent_id: string | null;
  title: string;
  slug: string;
  color: string;
  is_productive: boolean;
  google_calendar_id: string | null;
  sort_order: number;
  is_untracked: boolean;
  created_at: string;
  updated_at: string;
  patterns: TaskCategoryPattern[];
  inherited_list_id: string | null;
}

// The envelope returned by GET /api/categories
export interface CategoriesResponse {
  categories: Category[];
}

// The envelope returned by POST /api/categories and PATCH /api/categories/:id
export interface CategoryResponse {
  category: Category;
}

// The envelope returned by DELETE /api/categories/:id
export interface DeleteCategoryResponse {
  success: boolean;
}

// One pattern row of a create/update body
export interface NewCategoryPatternInput {
  regex: string;
  google_calendar_id?: string | null;
}

// Request body for POST /api/categories. `list_id` is required for roots,
// `parent_id` for children (never both).
export interface NewCategoryInput {
  title: string;
  slug?: string;
  color: string;
  is_productive?: boolean;
  google_calendar_id?: string | null;
  list_id?: string | null;
  parent_id?: string | null;
  sort_order?: number;
  is_untracked?: boolean;
  patterns: NewCategoryPatternInput[];
}

// Request body for PATCH /api/categories/:id — every field optional;
// `patterns` replaces the whole set when present.
export interface UpdateCategoryInput {
  title?: string;
  color?: string;
  is_productive?: boolean;
  google_calendar_id?: string | null;
  sort_order?: number;
  patterns?: NewCategoryPatternInput[];
}

export type TaskPriority = 'high' | 'medium' | 'low';
export type TaskDifficulty = 'easy' | 'medium' | 'hard';

export const TASK_PRIORITIES: TaskPriority[] = ['high', 'medium', 'low'];

export const TASK_PRIORITY_LABELS: Record<TaskPriority, string> = {
  high: 'P0',
  medium: 'P1',
  low: 'P2',
};

// The computed category attached to a task (`tasks` have no category_id
// column — the server classifies the title and returns this summary). The
// frontend groups tasks under `category.id`.
export interface TaskCategorySummary {
  id: string;
  title: string;
  slug: string;
  list_id: string | null;
  inherited_list_id: string | null;
  is_untracked: boolean;
  color: string;
}

// Response of GET /api/tasks/classify?title=…&category_id=… — the title→
// category match the create/update endpoints enforce, as a preview (never
// writes). Externally-tagged serde enum:
//   {"Matched":{"category":{...},"prefix":..,"suffix":..,"persist_title":..,"display_title":..}}
//   {"Untracked":{"conflict":bool,"categories":[...],"prefix":..,"suffix":..,"persist_title":..,"display_title":..}}
// Every variant carries the title chrome and the two views the modal needs:
// `prefix`/`suffix` are the fixed text around the input slot (empty when
// there is none), `persist_title` is what a save would store (filled, and
// guaranteed to file to the matched category), and `display_title` is the
// hole the input should show.
export type ClassifyResponse =
  | {
      Matched: {
        category: TaskCategorySummary;
        prefix: string;
        suffix: string;
        persist_title: string;
        display_title: string;
      };
    }
  | {
      Untracked: {
        conflict: boolean;
        categories: TaskCategorySummary[];
        prefix: string;
        suffix: string;
        persist_title: string;
        display_title: string;
      };
    };

// A task as returned by the API: the `tasks` row shape (snake_case) plus the
// computed `category`. `status` is driven by the timer endpoints: start →
// "IN_PROGRESS", stop → "OPEN", pause → "PLANNED" (since ADR 0002),
// complete/discard → their terminal states.
export type TaskStatus =
  | 'OPEN'
  | 'PLANNED'
  | 'IN_PROGRESS'
  | 'COMPLETED'
  | 'DISCARDED';

export interface TaskRecord {
  id: string;
  user_id: string;
  // The stored full string — always the authority.
  title: string;
  // Computed by the API: the hole split off `title` under the category's
  // first matching pattern ("Review Q3 | Work" → "Review Q3"). Never null; a
  // patternless match (e.g. "Work"), untracked, or conflict keep `title`.
  display_title: string;
  description: string;
  duration_minutes: number;
  priority: TaskPriority;
  difficulty: TaskDifficulty;
  // Per-user, per-status board rank; 0 = front of the column (Backlog
  // prepends). Part of the row since migration 0005.
  sort_order: number;
  status: TaskStatus;
  created_at: string;
  updated_at: string;
  // Computed by the API: `true` only for the user's focused task (its id
  // equals the user's `focused_task_id` pointer AND it is a living
  // IN_PROGRESS row). Never stored on the task; reads paint `false`
  // everywhere when the pointer is missing or dangling.
  focused: boolean;
  category: TaskCategorySummary;
}

// The envelope returned by GET /api/tasks
export interface TasksResponse {
  tasks: TaskRecord[];
}

// The envelope returned by POST /api/tasks and PATCH /api/tasks/:id
export interface TaskResponse {
  task: TaskRecord;
}

// The envelope returned by DELETE /api/tasks/:id
export interface DeleteTaskResponse {
  success: boolean;
}

// Request body for POST /api/tasks. The title must uniquely match a
// non-untracked category (the server decides and explains 400s); duration
// defaults to 15 minutes, priority to 'medium', difficulty to 'easy'.
export interface NewTaskInput {
  title: string;
  description?: string;
  duration_minutes?: number;
  priority?: TaskPriority;
  difficulty?: TaskDifficulty;
}

// Request body for PATCH /api/tasks/:id — every field optional. A present
// `title` must uniquely match a non-untracked category. Status is never
// updatable through PATCH: use the timer endpoints (start/stop/pause/
// complete/discard) or the move endpoint instead.
export interface UpdateTaskInput {
  title?: string;
  description?: string;
  duration_minutes?: number;
  priority?: TaskPriority;
  difficulty?: TaskDifficulty;
}

// Request body for POST /api/tasks/:id/move — the board drop. The server
// dispatches the ADR 0002 transition matrix (start/stop/pause/complete/
// discard/plan/unplan/reopen), then places the task at `sort_order` in the
// target status. Same-status moves are reorders (including IN_PROGRESS →
// IN_PROGRESS with a rank — In Progress is a real pile). `sort_order` is
// optional: drops send an absolute rank; no-drop callers (modal status pills,
// column+ create-then-move) omit it and the server applies the column
// default.
export interface MoveTaskInput {
  status: TaskStatus;
  sort_order?: number;
}

// The envelope returned by POST /api/tasks/:id/move: the moved task and the
// Google event the dispatched action touched. The move event carries extra
// internal cache fields; the board only distinguishes null/event, so it is
// typed loosely.
export interface MoveTaskResponse {
  task: TaskRecord;
  event: CalendarEvent | null;
}

// The envelope returned by POST /api/tasks/:id/focus and DELETE /api/focus
// (task-focus). `task` is the task that now holds (POST) or has just lost
// (DELETE) focus — the frontend also uses it to gauge whether focus is on.
// `previous` is the task that lost focus on a switch (null otherwise), and
// `events` are the new calendar segments created by the request.
export interface FocusTaskResponse {
  task: TaskRecord | null;
  previous: TaskRecord | null;
  events: CalendarEvent[];
}

// A routine (ADR 0004 amendment): a standing definition of repeated work —
// never completable, never on the Board. The `routines` row shape
// (snake_case) plus the computed `category`.
export interface RoutineRecord {
  id: string;
  user_id: string;
  title: string;
  estimated_minutes: number;
  // The whole recurrence in one TEXT blob — exactly two `\n`-separated lines
  // (ADR 0004 amendment): `DTSTART:YYYYMMDDTHHMMSS` (floating local, no
  // Z/TZID) + `RRULE:<body>`. No EXDATE/RDATE/EXRULE/TZID anywhere.
  rrule: string;
  sort_order: number;
  created_at: string;
  updated_at: string;
  // Computed per title with the same matcher as tasks (never fails — an
  // unmatched title keeps the `untracked` summary).
  category: TaskCategorySummary;
}

// The envelope returned by GET /api/routines
export interface RoutinesResponse {
  routines: RoutineRecord[];
}

// The envelope returned by POST /api/routines and PATCH /api/routines/:id
export interface RoutineResponse {
  routine: RoutineRecord;
}

// Request body for POST /api/routines. `estimated_minutes` defaults to 15
// server-side (min 1). The title must uniquely match a non-untracked category
// (the server decides and explains 400s); `rrule` is the two-line recurrence
// blob (`DTSTART:` line + `RRULE:` line), validated on create.
export interface NewRoutineInput {
  title: string;
  estimated_minutes?: number;
  rrule: string;
}

// Request body for PATCH /api/routines/:id — every field optional. A present
// `title` must uniquely match a non-untracked category; a present `rrule`
// blob is validated on its own (it carries its own DTSTART).
export interface UpdateRoutineInput {
  title?: string;
  estimated_minutes?: number;
  rrule?: string;
  sort_order?: number;
}

// ──────────────────────────────────────────
// Agenda (ADR 0004 § Agenda rules) — shapes mirror
// `packages/api-core/src/agenda.rs` view structs.
// ──────────────────────────────────────────

// Occurrence states (lowercase on purpose — these are NOT task statuses).
export type OccurrenceStatus = 'pending' | 'in_progress' | 'done' | 'skipped';

// One local-date instance of a routine (ADR 0004 § Nouns): every
// `routine_occurrences` column plus the routine's display fields
// (`estimated_minutes`, the `rrule` recurrence blob), the **resolved** title
// and its computed category.
export interface OccurrenceRecord {
  id: string;
  routine_id: string;
  user_id: string;
  // Local civil date `YYYY-MM-DD`.
  local_date: string;
  // Stored override; `null` = inherit the routine title.
  title: string | null;
  // `title ?? routine.title` — the display title and the classify input.
  resolved_title: string;
  // `pending | in_progress | done | skipped`.
  status: OccurrenceStatus;
  // From the routine (the estimate lives on the standing definition).
  estimated_minutes: number;
  // The routine's two-line recurrence blob (`DTSTART:` + `RRULE:`), for
  // display.
  rrule: string;
  // Local calendar id once started; `null` until then.
  calendar_id: string | null;
  // Google event id of the one-shot log; `null` until then.
  google_event_id: string | null;
  created_at: string;
  updated_at: string;
  // Computed from the **resolved** title with the same matcher as tasks.
  category: TaskCategorySummary;
}

// Agenda item kinds (v1: occurrences are auto-seeded; POST is tasks-only).
export type AgendaItemKind = 'task' | 'occurrence';

// One agenda membership row with its embed: the task (kind `task`) or the
// occurrence (kind `occurrence`) — exactly one is non-null.
export interface AgendaItemRecord {
  id: string;
  user_id: string;
  // Local civil date `YYYY-MM-DD`.
  local_date: string;
  kind: AgendaItemKind;
  ref_id: string;
  sort_order: number;
  // Non-null for kind=task (full `TaskRecord`, `focused` included).
  task: TaskRecord | null;
  // Non-null for kind=occurrence.
  occurrence: OccurrenceRecord | null;
}

// The envelope returned by GET /api/agenda?date=YYYY-MM-DD (seeds on read;
// the date query is optional — missing/blank = civil today). `today` +
// `time_zone` are the server's civil today (ADR 0004 amendment): the civil
// date of now in the user's primary Google calendar's IANA `time_zone` —
// the browser must never compute "today" itself.
export interface AgendaResponse {
  items: AgendaItemRecord[];
  // YYYY-MM-DD — civil today in `time_zone`; Home's "Today" anchor.
  today: string;
  // IANA name of the zone that produced `today` (the primary calendar's
  // `time_zone`, or "UTC" when the user has no primary calendar).
  time_zone: string;
}

// The envelope returned by POST /api/agenda/items,
// POST /api/agenda/items/:id/move and POST /api/agenda/items/:id/reschedule.
export interface AgendaItemResponse {
  item: AgendaItemRecord;
}

// Request body for POST /api/agenda/items — tasks only in v1 (occurrences
// are auto-seeded); the `date` is required. `sort_order` is optional: the
// server appends at `max+1` for the date when omitted.
export interface NewAgendaItemInput {
  kind: 'task';
  ref_id: string;
  date: string;
  sort_order?: number;
}

// Request body for POST /api/agenda/items/:id/move — the absolute rank the
// item lands on (peers at/after it shift up one, within that date's pile).
export interface MoveAgendaItemInput {
  sort_order: number;
}

// Request body for POST /api/agenda/items/:id/reschedule — the local civil
// date (`YYYY-MM-DD`) the slot relocates to. Occurrences move only while
// `pending | skipped` (skipped → pending; in_progress/done → 400); tasks
// move the membership slot only, task status unchanged. Same date → 200
// no-op.
export interface RescheduleAgendaItemInput {
  date: string;
}

// The envelope returned by PATCH /api/occurrences/:id,
// POST /api/occurrences/:id/complete, /skip, /pause, and /reopen.
export interface OccurrenceResponse {
  occurrence: OccurrenceRecord;
}

// Request body for PATCH /api/occurrences/:id — the day-level title override;
// empty clears back to inheriting the routine title.
export interface UpdateOccurrenceInput {
  title: string;
}

// The envelope returned by POST /api/occurrences/:id/start: the fresh
// occurrence plus the one-shot Google log this start created (`event` is null
// on the idempotent in_progress no-op — no second event was opened).
export interface OccurrenceActionResponse {
  occurrence: OccurrenceRecord;
  event: CalendarEvent | null;
}
