export interface Task {
  id: string;
  title: string;
  duration: number;
  priority: 'high' | 'medium' | 'low';
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

// A task as returned by the API: the `tasks` row shape (snake_case) plus the
// computed `category`. `status` is driven by the timer endpoints:
// "OPEN" | "IN_PROGRESS" | "COMPLETED" | "DISCARDED".
export type TaskStatus = 'OPEN' | 'IN_PROGRESS' | 'COMPLETED' | 'DISCARDED';

export interface TaskRecord {
  id: string;
  user_id: string;
  title: string;
  description: string;
  duration_minutes: number;
  priority: TaskPriority;
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
// defaults to 15 minutes, priority to 'medium'.
export interface NewTaskInput {
  title: string;
  description?: string;
  duration_minutes?: number;
  priority?: TaskPriority;
}

// Request body for PATCH /api/tasks/:id — every field optional. A present
// `title` must uniquely match a non-untracked category. Status is never
// updatable through PATCH: use the timer endpoints (start/stop/pause/
// complete/discard) instead.
export interface UpdateTaskInput {
  title?: string;
  description?: string;
  duration_minutes?: number;
  priority?: TaskPriority;
}
