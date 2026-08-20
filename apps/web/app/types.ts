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
}

// The envelope returned by GET /api/calendar/events
export interface CalendarEventsResponse {
  events: CalendarEvent[];
  source: 'cache' | string;
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
  google_color_id: string | null;
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
  google_color_id?: string | null;
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
  google_color_id?: string | null;
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

// Response of GET /api/tasks/classify?title=… — the title→category match the
// create/update endpoints enforce, as a preview (never writes). Externally-
// tagged serde enum: {"Matched":{"category":{...}}} or
// {"Untracked":{"conflict":bool,"categories":[...]}}.
export type ClassifyResponse =
  | { Matched: { category: TaskCategorySummary } }
  | { Untracked: { conflict: boolean; categories: TaskCategorySummary[] } };

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

// The optional `displace` sub-object of a move: parks the currently running
// task (its landing status must be PLANNED/COMPLETED/DISCARDED — never
// OPEN/IN_PROGRESS) before the moved task starts. `sort_order` is optional:
// drops send an absolute rank; no-drop callers (modal pills, the conflict
// dialog) omit it and the server applies the column default (the park always
// prepends).
export interface MoveDisplaceInput {
  id: string;
  status: 'PLANNED' | 'COMPLETED' | 'DISCARDED';
  sort_order?: number;
}

// Request body for POST /api/tasks/:id/move — the board drop. The server
// dispatches the ADR 0002 transition matrix (start/stop/pause/complete/
// discard/plan/unplan/reopen), then places the task at `sort_order` in the
// target status. Same-status moves are reorders. `sort_order` is optional:
// drops send an absolute rank; no-drop callers (modal status pills, column+
// create-then-move, displace parks) omit it and the server applies the
// column default. `displace` is optional / null and only valid when moving
// to IN_PROGRESS.
export interface MoveTaskInput {
  status: TaskStatus;
  sort_order?: number;
  displace?: MoveDisplaceInput | null;
}

// The envelope returned by POST /api/tasks/:id/move: the moved task, the
// optionally displaced (parked) task, and the Google event the dispatched
// action touched. The move event carries extra internal cache fields; the
// board only distinguishes null/event, so it is typed loosely.
export interface MoveTaskResponse {
  task: TaskRecord;
  displaced: TaskRecord | null;
  event: CalendarEvent | null;
}

// Start failure AFTER a successful displace: no rollback — the displaced
// task stays parked, and `displaced` lets the client snap the moved card
// back. All other errors stay `{ error: string }`.
export interface MoveTaskError {
  error: string;
  displaced?: TaskRecord;
}
