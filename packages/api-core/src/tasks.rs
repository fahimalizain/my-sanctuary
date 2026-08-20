//! Task service: CRUD for `tasks` (classified by title regex) plus the timer:
//! start/stop/pause/complete/discard backed by Google Calendar events.
//!
//! Pure Rust and unit-testable: persistence goes through [`TaskRepo`]/
//! [`TaskCategoryRepo`]/[`TaskListRepo`]/[`TaskLogRepo`]/[`CalendarRepo`]/
//! [`CalendarEventRepo`] (faked with in-memory impls in tests), Google HTTP
//! through [`HttpClient`], and "now" comes from the caller — never
//! `SystemTime`. The Worker layers session checks and token refresh on top
//! (`apps/worker/src/tasks.rs`).
//!
//! Domain rules (locked):
//! - Tasks carry NO `category_id`/`list_id`. The category is **computed** per
//!   title via [`crate::categories::classify`] with [`CalendarScope::Ignore`]:
//!   a pattern's `google_calendar_id` is a write destination, never an inbound
//!   filter, so calendar-scoped patterns match task titles on regex alone.
//! - Create/update reject a title that does not uniquely match a non-untracked
//!   category (400). `Untracked { conflict: false }` (0 matches),
//!   `Untracked { conflict: true }` (cross-tree conflict), and a match on the
//!   `untracked` sink itself are all invalid. A title matching only a root
//!   whose children do not match is **allowed** (parent remainder).
//! - Titles are not unique. Create always stores `status = "OPEN"` and
//!   **prepends Backlog**: the living OPEN rows of the user are shifted up one
//!   (`sort_order + 1`), and the new task lands at `sort_order = 0`.
//! - `duration_minutes` defaults to 15 and must be >= 1; `priority` must be
//!   `high|medium|low` (default `medium`); `difficulty` must be
//!   `easy|medium|hard` (default `easy`).
//! - Delete is SOFT; a missing/soft-deleted/other-user task is always
//!   [`TasksError::NotFound`].
//! - List returns each living task with its **computed** category
//!   ([`TaskCategorySummary`]) so the client never reimplements the regex
//!   matching. A task whose title no longer matches anything (e.g. the user
//!   deleted the pattern) is still returned with the `untracked` summary —
//!   listing is a read, not a validation.
//!
//! Timer rules (locked):
//! - `start_task` opens a Google Calendar event on the **minute grid**
//!   (`T … T + duration_minutes` where `T = nearest_minute_unix(now)`) with
//!   `extendedProperties.shared.sanctuary_task_id` = task UUID (never
//!   `private`, never a description footer). Summary is the task **title**
//!   exactly — no `| Category` suffix.
//! - Calendar pick: `wanted` is the first non-empty of the matched
//!   category's first regex-matching pattern's `google_calendar_id` (patterns
//!   walked in stored `sort_order`), the matched category's
//!   `google_calendar_id`, and the parent root's `google_calendar_id` (a
//!   child's match only — one-level tree). The started event lands on `wanted`
//!   **when that calendar exists for this user, is not soft-deleted, and is
//!   writable** (`access_role` `owner` or `writer`); a missing or read-only
//!   named calendar never falls through to the next inheritance slot — it
//!   goes to the user's **primary** calendar. No writable calendar → 400.
//! - One running task per user: a second start raises [`TasksError::Conflict`]
//!   (409) even when the same task is already running. **`tasks.status ==
//!   "IN_PROGRESS"` is the only lock** — the start gate scans the user's
//!   living tasks by status, never the event window.
//! - Every timer Google write snaps to the nearest minute: `start_task`
//!   creates at `T … T + duration_minutes`, and all exit verbs
//!   (`stop_task`/`pause_task`/`complete_task`/`discard_task`, and the
//!   displace park) PATCH the event's end to snapped `T` (`start + 60s` when
//!   `T <= start`, keeping the event valid).
//! - Exit verbs resolve the event to PATCH through the task's **latest
//!   `started` log** (`calendar_id` + `google_event_id`) — never through the
//!   running-event window — so a run whose event end already passed still
//!   closes. Missing log / empty ids / Google 404 → today's idempotent
//!   status-only flip: OPEN (stop) or PLANNED (pause), terminal status for
//!   complete/discard.
//! - `complete_task`/`discard_task` auto-stop a running event first (a
//!   `stopped` log precedes the terminal log), then set the terminal status.
//!   Repeating the same terminal action is an idempotent 200 no-op.
//! - **Nothing is terminal** (ADR 0002): start on COMPLETED/DISCARDED is
//!   allowed — a NEW calendar event opens and history stays. Stop/pause on a
//!   COMPLETED/DISCARDED task stay invalid as verbs; reopen is the path back.
//!   Missing/other-user/soft-deleted tasks are 404.
//! - Every transition appends to `task_logs` (an audit trail, not a
//!   timesheet): `started|stopped|paused|completed|discarded` plus the move
//!   verbs `planned|unplanned|reopened`.
//! - The `*/2` elongate cron ([`run_elongate_cron`]) grows every living
//!   IN_PROGRESS task's event end to `max(current, ceil-5min(now + 5min)` in
//!   the event calendar's TZ) so the live calendar block never looks
//!   finished. Never shrink (the exit verbs snap the end back to actuals on
//!   close — that may shrink and is correct), never recreate a missing
//!   event, never touch status: IN_PROGRESS stays the lock.
//!
//! Move ([`move_task`], ADR 0002 § Move API): the board drop dispatches the
//! transition matrix (start/stop/pause/complete/discard/plan/unplan/reopen/
//! reorder), then places the task at `sort_order` in the target status (peer
//! shifts never touch `updated_at`, and the source column is never compacted).
//! A same-status cross-card drag is a pure reorder. `displace` parks the
//! running task first (PLANNED/COMPLETED/DISCARDED only — `displace.id` must
//! be the task whose **status** is IN_PROGRESS), then starts the moved task;
//! if that start fails the parked task STAYS — the error is
//! [`TasksError::AfterDisplace`] and carries the displaced task's view.

use std::collections::HashMap;

use thiserror::Error;

use crate::calendar::{create_event, patch_event, CalendarError};
use crate::categories::{
    classify, classify_detailed, ensure_taxonomy, first_matching_pattern, CalendarScope,
    CategoryWithPatterns, ClassifyOutcome,
};
use crate::config::OAuthConfig;
use crate::models::{
    CalendarEvent, NewEventInput, NewTask, NewTaskInput, NewTaskLog, Task, TaskCategory,
    TaskCategoryPattern, UpdateTask,
};
use crate::oauth::HttpClient;
use crate::repo::{
    CalendarEventRepo, CalendarRepo, RepoError, TaskCategoryRepo, TaskListRepo, TaskLogRepo,
    TaskRepo, TokenRepo,
};
use crate::time::{
    ceil_5min_unix_in_zone, nearest_minute_unix, rfc3339_to_unix_secs, unix_secs_to_rfc3339,
};
use crate::token::{refresh_if_needed, GoogleAccess};

/// Default planned duration for a new task, in minutes.
pub const DEFAULT_DURATION_MINUTES: i64 = 15;
/// Minimum planned duration, in minutes.
pub const MIN_DURATION_MINUTES: i64 = 1;
/// The status every created task gets.
pub const TASK_STATUS_OPEN: &str = "OPEN";
/// A running task — **the one-running-task lock** (a second start is 409
/// while any living task carries this status, whatever the event cache says).
pub const TASK_STATUS_IN_PROGRESS: &str = "IN_PROGRESS";
/// A finished task.
pub const TASK_STATUS_COMPLETED: &str = "COMPLETED";
/// A discarded task.
pub const TASK_STATUS_DISCARDED: &str = "DISCARDED";
/// A paused task sitting in the Planned pile — `pause` lands here since the
/// board slice, not back in Backlog.
pub const TASK_STATUS_PLANNED: &str = "PLANNED";

/// `task_logs.type` values (the audit trail is append-only).
pub const TASK_LOG_STARTED: &str = "started";
pub const TASK_LOG_STOPPED: &str = "stopped";
pub const TASK_LOG_PAUSED: &str = "paused";
pub const TASK_LOG_COMPLETED: &str = "completed";
pub const TASK_LOG_DISCARDED: &str = "discarded";
/// `planned`/`unplanned`/`reopened` arrive with the move endpoint (slice 2);
/// the constants exist now so the log vocabulary is complete.
pub const TASK_LOG_PLANNED: &str = "planned";
pub const TASK_LOG_UNPLANNED: &str = "unplanned";
pub const TASK_LOG_REOPENED: &str = "reopened";

/// Errors produced by the tasks service.
///
/// No `PartialEq`/`Eq`: the `Calendar` variant wraps
/// [`CalendarError`] (which carries non-`Eq` HTTP errors) — tests match with
/// `matches!`, never `==`.
#[derive(Debug, Clone, Error)]
pub enum TasksError {
    #[error("{0}")]
    Invalid(String),
    #[error("task not found")]
    NotFound,
    #[error("a task is already running")]
    Conflict,
    #[error("google api error: {0}")]
    GoogleApi(String),
    #[error("database error: {0}")]
    Repo(#[from] RepoError),
    #[error("calendar error: {0}")]
    Calendar(CalendarError),
    /// The move already parked `displaced` (its event is closed, it sits in
    /// its new status/rank) when the subsequent start failed. Deliberately
    /// NO rollback (ADR 0002): `displaced` stays parked and the worker
    /// serializes `{"error": <inner>…, "displaced": TaskView}` so the client
    /// can snap the moved card back.
    #[error("move failed after displacing the running task")]
    AfterDisplace {
        displaced: TaskView,
        #[source]
        source: Box<TasksError>,
    },
}

impl From<CalendarError> for TasksError {
    fn from(err: CalendarError) -> Self {
        match err {
            CalendarError::GoogleApi(message) => TasksError::GoogleApi(message),
            // The timer only ever touches calendars it resolved itself, so a
            // NotFound/InvalidResponse/Http here is a 500-shaped surprise —
            // surface the message and let the worker pick the status.
            other => TasksError::Calendar(other),
        }
    }
}

/// `list_tasks`/`create_task` run `ensure_taxonomy`, whose errors fold into
/// [`TasksError`] (same HTTP mapping as the lists path: 400 / 404 / 500; a
/// taxonomy Conflict is a domain violation, so it surfaces as a 400 Invalid).
impl From<crate::categories::CategoriesError> for TasksError {
    fn from(err: crate::categories::CategoriesError) -> Self {
        match err {
            crate::categories::CategoriesError::Invalid(message) => TasksError::Invalid(message),
            crate::categories::CategoriesError::NotFound => TasksError::NotFound,
            crate::categories::CategoriesError::Conflict(message) => TasksError::Invalid(message),
            crate::categories::CategoriesError::Repo(err) => TasksError::Repo(err),
        }
    }
}

/// Response envelope for `GET /api/tasks`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TasksResponse {
    pub tasks: Vec<TaskView>,
}

/// Response envelope for `POST /api/tasks` and `PATCH /api/tasks/:id`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskResponse {
    pub task: TaskView,
}

/// Response envelope for `DELETE /api/tasks/:id`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeleteTaskResponse {
    pub success: bool,
}

/// Response envelope for the timer actions (start/stop/pause/complete/
/// discard): the fresh task view plus the Google event the action touched
/// (`None` when no event was involved, e.g. an idempotent complete or a stop
/// whose event had already been closed in Google).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskActionResponse {
    pub task: TaskView,
    pub event: Option<CalendarEvent>,
}

/// Request body for `POST /api/tasks/:id/move` (ADR 0002 § Move API): the
/// absolute `sort_order` to assign in the target `status`. `displace` is
/// optional (omitted or explicit `null`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct MoveTaskInput {
    /// One of `OPEN|PLANNED|IN_PROGRESS|COMPLETED|DISCARDED` (the service
    /// validates; unknown values are 400).
    pub status: String,
    /// The absolute rank to assign in the target status (>= 0).
    pub sort_order: i64,
    /// When set: park the running task first (must be the currently running
    /// task; its landing status must be PLANNED/COMPLETED/DISCARDED), then
    /// start the moved task.
    pub displace: Option<DisplaceInput>,
}

/// The `displace` sub-object: parks the running task at its own status/rank
/// before the moved task starts. `status` is locked to
/// `PLANNED|COMPLETED|DISCARDED` (never OPEN/IN_PROGRESS).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct DisplaceInput {
    /// Id of the currently running task (its `tasks.status` row is
    /// IN_PROGRESS — the status lock, not the event window); anything else
    /// is 400.
    pub id: String,
    pub status: String,
    pub sort_order: i64,
}

/// Response envelope for `POST /api/tasks/:id/move`: the moved task plus the
/// optionally displaced task and the Google event the dispatched action
/// touched (the same `TaskActionResponse` family, plus `displaced`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MoveTaskResponse {
    pub task: TaskView,
    pub displaced: Option<TaskView>,
    pub event: Option<CalendarEvent>,
}

/// HTTP shape of a task: every `tasks` column (minus `deleted_at`) plus the
/// **computed** category summary — the client groups tasks by `category.id`
/// without reimplementing the matcher.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskView {
    pub id: String,
    pub user_id: String,
    pub title: String,
    pub description: String,
    pub duration_minutes: i64,
    pub priority: String,
    pub difficulty: String,
    /// Per-user, per-status board rank (0 = front of the column). The frontend
    /// needs it to render columns in order and, later, to place drops.
    pub sort_order: i64,
    pub status: String,
    /// RFC 3339 instant.
    pub created_at: String,
    /// RFC 3339 instant.
    pub updated_at: String,
    pub category: TaskCategorySummary,
}

/// Slimmer category attached to a [`TaskView`]: the fields the Lists page
/// needs to show a task under its category (full `CategoryView` pulls in the
/// pattern rows, which a task list does not need).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskCategorySummary {
    pub id: String,
    pub title: String,
    pub slug: String,
    /// Stored column; `None` for children and `untracked`.
    pub list_id: Option<String>,
    /// Root's own `list_id`, or the parent root's `list_id` for children.
    pub inherited_list_id: Option<String>,
    pub is_untracked: bool,
    pub color: String,
}

/// All living categories plus the matcher set built from their patterns, in
/// one round-trip pair (list + bulk patterns) — the unit of work for every
/// classify here.
struct Taxonomy {
    categories: Vec<TaskCategory>,
    matchers: Vec<CategoryWithPatterns>,
}

/// Lists the user's living tasks with their computed category summaries.
///
/// Seeding: like `list_lists`, this runs `ensure_taxonomy` (when the user has
/// zero living lists OR zero living categories) so a tasks-only client still
/// has a matcher. The count-gate keeps the sequential first-visit order
/// (lists → categories → tasks) safe: by the time the frontend's third fetch
/// lands, taxonomy exists and this is a no-op.
///
/// A task whose title now classifies as `untracked` (e.g. the user deleted the
/// matching pattern) is still returned, with the `untracked` summary attached
/// — never a 400 from a read.
pub async fn list_tasks(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    task_repo: &dyn TaskRepo,
    user_id: &str,
) -> Result<TasksResponse, TasksError> {
    let lists = list_repo.list_by_user_id(user_id).await?;
    let categories = category_repo.list_by_user_id(user_id).await?;
    ensure_taxonomy(list_repo, category_repo, &lists, &categories, user_id).await?;
    // Re-load: the seed may have inserted lists/categories.
    let taxonomy = load_taxonomy(category_repo, user_id).await?;
    let tasks = task_repo.list_by_user_id(user_id).await?;
    let views = tasks
        .iter()
        .map(|task| to_view(task, &taxonomy))
        .collect();
    Ok(TasksResponse { tasks: views })
}

/// Response envelope for the classify endpoint. The status line states
/// drive the TaskModal: matched → "Files to ● X"; untracked(conflict=false)
/// → "No category matches — Save will fail"; untracked(conflict=true) →
/// "Matches A and B — be more specific" (categories names them).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum ClassifyResponse {
    Matched { category: TaskCategorySummary },
    /// `conflict: true` when several categories matched (names them in
    /// `categories`); `false` when nothing matched. The untracked sink
    /// can never match (it has no patterns), so it never appears here.
    Untracked { conflict: bool, categories: Vec<TaskCategorySummary> },
}

/// Classifies a title against the user's taxonomy for the modal's blur
/// preview. A read — never writes task rows.
///
/// - A blank title is 400 (same rule as create).
/// - 0 matches → `Untracked { conflict: false }`.
/// - 1 match → `Matched`, unless the match is the `untracked` sink
///   (impossible per the locked rules; checked defensively like `to_view`)
///   — then `Untracked { conflict: false }`.
/// - 2+ matches → `Untracked { conflict: true }` naming every match.
pub async fn classify_title(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
    title: &str,
) -> Result<ClassifyResponse, TasksError> {
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err(TasksError::Invalid("title must not be empty".to_string()));
    }
    // Same count-gated load as create: seed on first visit, then read.
    let taxonomy = load_taxonomy_seeded(list_repo, category_repo, user_id).await?;
    let detail = classify_detailed(&title, CalendarScope::Ignore, &taxonomy.matchers);
    let response = match detail.matched.len() {
        0 => ClassifyResponse::Untracked {
            conflict: false,
            categories: Vec::new(),
        },
        1 => {
            let id = &detail.matched[0];
            let category = taxonomy
                .categories
                .iter()
                .find(|category| &category.id == id);
            match category {
                Some(category) if !category.is_untracked => ClassifyResponse::Matched {
                    category: summary_for(category, &taxonomy.categories),
                },
                // The sink (or a vanished category — impossible in one run,
                // but never panic on a read): report no match.
                _ => ClassifyResponse::Untracked {
                    conflict: false,
                    categories: Vec::new(),
                },
            }
        }
        _ => ClassifyResponse::Untracked {
            conflict: true,
            categories: detail
                .matched
                .iter()
                .filter_map(|id| {
                    taxonomy
                        .categories
                        .iter()
                        .find(|category| &category.id == id)
                        // The sink cannot match (no patterns); skip it
                        // defensively so a conflict never names it.
                        .filter(|category| !category.is_untracked)
                        .map(|category| summary_for(category, &taxonomy.categories))
                })
                .collect(),
        },
    };
    Ok(response)
}

/// Creates a task (always `status = "OPEN"`).
///
/// Validation (all 400):
/// - `title` must not be blank, and must classify to a single non-untracked
///   category: 0 matches → `title does not match a category`, several matches
///   → `title matches multiple categories`, a match on the `untracked` sink →
///   `title matches untracked`.
/// - `duration_minutes` defaults to [`DEFAULT_DURATION_MINUTES`] and must be
///   at least [`MIN_DURATION_MINUTES`].
/// - `priority` defaults to `medium` and must be `high|medium|low`.
/// - `difficulty` defaults to `easy` and must be `easy|medium|hard`.
///
/// `ensure_taxonomy` runs first so the very first task of a fresh user finds
/// a seeded matcher.
///
/// Ordering: the new task **prepends Backlog** — the user's living OPEN rows
/// are shifted up by one (`sort_order >= 0`), then the task is inserted at
/// `sort_order = 0`.
pub async fn create_task(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    task_repo: &dyn TaskRepo,
    user_id: &str,
    input: &NewTaskInput,
) -> Result<TaskResponse, TasksError> {
    let title = input.title.trim().to_string();
    if title.is_empty() {
        return Err(TasksError::Invalid("title must not be empty".to_string()));
    }
    let duration_minutes = input.duration_minutes.unwrap_or(DEFAULT_DURATION_MINUTES);
    if duration_minutes < MIN_DURATION_MINUTES {
        return Err(TasksError::Invalid("duration must be at least 1 minute".to_string()));
    }
    let priority = input
        .priority
        .as_deref()
        .unwrap_or("medium")
        .to_string();
    if !is_valid_priority(&priority) {
        return Err(TasksError::Invalid(
            "priority must be one of high, medium, low".to_string(),
        ));
    }
    let difficulty = input
        .difficulty
        .as_deref()
        .unwrap_or("easy")
        .to_string();
    if !is_valid_difficulty(&difficulty) {
        return Err(TasksError::Invalid(
            "difficulty must be one of easy, medium, hard".to_string(),
        ));
    }

    let lists = list_repo.list_by_user_id(user_id).await?;
    let categories = category_repo.list_by_user_id(user_id).await?;
    ensure_taxonomy(list_repo, category_repo, &lists, &categories, user_id).await?;
    let taxonomy = load_taxonomy(category_repo, user_id).await?;
    // Validates the rules; the response view re-classifies the stored title.
    resolve_category(&title, &taxonomy)?;

    // Prepend Backlog: every living OPEN row of the user moves up one so the
    // new task can rank 0. Peers keep their `updated_at` — re-ranking is not
    // a content change.
    task_repo
        .shift_sort_order(user_id, TASK_STATUS_OPEN, 0)
        .await?;
    let task = task_repo
        .insert(NewTask {
            user_id: user_id.to_string(),
            title,
            description: input.description.as_deref().unwrap_or("").trim().to_string(),
            duration_minutes,
            priority,
            difficulty,
            sort_order: 0,
        })
        .await?;
    Ok(TaskResponse {
        task: to_view(&task, &taxonomy),
    })
}

/// Updates a task's `title`/`description`/`duration_minutes`/`priority`/
/// `difficulty` (`None` = unchanged; status is never updated this slice).
///
/// - A body with nothing to update is 400.
/// - When `title` is present it must be non-blank AND uniquely match a
///   non-untracked category (same rules as create, same messages).
/// - `duration_minutes` must be >= 1 when present; `priority` must be
///   `high|medium|low` when present; `difficulty` must be
///   `easy|medium|hard` when present.
/// - A missing, soft-deleted, or another user's task is 404.
pub async fn update_task(
    category_repo: &dyn TaskCategoryRepo,
    task_repo: &dyn TaskRepo,
    user_id: &str,
    id: &str,
    updates: &UpdateTask,
) -> Result<TaskResponse, TasksError> {
    if updates.title.is_none()
        && updates.description.is_none()
        && updates.duration_minutes.is_none()
        && updates.priority.is_none()
        && updates.difficulty.is_none()
    {
        return Err(TasksError::Invalid("nothing to update".to_string()));
    }
    if let Some(duration_minutes) = updates.duration_minutes {
        if duration_minutes < MIN_DURATION_MINUTES {
            return Err(TasksError::Invalid("duration must be at least 1 minute".to_string()));
        }
    }
    if let Some(priority) = updates.priority.as_deref() {
        if !is_valid_priority(priority) {
            return Err(TasksError::Invalid(
                "priority must be one of high, medium, low".to_string(),
            ));
        }
    }
    if let Some(difficulty) = updates.difficulty.as_deref() {
        if !is_valid_difficulty(difficulty) {
            return Err(TasksError::Invalid(
                "difficulty must be one of easy, medium, hard".to_string(),
            ));
        }
    }

    let Some(task) = task_repo.get_by_id(id).await? else {
        return Err(TasksError::NotFound);
    };
    // `get_by_id` is intentionally not user-scoped; ownership is checked here
    // so another user's task is a plain 404, never a leak.
    if task.user_id != user_id {
        return Err(TasksError::NotFound);
    }

    // A new title must classify to a single non-untracked category, exactly
    // like create. The old title needs no re-validation.
    if let Some(title) = updates.title.as_deref() {
        let title = title.trim();
        if title.is_empty() {
            return Err(TasksError::Invalid("title must not be empty".to_string()));
        }
        let taxonomy = load_taxonomy(category_repo, user_id).await?;
        resolve_category(title, &taxonomy)?;
    }

    let Some(updated) = task_repo.update(id, updates).await? else {
        // Deleted between the read and the write.
        return Err(TasksError::NotFound);
    };
    let taxonomy = load_taxonomy(category_repo, user_id).await?;
    Ok(TaskResponse {
        task: to_view(&updated, &taxonomy),
    })
}

/// SOFT deletes a task.
///
/// - Missing, soft-deleted, or another user's task → [`TasksError::NotFound`]
///   (404) — ownership is never leaked.
/// - Otherwise the row is stamped with `deleted_at = now_rfc3339` and
///   `{"success": true}` is returned.
pub async fn delete_task(
    task_repo: &dyn TaskRepo,
    user_id: &str,
    id: &str,
    now_rfc3339: &str,
) -> Result<DeleteTaskResponse, TasksError> {
    let Some(task) = task_repo.get_by_id(id).await? else {
        return Err(TasksError::NotFound);
    };
    if task.user_id != user_id {
        return Err(TasksError::NotFound);
    }
    task_repo.soft_delete(id, now_rfc3339).await?;
    Ok(DeleteTaskResponse { success: true })
}

// ──────────────────────────────────────────
// Timer (slice 4): start / stop / pause / complete / discard
// ──────────────────────────────────────────

/// Starts a task: opens a timed Google Calendar event and marks the task
/// IN_PROGRESS. The event's summary is the task **title** exactly and carries
/// `extendedProperties.shared.sanctuary_task_id` = task UUID.
///
/// **Nothing is terminal** (ADR 0002): starting a COMPLETED/DISCARDED task is
/// allowed — a NEW event opens for the new chapter and the old logs/events
/// stay. Start on PLANNED was always allowed, so a paused task restarts.
///
/// Order of operations:
/// 1. Load the task — missing, soft-deleted, or another user's task → 404.
/// 2. If ANY living task of the user has `status == IN_PROGRESS` → 409
///    (`Conflict`) — even when it is this very task. **Status is the only
///    lock**: the event window is never consulted here.
/// 3. Classify the title (seeding the taxonomy like every other read) to pick
///    the category.
/// 4. Resolve the target calendar: `wanted` is the first non-empty of the
///    first regex-matching pattern's `google_calendar_id` (in stored
///    `sort_order`), the matched category's `google_calendar_id`, the parent
///    root's `google_calendar_id`. Use `wanted` when that calendar exists for
///    this user and is writable (`access_role` owner/writer); a missing or
///    read-only named calendar goes to the user's primary calendar, never to
///    the next inheritance slot. No writable calendar → 400.
/// 5. `create_event` on the minute grid (`T … T + duration_minutes`,
///    `T = nearest_minute_unix(now)`, task carrier attached).
/// 6. `tasks.status` → IN_PROGRESS and a `started` log row, both in this
///    request.
pub async fn start_task(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    task_repo: &dyn TaskRepo,
    logs: &dyn TaskLogRepo,
    access: &GoogleAccess,
    user_id: &str,
    task_id: &str,
    now_unix: i64,
) -> Result<TaskActionResponse, TasksError> {
    let Some(task) = task_repo.get_by_id(task_id).await? else {
        return Err(TasksError::NotFound);
    };
    if task.user_id != user_id {
        return Err(TasksError::NotFound);
    }
    // No status gate: start on COMPLETED/DISCARDED opens a NEW event (the
    // board's reopen-by-start, ADR 0002) — history stays in the logs.

    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    // The one-running-task lock is `tasks.status == IN_PROGRESS` — never the
    // event window, so a stale/expired cache can neither block nor free a
    // start.
    let living = task_repo.list_by_user_id(user_id).await?;
    if living.iter().any(|task| task.status == TASK_STATUS_IN_PROGRESS) {
        return Err(TasksError::Conflict);
    }

    let taxonomy = load_taxonomy_seeded(list_repo, category_repo, user_id).await?;
    let target = resolve_target_calendar(calendars, &taxonomy, &task.title, user_id).await?;

    // Timer Google writes snap to the nearest minute.
    let t_unix = nearest_minute_unix(now_unix);
    let start_rfc3339 = unix_secs_to_rfc3339(t_unix);
    let end_rfc3339 = unix_secs_to_rfc3339(t_unix + task.duration_minutes * 60);
    let output = create_event(
        http,
        calendars,
        events,
        access,
        &NewEventInput {
            calendar_id: target.calendar_id.clone(),
            summary: task.title.clone(),
            description: None,
            start: start_rfc3339,
            end: end_rfc3339,
            task_id: Some(task.id.clone()),
            color_id: target.google_color_id,
        },
        now_unix,
    )
    .await?;
    let event = output.event;

    let Some(updated) = task_repo
        .set_status(&task.id, TASK_STATUS_IN_PROGRESS, &now_rfc3339)
        .await?
    else {
        return Err(TasksError::NotFound);
    };
    logs.insert(
        NewTaskLog {
            task_id: task.id.clone(),
            user_id: user_id.to_string(),
            r#type: TASK_LOG_STARTED.to_string(),
            at: now_rfc3339.clone(),
            calendar_id: Some(target.calendar_id),
            google_event_id: Some(event.google_event_id.clone()),
        },
        &now_rfc3339,
    )
    .await?;

    Ok(TaskActionResponse {
        task: to_view(&updated, &taxonomy),
        event: Some(event),
    })
}

/// Stops a task: PATCHes its open event's end to now (`start + 60s` when
/// `now <= start`) and flips status back to OPEN. The status flip is
/// idempotent — when the user already closed the event in Google, stop only
/// rewrites the status and appends a `stopped` log.
pub async fn stop_task(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    category_repo: &dyn TaskCategoryRepo,
    task_repo: &dyn TaskRepo,
    logs: &dyn TaskLogRepo,
    access: &GoogleAccess,
    user_id: &str,
    task_id: &str,
    now_unix: i64,
) -> Result<TaskActionResponse, TasksError> {
    stop_or_pause(
        http, calendars, events, category_repo, task_repo, logs, access, user_id, task_id,
        now_unix, TASK_LOG_STOPPED, TASK_STATUS_OPEN,
    )
    .await
}

/// Pauses a task: identical to [`stop_task`] (the event's end is patched to
/// now) except the log row says `paused` and the status lands **PLANNED**,
/// not OPEN (ADR 0002: pause parks the task in the Planned pile). Reopening
/// later is simply Start again — start already allows PLANNED (logged
/// `started`).
pub async fn pause_task(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    category_repo: &dyn TaskCategoryRepo,
    task_repo: &dyn TaskRepo,
    logs: &dyn TaskLogRepo,
    access: &GoogleAccess,
    user_id: &str,
    task_id: &str,
    now_unix: i64,
) -> Result<TaskActionResponse, TasksError> {
    stop_or_pause(
        http, calendars, events, category_repo, task_repo, logs, access, user_id, task_id,
        now_unix, TASK_LOG_PAUSED, TASK_STATUS_PLANNED,
    )
    .await
}

/// Completes a task: auto-stops a running event first (a `stopped` log
/// precedes the `completed` log), then sets COMPLETED. Already COMPLETED is
/// an idempotent 200 no-op.
pub async fn complete_task(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    category_repo: &dyn TaskCategoryRepo,
    task_repo: &dyn TaskRepo,
    logs: &dyn TaskLogRepo,
    access: &GoogleAccess,
    user_id: &str,
    task_id: &str,
    now_unix: i64,
) -> Result<TaskActionResponse, TasksError> {
    complete_or_discard(
        http, calendars, events, category_repo, task_repo, logs, access, user_id, task_id,
        now_unix, TASK_STATUS_COMPLETED, TASK_LOG_COMPLETED,
    )
    .await
}

/// Discards a task: auto-stops a running event first (a `stopped` log
/// precedes the `discarded` log), then sets DISCARDED. Already DISCARDED is
/// an idempotent 200 no-op.
pub async fn discard_task(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    category_repo: &dyn TaskCategoryRepo,
    task_repo: &dyn TaskRepo,
    logs: &dyn TaskLogRepo,
    access: &GoogleAccess,
    user_id: &str,
    task_id: &str,
    now_unix: i64,
) -> Result<TaskActionResponse, TasksError> {
    complete_or_discard(
        http, calendars, events, category_repo, task_repo, logs, access, user_id, task_id,
        now_unix, TASK_STATUS_DISCARDED, TASK_LOG_DISCARDED,
    )
    .await
}

/// Shared stop/pause machinery (see [`stop_task`]).
///
/// `target_status` is the landing status: OPEN for stop, PLANNED for pause
/// (mirrors `complete_or_discard`, where the caller picks the terminal state).
async fn stop_or_pause(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    category_repo: &dyn TaskCategoryRepo,
    task_repo: &dyn TaskRepo,
    logs: &dyn TaskLogRepo,
    access: &GoogleAccess,
    user_id: &str,
    task_id: &str,
    now_unix: i64,
    log_type: &str,
    target_status: &str,
) -> Result<TaskActionResponse, TasksError> {
    let Some(task) = task_repo.get_by_id(task_id).await? else {
        return Err(TasksError::NotFound);
    };
    if task.user_id != user_id {
        return Err(TasksError::NotFound);
    }
    // Terminal tasks are never reopened — not even by stop/pause.
    if task.status == TASK_STATUS_COMPLETED || task.status == TASK_STATUS_DISCARDED {
        return Err(TasksError::Invalid(format!(
            "cannot {log_type} a {status} task",
            status = task.status
        )));
    }

    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    let (patched, at, calendar_id, google_event_id) =
        stop_running_event(http, calendars, events, logs, access, task_id, now_unix).await?;

    let Some(updated) = task_repo
        .set_status(task_id, target_status, &now_rfc3339)
        .await?
    else {
        return Err(TasksError::NotFound);
    };
    logs.insert(
        NewTaskLog {
            task_id: task_id.to_string(),
            user_id: user_id.to_string(),
            r#type: log_type.to_string(),
            at,
            calendar_id,
            google_event_id,
        },
        &now_rfc3339,
    )
    .await?;

    let taxonomy = load_taxonomy(category_repo, user_id).await?;
    Ok(TaskActionResponse {
        task: to_view(&updated, &taxonomy),
        event: patched,
    })
}

/// Shared complete/discard machinery (see [`complete_task`]).
async fn complete_or_discard(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    category_repo: &dyn TaskCategoryRepo,
    task_repo: &dyn TaskRepo,
    logs: &dyn TaskLogRepo,
    access: &GoogleAccess,
    user_id: &str,
    task_id: &str,
    now_unix: i64,
    target_status: &str,
    log_type: &str,
) -> Result<TaskActionResponse, TasksError> {
    let Some(task) = task_repo.get_by_id(task_id).await? else {
        return Err(TasksError::NotFound);
    };
    if task.user_id != user_id {
        return Err(TasksError::NotFound);
    }
    // Idempotent 200: the terminal action already happened — no Google call,
    // no log row, just the current state.
    if task.status == target_status {
        let taxonomy = load_taxonomy(category_repo, user_id).await?;
        return Ok(TaskActionResponse {
            task: to_view(&task, &taxonomy),
            event: None,
        });
    }

    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    // Auto-stop: a running event is closed first, and that close is logged
    // (`stopped`) before the terminal log.
    let (patched, at, calendar_id, google_event_id) =
        stop_running_event(http, calendars, events, logs, access, task_id, now_unix).await?;
    if let Some(_event) = &patched {
        logs.insert(
            NewTaskLog {
                task_id: task_id.to_string(),
                user_id: user_id.to_string(),
                r#type: TASK_LOG_STOPPED.to_string(),
                at,
                calendar_id,
                google_event_id,
            },
            &now_rfc3339,
        )
        .await?;
    }

    let Some(updated) = task_repo
        .set_status(task_id, target_status, &now_rfc3339)
        .await?
    else {
        return Err(TasksError::NotFound);
    };
    logs.insert(
        NewTaskLog {
            task_id: task_id.to_string(),
            user_id: user_id.to_string(),
            r#type: log_type.to_string(),
            at: now_rfc3339.clone(),
            calendar_id: None,
            google_event_id: None,
        },
        &now_rfc3339,
    )
    .await?;

    let taxonomy = load_taxonomy(category_repo, user_id).await?;
    Ok(TaskActionResponse {
        task: to_view(&updated, &taxonomy),
        event: patched,
    })
}

/// Resolves the cached event a task's latest `started` log points at, for the
/// exit verbs and the displace park: `None` when the task never started, the
/// log carries no ids, or the cached row is gone (the user closed the event
/// in Google).
async fn latest_started_event(
    logs: &dyn TaskLogRepo,
    events: &dyn CalendarEventRepo,
    task_id: &str,
) -> Result<Option<CalendarEvent>, RepoError> {
    let Some(log) = logs.latest_started_by_task_id(task_id).await? else {
        return Ok(None);
    };
    if log.calendar_id.is_empty() || log.google_event_id.is_empty() {
        return Ok(None);
    }
    events
        .get_by_calendar_and_google_id(&log.calendar_id, &log.google_event_id)
        .await
}

/// Closes the task's run: PATCHes the end of the event its latest `started`
/// log points at to snapped `T` (or `start + 60s` when `T <= start`, keeping
/// the Google event valid — the invert guard, on the minute grid). Returns
/// the patched event (None on the idempotent no-event path), the closing
/// instant, and the calendar/google ids for the log row.
///
/// The event is resolved through the **log**, never through
/// `list_running_by_user_id`, so a run whose event window already lapsed
/// still closes. A missing log / empty ids / missing cached row / Google 404
/// all land on the no-event path: the status flip still happens.
async fn stop_running_event(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    logs: &dyn TaskLogRepo,
    access: &GoogleAccess,
    task_id: &str,
    now_unix: i64,
) -> Result<(Option<CalendarEvent>, String, Option<String>, Option<String>), TasksError> {
    let snapped_rfc3339 = unix_secs_to_rfc3339(nearest_minute_unix(now_unix));
    let Some(found) = latest_started_event(logs, events, task_id).await? else {
        return Ok((None, snapped_rfc3339, None, None));
    };
    let start_unix = rfc3339_to_unix_secs(&found.start_time).unwrap_or(now_unix);
    let snapped = nearest_minute_unix(now_unix);
    let end_unix = if snapped <= start_unix {
        start_unix + 60
    } else {
        snapped
    };
    let end_rfc3339 = unix_secs_to_rfc3339(end_unix);
    let output = match patch_event(
        http,
        calendars,
        events,
        access,
        &found.calendar_id,
        &found.google_event_id,
        &end_rfc3339,
        now_unix,
    )
    .await
    {
        Ok(output) => output,
        // The event is gone on Google's side (404): fall through to the
        // idempotent status-only flip — never fail a stop/complete on a
        // ghost event.
        Err(CalendarError::GoogleApi(message)) if message.contains("404") => {
            return Ok((None, snapped_rfc3339, None, None));
        }
        Err(err) => return Err(TasksError::from(err)),
    };
    Ok((
        Some(output.event),
        end_rfc3339,
        Some(found.calendar_id),
        Some(found.google_event_id),
    ))
}

// ──────────────────────────────────────────
// Elongate cron (slice 2): grow IN_PROGRESS events every 2 minutes
// ──────────────────────────────────────────

/// Outcome of one elongate cron run: counters plus human-readable failures —
/// a failure for one task never fails the whole job.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ElongateReport {
    /// Tasks whose Google event end was extended in this run.
    pub elongated: usize,
    /// Tasks skipped: no started log / empty ids / missing cache row, an
    /// unparseable stored end, Google 404 (the event is gone), or a current
    /// end already at/after the target (never shrink).
    pub skipped: usize,
    /// Human-readable failures; empty when everything worked.
    pub errors: Vec<String>,
}

/// The elongate cron (triggered by the `*/2 * * * *` schedule): while a task
/// stays IN_PROGRESS, grow its Google event so the live calendar block does
/// not look finished.
///
/// Target: `end = max(current_end, ceil_5min(now + 5min)` in the event
/// calendar's IANA time zone), persisted as a UTC `…Z` string via
/// `unix_secs_to_rfc3339` — the same persistence as every other timer write.
///
/// Per task, in order:
/// 1. `refresh_if_needed` for the task's owner. On failure the task is
///    skipped entirely (one error) — other users still elongate.
/// 2. Resolve the cached event through the task's latest `started` log
///    (`latest_started_event`, the same path as the exit verbs). No log /
///    empty ids / missing cache row → skipped: the event is gone, never
///    recreate it, never flip status.
/// 3. Parse `event.end_time` (handles `+05:30` cache rows). Unparseable →
///    skipped.
/// 4. Load the event's calendar for `time_zone`; a missing calendar or an
///    empty/unknown zone falls back to UTC.
/// 5. PATCH only when `target > current_end` (never shrink). A Google 404 →
///    skipped (the event vanished; never recreate). Other errors are
///    collected and the loop continues.
///
/// Status is deliberately never touched here: IN_PROGRESS rows are the only
/// work list, and only the exit verbs own the status flip.
pub async fn run_elongate_cron(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    logs: &dyn TaskLogRepo,
    tasks: &dyn TaskRepo,
    tokens: &dyn TokenRepo,
    oauth: &OAuthConfig,
    now_unix: i64,
) -> ElongateReport {
    let mut report = ElongateReport::default();
    let running = match tasks.list_in_progress().await {
        Ok(running) => running,
        Err(err) => {
            report
                .errors
                .push(format!("list_in_progress failed: {err}"));
            return report;
        }
    };

    for task in &running {
        let access = match refresh_if_needed(http, tokens, oauth, &task.user_id, now_unix).await {
            Ok(access) => access,
            Err(err) => {
                report.errors.push(format!(
                    "token refresh failed for user {} (task {}): {err}",
                    task.user_id, task.id
                ));
                continue;
            }
        };
        // The event a run is attached to, resolved through the started log —
        // a missing log/ids/cache row means there is nothing to grow: skip
        // (do not recreate the event, do not flip status).
        let event = match latest_started_event(logs, events, &task.id).await {
            Ok(Some(event)) => event,
            Ok(None) => {
                report.skipped += 1;
                continue;
            }
            Err(err) => {
                report.errors.push(format!(
                    "started-log lookup failed for task {}: {err}",
                    task.id
                ));
                continue;
            }
        };
        let Some(current_end_unix) = rfc3339_to_unix_secs(&event.end_time) else {
            report.skipped += 1;
            continue;
        };
        // The calendar's IANA time_zone decides the 5-minute grid (the offset
        // resolver falls back to UTC for empty/missing/unknown zones).
        let time_zone = match calendars.get_by_id(&event.calendar_id).await {
            Ok(Some(cal)) => cal.time_zone,
            Ok(None) => "UTC".to_string(),
            Err(err) => {
                report.errors.push(format!(
                    "calendar lookup failed for task {} (calendar {}): {err}",
                    task.id, event.calendar_id
                ));
                "UTC".to_string()
            }
        };
        let target_unix = ceil_5min_unix_in_zone(now_unix, &time_zone);
        if current_end_unix >= target_unix {
            // Never shrink: the event already covers the target instant.
            report.skipped += 1;
            continue;
        }
        match patch_event(
            http,
            calendars,
            events,
            &access,
            &event.calendar_id,
            &event.google_event_id,
            &unix_secs_to_rfc3339(target_unix),
            now_unix,
        )
        .await
        {
            Ok(_) => report.elongated += 1,
            // The event is gone on Google's side (404): skip — never
            // recreate it, never touch the status.
            Err(CalendarError::GoogleApi(message)) if message.contains("404") => {
                report.skipped += 1;
            }
            Err(err) => report.errors.push(format!(
                "elongate failed for task {} (event {}): {err}",
                task.id, event.google_event_id
            )),
        }
    }
    report
}

// ──────────────────────────────────────────
// Move (ADR 0002 § Move API): POST /api/tasks/:id/move
// ──────────────────────────────────────────

/// Validates an incoming `MoveTaskInput` target status (unknown → 400).
fn is_valid_task_status(status: &str) -> bool {
    matches!(
        status,
        TASK_STATUS_OPEN
            | TASK_STATUS_PLANNED
            | TASK_STATUS_IN_PROGRESS
            | TASK_STATUS_COMPLETED
            | TASK_STATUS_DISCARDED
    )
}

/// The move endpoint's Google gate (mirrors the worker's `needs_google`):
/// start, any exit from IN_PROGRESS, and the displace park all PATCH/create
/// Google events and require a refreshable token. The session-only
/// transitions (plan/unplan/reopen/reorder/idle complete/discard) must be
/// able to run with both `None`.
fn require_google<'a>(
    http: Option<&'a dyn HttpClient>,
    access: Option<&'a GoogleAccess>,
) -> Result<(&'a dyn HttpClient, &'a GoogleAccess), TasksError> {
    match (http, access) {
        (Some(http), Some(access)) => Ok((http, access)),
        // The worker gate decides `needs_google` from the same matrix; this
        // is a "should not happen" backstop, not a user-facing path.
        _ => Err(TasksError::Invalid(
            "google access required for this move".to_string(),
        )),
    }
}

/// Applies the ADR 0002 transition matrix for `from → to` on `task_id` and
/// returns the Google event the action touched (`None` for the status-only
/// transitions). Reuses the existing timer verbs (start/stop/pause/complete/
/// discard) so the Google writes stay in one place; plan/unplan/reopen and
/// idle complete/discard are pure local transitions + audit logs.
///
/// `http`/`access` are only consumed by the Google-touching arms; the
/// session-only arms work with both `None` (see [`require_google`]).
async fn dispatch_matrix_action(
    http: Option<&dyn HttpClient>,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    task_repo: &dyn TaskRepo,
    logs: &dyn TaskLogRepo,
    access: Option<&GoogleAccess>,
    user_id: &str,
    task_id: &str,
    from: &str,
    to: &str,
    now_unix: i64,
) -> Result<Option<CalendarEvent>, TasksError> {
    // `to == IN_PROGRESS` is always start — the IN_PROGRESS → IN_PROGRESS
    // no-op is short-circuited by the caller, so `from` is never
    // IN_PROGRESS here.
    match (from, to) {
        (_, TASK_STATUS_IN_PROGRESS) => {
            let (http, access) = require_google(http, access)?;
            start_task(
                http, calendars, events, list_repo, category_repo, task_repo, logs, access,
                user_id, task_id, now_unix,
            )
            .await
            .map(|response| response.event)
        }
        (TASK_STATUS_IN_PROGRESS, TASK_STATUS_OPEN) => {
            let (http, access) = require_google(http, access)?;
            stop_task(
                http, calendars, events, category_repo, task_repo, logs, access, user_id,
                task_id, now_unix,
            )
            .await
            .map(|response| response.event)
        }
        (TASK_STATUS_IN_PROGRESS, TASK_STATUS_PLANNED) => {
            let (http, access) = require_google(http, access)?;
            pause_task(
                http, calendars, events, category_repo, task_repo, logs, access, user_id,
                task_id, now_unix,
            )
            .await
            .map(|response| response.event)
        }
        (TASK_STATUS_IN_PROGRESS, TASK_STATUS_COMPLETED) => {
            let (http, access) = require_google(http, access)?;
            complete_task(
                http, calendars, events, category_repo, task_repo, logs, access, user_id,
                task_id, now_unix,
            )
            .await
            .map(|response| response.event)
        }
        (TASK_STATUS_IN_PROGRESS, TASK_STATUS_DISCARDED) => {
            let (http, access) = require_google(http, access)?;
            discard_task(
                http, calendars, events, category_repo, task_repo, logs, access, user_id,
                task_id, now_unix,
            )
            .await
            .map(|response| response.event)
        }
        // plan / unplan / reopen: status-only flips + audit logs, no Google.
        (TASK_STATUS_OPEN, TASK_STATUS_PLANNED) => {
            local_transition(
                task_repo, logs, user_id, task_id, TASK_STATUS_PLANNED, TASK_LOG_PLANNED,
                now_unix,
            )
            .await?;
            Ok(None)
        }
        (TASK_STATUS_PLANNED, TASK_STATUS_OPEN) => {
            local_transition(
                task_repo, logs, user_id, task_id, TASK_STATUS_OPEN, TASK_LOG_UNPLANNED,
                now_unix,
            )
            .await?;
            Ok(None)
        }
        (TASK_STATUS_COMPLETED | TASK_STATUS_DISCARDED, TASK_STATUS_OPEN | TASK_STATUS_PLANNED) => {
            local_transition(task_repo, logs, user_id, task_id, to, TASK_LOG_REOPENED, now_unix)
                .await?;
            Ok(None)
        }
        // complete / discard from any non-running status: no Google writes.
        (_, TASK_STATUS_COMPLETED) => {
            terminal_transition(
                http, calendars, events, category_repo, task_repo, logs, access, user_id,
                task_id, TASK_STATUS_COMPLETED, TASK_LOG_COMPLETED, now_unix,
            )
            .await
        }
        (_, TASK_STATUS_DISCARDED) => {
            terminal_transition(
                http, calendars, events, category_repo, task_repo, logs, access, user_id,
                task_id, TASK_STATUS_DISCARDED, TASK_LOG_DISCARDED, now_unix,
            )
            .await
        }
        // The matrix's same-status cells are no-ops/reorders; reorders are
        // handled by the caller, and the only same-status dispatch that can
        // reach here is the displace park when the running task's stored
        // status already equals its landing status (stale cache) — a no-op.
        (from, to) if from == to => Ok(None),
        // Every other cell of the 5×5 matrix is dispatched above; this is
        // unreachable by construction.
        (from, to) => Err(TasksError::Invalid(format!(
            "unexpected move transition: {from} → {to}"
        ))),
    }
}

/// A pure local status flip + audit log (no Google writes) — the
/// plan/unplan/reopen/idle-complete/discard legs of the matrix.
async fn local_transition(
    task_repo: &dyn TaskRepo,
    logs: &dyn TaskLogRepo,
    user_id: &str,
    task_id: &str,
    to_status: &str,
    log_type: &str,
    now_unix: i64,
) -> Result<(), TasksError> {
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    task_repo
        .set_status(task_id, to_status, &now_rfc3339)
        .await?
        .ok_or(TasksError::NotFound)?;
    logs.insert(
        NewTaskLog {
            task_id: task_id.to_string(),
            user_id: user_id.to_string(),
            r#type: log_type.to_string(),
            at: now_rfc3339.clone(),
            calendar_id: None,
            google_event_id: None,
        },
        &now_rfc3339,
    )
    .await?;
    Ok(())
}

/// Complete/discard from a status other than IN_PROGRESS: no Google writes
/// (set status + terminal log only — idle complete/discard stay local).
///
/// Status is the lock: only a task whose **status** is IN_PROGRESS has a
/// living timer, so a leftover event window in the cache never triggers a
/// Google auto-stop here. (The matrix already routes true IN_PROGRESS →
/// COMPLETED/DISCARDED to `complete_task`/`discard_task`; this arm is a
/// backstop only.)
async fn terminal_transition(
    http: Option<&dyn HttpClient>,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    category_repo: &dyn TaskCategoryRepo,
    task_repo: &dyn TaskRepo,
    logs: &dyn TaskLogRepo,
    access: Option<&GoogleAccess>,
    user_id: &str,
    task_id: &str,
    target_status: &str,
    log_type: &str,
    now_unix: i64,
) -> Result<Option<CalendarEvent>, TasksError> {
    // Status is the lock: a non-IN_PROGRESS row has no living timer, even if
    // a stale event window lingers in the cache — no Google writes.
    let Some(task) = task_repo.get_by_id(task_id).await? else {
        return Err(TasksError::NotFound);
    };
    if task.status == TASK_STATUS_IN_PROGRESS {
        let (http, access) = require_google(http, access)?;
        let response = if target_status == TASK_STATUS_COMPLETED {
            complete_task(
                http, calendars, events, category_repo, task_repo, logs, access, user_id,
                task_id, now_unix,
            )
            .await?
        } else {
            discard_task(
                http, calendars, events, category_repo, task_repo, logs, access, user_id,
                task_id, now_unix,
            )
            .await?
        };
        return Ok(response.event);
    }
    local_transition(task_repo, logs, user_id, task_id, target_status, log_type, now_unix).await?;
    Ok(None)
}

/// Cross-column (or first-insert-into-a-column) placement: shift the target
/// status's living peers with `sort_order >=` the insert up by one, then set
/// the task's rank. The source column is deliberately NOT compacted (gaps
/// are fine). Returns the freshly placed row.
async fn place_at(
    task_repo: &dyn TaskRepo,
    user_id: &str,
    task_id: &str,
    target_status: &str,
    sort_order: i64,
) -> Result<Task, TasksError> {
    task_repo
        .shift_sort_order(user_id, target_status, sort_order)
        .await?;
    task_repo
        .set_sort_order(task_id, sort_order)
        .await?
        .ok_or(TasksError::NotFound)
}

/// Same-column reorder: the card already occupies `task.sort_order`; move it
/// to `new_sort_order`.
/// - `new == old` → no-op, the row is returned unchanged.
/// - `new < old` (toward the front): peers with `new <= rank < old` shift up.
/// - `new > old` (toward the back): peers with `old < rank <= new` shift down.
///
/// The moving card is outside its own shift range in both directions, so the
/// two statements never touch it.
async fn reorder_in_place(
    task_repo: &dyn TaskRepo,
    user_id: &str,
    task: &Task,
    new_sort_order: i64,
) -> Result<Task, TasksError> {
    let old = task.sort_order;
    if new_sort_order == old {
        return Ok(task.clone());
    }
    let status = &task.status;
    if new_sort_order < old {
        task_repo
            .shift_sort_order_by(user_id, status, new_sort_order, old - 1, 1)
            .await?;
    } else {
        task_repo
            .shift_sort_order_by(user_id, status, old + 1, new_sort_order, -1)
            .await?;
    }
    task_repo
        .set_sort_order(&task.id, new_sort_order)
        .await?
        .ok_or(TasksError::NotFound)
}

/// `POST /api/tasks/:id/move` — the board drop (ADR 0002 § Move API).
///
/// Dispatches the transition matrix for `task.status → input.status`
/// (reusing `start_task`/`stop_task`/`pause_task`/`complete_task`/
/// `discard_task` for the Google-touching legs, with plan/unplan/reopen
/// as local flips), then places the task at `input.sort_order` in the target
/// status. Same-status moves are pure reorders (the IN_PROGRESS → IN_PROGRESS
/// case is a no-op that ignores `sort_order`).
///
/// `displace` (move to IN_PROGRESS only) parks the running task first:
/// 1. `displace.id` must be the task whose **status** is IN_PROGRESS — the
///    status lock, never the event window (400 `"displace id is not the
///    running task"` otherwise).
/// 2. Park A at `displace.status`/`displace.sort_order` (must be
///    PLANNED/COMPLETED/DISCARDED — the matrix from IN_PROGRESS).
/// 3. `start_task(B)` on the minute grid (`B.start = A.end` — snapped `T`,
///    or `A.start + 60` under the invert guard), then place B.
/// 4. If step 3 fails: [`TasksError::AfterDisplace`] — A STAYS parked
///    (no rollback) and the error carries A's view.
///
/// `http`/`access` are `Option` because status-only moves (plan/unplan/
/// reopen/reorder/idle complete/discard) never touch Google; the worker
/// passes both only when its `needs_google` gate fires (target IN_PROGRESS,
/// leaving IN_PROGRESS, or any displace).
///
/// Validation: unknown `status` → 400; negative `sort_order` → 400;
/// `displace.status` outside PLANNED/COMPLETED/DISCARDED → 400; move to
/// IN_PROGRESS without `displace` while something runs → 409 (from
/// `start_task`); missing/other-user/soft-deleted task → 404.
pub async fn move_task(
    http: Option<&dyn HttpClient>,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    task_repo: &dyn TaskRepo,
    logs: &dyn TaskLogRepo,
    access: Option<&GoogleAccess>,
    user_id: &str,
    task_id: &str,
    now_unix: i64,
    input: &MoveTaskInput,
) -> Result<MoveTaskResponse, TasksError> {
    if !is_valid_task_status(&input.status) {
        return Err(TasksError::Invalid("unknown task status".to_string()));
    }
    if input.sort_order < 0 {
        return Err(TasksError::Invalid(
            "sort_order must not be negative".to_string(),
        ));
    }
    let Some(task) = task_repo.get_by_id(task_id).await? else {
        return Err(TasksError::NotFound);
    };
    if task.user_id != user_id {
        return Err(TasksError::NotFound);
    }
    // `displace` parks the runner so the moved task can start — it only makes
    // sense on a move TO IN_PROGRESS (the conflict dialog's one move call).
    if input.displace.is_some() && input.status != TASK_STATUS_IN_PROGRESS {
        return Err(TasksError::Invalid(
            "displace is only allowed when moving to in progress".to_string(),
        ));
    }
    if let Some(displace) = &input.displace {
        if displace.sort_order < 0 {
            return Err(TasksError::Invalid(
                "sort_order must not be negative".to_string(),
            ));
        }
        if !matches!(
            displace.status.as_str(),
            TASK_STATUS_PLANNED | TASK_STATUS_COMPLETED | TASK_STATUS_DISCARDED
        ) {
            return Err(TasksError::Invalid(
                "displace status must be planned, completed, or discarded".to_string(),
            ));
        }
    }

    // The matrix's IN_PROGRESS → IN_PROGRESS no-op: nothing happens, the
    // current task is returned as-is, `sort_order` is ignored.
    if task.status == TASK_STATUS_IN_PROGRESS && input.status == TASK_STATUS_IN_PROGRESS {
        let taxonomy = load_taxonomy(category_repo, user_id).await?;
        return Ok(MoveTaskResponse {
            task: to_view(&task, &taxonomy),
            displaced: None,
            event: None,
        });
    }

    // ── Displace flow: park A, then start B ──
    if let Some(displace) = &input.displace {
        // `displace.id` must be the task whose STATUS is IN_PROGRESS (the
        // lock) — the event window is never consulted, so a stale/expired
        // event can neither block the displace nor falsify the identity.
        let Some(displaced_task) = task_repo.get_by_id(&displace.id).await? else {
            return Err(TasksError::Invalid(
                "displace id is not the running task".to_string(),
            ));
        };
        if displaced_task.user_id != user_id || displaced_task.status != TASK_STATUS_IN_PROGRESS {
            return Err(TasksError::Invalid(
                "displace id is not the running task".to_string(),
            ));
        }
        // B's start instant on the minute grid (rule: `A.end == B.start`):
        // snapped T, or `A.start + 60` when T has not passed A's start (the
        // same invert guard the park PATCH applies, so the two agree).
        let displaced_start_unix = latest_started_event(logs, events, &displaced_task.id)
            .await?
            .and_then(|event| rfc3339_to_unix_secs(&event.start_time));
        let t_unix = nearest_minute_unix(now_unix);
        let b_start_unix = match displaced_start_unix {
            Some(start) if t_unix <= start => start + 60,
            _ => t_unix,
        };

        // Park A (matrix from IN_PROGRESS: pause/complete/discard), then rank.
        // A failure here is a plain error — nothing was started yet.
        dispatch_matrix_action(
            http, calendars, events, list_repo, category_repo, task_repo, logs, access,
            user_id, &displaced_task.id, &displaced_task.status, &displace.status, now_unix,
        )
        .await?;
        let displaced_row = place_at(
            task_repo,
            user_id,
            &displaced_task.id,
            &displace.status,
            displace.sort_order,
        )
        .await?;

        // Start B, then rank B. A start failure is an HONEST partial failure:
        // A stays parked (no rollback) and `AfterDisplace` carries A's view.
        match dispatch_matrix_action(
            http, calendars, events, list_repo, category_repo, task_repo, logs, access,
            user_id, task_id, &task.status, TASK_STATUS_IN_PROGRESS, b_start_unix,
        )
        .await
        {
            Ok(event) => {
                let row = place_at(
                    task_repo,
                    user_id,
                    task_id,
                    TASK_STATUS_IN_PROGRESS,
                    input.sort_order,
                )
                .await?;
                let taxonomy = load_taxonomy(category_repo, user_id).await?;
                Ok(MoveTaskResponse {
                    task: to_view(&row, &taxonomy),
                    displaced: Some(to_view(&displaced_row, &taxonomy)),
                    event,
                })
            }
            Err(inner) => {
                let taxonomy = load_taxonomy(category_repo, user_id).await?;
                Err(TasksError::AfterDisplace {
                    displaced: to_view(&displaced_row, &taxonomy),
                    source: Box::new(inner),
                })
            }
        }
    } else if task.status == input.status {
        // Same column: pure reorder (the moving card keeps its status; rank
        // shifts neighbors only when it actually changes position).
        let row = reorder_in_place(task_repo, user_id, &task, input.sort_order).await?;
        let taxonomy = load_taxonomy(category_repo, user_id).await?;
        Ok(MoveTaskResponse {
            task: to_view(&row, &taxonomy),
            displaced: None,
            event: None,
        })
    } else {
        // Cross-column: dispatch the matrix action, then place.
        let event = dispatch_matrix_action(
            http, calendars, events, list_repo, category_repo, task_repo, logs, access,
            user_id, task_id, &task.status, &input.status, now_unix,
        )
        .await?;
        let row = place_at(task_repo, user_id, task_id, &input.status, input.sort_order).await?;
        let taxonomy = load_taxonomy(category_repo, user_id).await?;
        Ok(MoveTaskResponse {
            task: to_view(&row, &taxonomy),
            displaced: None,
            event,
        })
    }
}

/// Loads the taxonomy, seeding it first (same count-gated path as
/// `list_tasks`/`create_task`; the gate keeps the first Lists visit order).
async fn load_taxonomy_seeded(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
) -> Result<Taxonomy, TasksError> {
    let lists = list_repo.list_by_user_id(user_id).await?;
    let categories = category_repo.list_by_user_id(user_id).await?;
    ensure_taxonomy(list_repo, category_repo, &lists, &categories, user_id).await?;
    load_taxonomy(category_repo, user_id).await.map_err(TasksError::from)
}

/// Resolves the Google calendar a started event lands on (locked rule):
/// `wanted` is the first non-empty of the matched category's first
/// regex-matching pattern's `google_calendar_id` (patterns walked in stored
/// `sort_order` — a first match WITHOUT a calendar does not skip ahead to a
/// later matching pattern that has one), the matched category's
/// `google_calendar_id`, and the parent root's `google_calendar_id` (a
/// child's match only; a root match has no parent slot). The started event
/// lands on `wanted` when that calendar exists for the user, is not
/// soft-deleted (`list_by_user_id` already filters), and is writable
/// (`access_role` owner/writer); a missing or read-only named calendar falls
/// back to the user's **primary** calendar (also writable), never to the
/// next inheritance slot. No writable calendar → 400.
async fn resolve_target_calendar(
    calendars: &dyn CalendarRepo,
    taxonomy: &Taxonomy,
    title: &str,
    user_id: &str,
) -> Result<TargetCalendar, TasksError> {
    let user_cals = calendars.list_by_user_id(user_id).await?;
    let category = match classify(title, CalendarScope::Ignore, &taxonomy.matchers) {
        ClassifyOutcome::Matched { category_id } => taxonomy
            .categories
            .iter()
            .find(|category| category.id == category_id),
        // A title that matches nothing (or conflicts) has no inheritance
        // chain — `wanted` stays None and the primary fallback below runs:
        // starting is a read, never a validation.
        ClassifyOutcome::Untracked { .. } => None,
    };
    let matcher = category.and_then(|category| {
        taxonomy
            .matchers
            .iter()
            .find(|matcher| matcher.category_id == category.id)
    });
    // One-level tree: `parent_id` is at most a root.
    let parent = category
        .and_then(|category| category.parent_id.as_deref())
        .and_then(|parent_id| taxonomy.categories.iter().find(|entry| entry.id == parent_id));
    let wanted = matcher
        .and_then(|matcher| first_matching_pattern(title, CalendarScope::Ignore, matcher))
        .and_then(|pattern| pattern.google_calendar_id.as_deref())
        .or_else(|| category.and_then(|category| category.google_calendar_id.as_deref()))
        .or_else(|| parent.and_then(|parent| parent.google_calendar_id.as_deref()));
    let target = wanted
        // A named-but-missing or read-only calendar never falls through to
        // the next inheritance slot: straight to the user's primary.
        .and_then(|wanted| {
            user_cals
                .iter()
                .find(|cal| cal.google_calendar_id == wanted && is_writable(cal))
        })
        .or_else(|| {
            user_cals
                .iter()
                .find(|cal| cal.is_primary && is_writable(cal))
        })
        .ok_or_else(|| TasksError::Invalid("no writable calendar".to_string()))?;
    Ok(TargetCalendar {
        calendar_id: target.id.clone(),
        // The matched category's STORED color, or `None` for untracked /
        // categories without one — the event insert omits `colorId` then.
        // Never inherited from the pattern or the parent.
        google_color_id: category.and_then(|category| category.google_color_id.clone()),
    })
}

/// A resolved target calendar for a started event (local `google_calendars.id`
/// — the Google id itself lives on the row the event insert re-reads).
#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetCalendar {
    calendar_id: String,
    /// Stored `google_color_id` of the matched category; `None` when the
    /// title is untracked or the category has no stored color.
    google_color_id: Option<String>,
}

/// Whether a calendar looks writable: Google `access_role` is `owner` or
/// `writer`.
fn is_writable(calendar: &crate::models::GoogleCalendar) -> bool {
    calendar.access_role == "owner" || calendar.access_role == "writer"
}

// ──────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────

fn is_valid_priority(priority: &str) -> bool {
    matches!(priority, "high" | "medium" | "low")
}

fn is_valid_difficulty(difficulty: &str) -> bool {
    matches!(difficulty, "easy" | "medium" | "hard")
}

fn to_view(task: &Task, taxonomy: &Taxonomy) -> TaskView {
    let outcome = classify(&task.title, CalendarScope::Ignore, &taxonomy.matchers);
    let category = match &outcome {
        ClassifyOutcome::Matched { category_id } => taxonomy
            .categories
            .iter()
            .find(|category| category.id == *category_id)
            // A task matched a category that vanished between the pattern
            // build and here (impossible in one run, but never panic on read):
            // fall back to the untracked sink.
            .map(|category| summary_for(category, &taxonomy.categories))
            .or_else(|| untracked_summary(&taxonomy.categories)),
        ClassifyOutcome::Untracked { .. } => untracked_summary(&taxonomy.categories),
    };
    let category = category.expect("ensure_taxonomy guarantees the untracked sink exists");
    TaskView {
        id: task.id.clone(),
        user_id: task.user_id.clone(),
        title: task.title.clone(),
        description: task.description.clone(),
        duration_minutes: task.duration_minutes,
        priority: task.priority.clone(),
        difficulty: task.difficulty.clone(),
        sort_order: task.sort_order,
        status: task.status.clone(),
        created_at: task.created_at.clone(),
        updated_at: task.updated_at.clone(),
        category,
    }
}

/// The `untracked` sink summary; `None` only before the first seed (callers
/// run `ensure_taxonomy` first, and the sink is undeletable).
fn untracked_summary(categories: &[TaskCategory]) -> Option<TaskCategorySummary> {
    categories
        .iter()
        .find(|category| category.is_untracked)
        .map(|category| summary_for(category, categories))
}

fn summary_for(category: &TaskCategory, categories: &[TaskCategory]) -> TaskCategorySummary {
    // Inherited list: a child takes the parent root's list_id; a root takes
    // its own (untracked has none).
    let inherited_list_id = match category.parent_id.as_deref() {
        Some(parent_id) => categories
            .iter()
            .find(|entry| entry.id == parent_id)
            .and_then(|parent| parent.list_id.clone()),
        None => category.list_id.clone(),
    };
    TaskCategorySummary {
        id: category.id.clone(),
        title: category.title.clone(),
        slug: category.slug.clone(),
        list_id: category.list_id.clone(),
        inherited_list_id,
        is_untracked: category.is_untracked,
        color: category.color.clone(),
    }
}

/// Resolves a title to a single non-untracked category id, enforcing the
/// locked create/update rules:
/// - 0 matches → `title does not match a category`
/// - several matches (cross-tree conflict) → `title matches multiple categories`
/// - the matched category is the `untracked` sink → `title matches untracked`
///
/// A title matching only a root whose children do not match (parent
/// remainder) resolves to the root — `classify` already drops parents beaten
/// by their own children, so a single leftover match is always OK here.
fn resolve_category(title: &str, taxonomy: &Taxonomy) -> Result<String, TasksError> {
    match classify(title, CalendarScope::Ignore, &taxonomy.matchers) {
        ClassifyOutcome::Matched { category_id } => {
            if taxonomy
                .categories
                .iter()
                .any(|category| category.id == category_id && category.is_untracked)
            {
                return Err(TasksError::Invalid("title matches untracked".to_string()));
            }
            Ok(category_id)
        }
        ClassifyOutcome::Untracked { conflict: false } => {
            Err(TasksError::Invalid("title does not match a category".to_string()))
        }
        ClassifyOutcome::Untracked { conflict: true } => {
            Err(TasksError::Invalid("title matches multiple categories".to_string()))
        }
    }
}

/// Loads living categories and their patterns in two queries (list + bulk
/// patterns), matching `list_categories`.
async fn load_taxonomy(
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
) -> Result<Taxonomy, RepoError> {
    let categories = category_repo.list_by_user_id(user_id).await?;

    // Every living category's patterns in one query, grouped by category
    // (kills the old per-category N+1: one patterns SELECT per category).
    let mut patterns_by_category: HashMap<String, Vec<TaskCategoryPattern>> = HashMap::new();
    for pattern in category_repo.list_patterns_by_user_id(user_id).await? {
        patterns_by_category
            .entry(pattern.category_id.clone())
            .or_default()
            .push(pattern);
    }

    let mut matchers = Vec::with_capacity(categories.len());
    for category in &categories {
        let patterns = patterns_by_category.remove(&category.id).unwrap_or_default();
        matchers.push(CategoryWithPatterns {
            category_id: category.id.clone(),
            parent_id: category.parent_id.clone(),
            patterns,
        });
    }
    Ok(Taxonomy { categories, matchers })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::models::{
        GoogleCalendar, GoogleOAuthToken, NewCalendar, NewCalendarEvent, NewTask,
        NewTaskCategory, NewTaskCategoryInput, NewTaskCategoryPattern, NewTaskList, NewToken,
        TaskCategory, TaskCategoryPattern, TaskList, TaskLog, UpdateTaskCategory, UpdateTaskList,
    };
    use crate::oauth::{HttpClient, HttpError};
    use crate::repo::{
        CalendarEventRepo, CalendarRepo, TaskCategoryRepo, TaskLogRepo, TokenRepo,
    };

    // ──────────────────────────────────────────
    // Fakes
    // ──────────────────────────────────────────

    /// In-memory `TaskRepo` mirroring D1 semantics: soft-deleted rows are
    /// filtered from reads, updates are partial (COALESCE).
    struct FakeTaskRepo {
        stored: Mutex<Vec<Task>>,
        next_id: Mutex<u64>,
    }

    impl FakeTaskRepo {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl TaskRepo for FakeTaskRepo {
        async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<Task>, RepoError> {
            let mut rows: Vec<Task> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.deleted_at.is_none())
                .cloned()
                .collect();
            // Mirrors TASK_LIST_BY_USER_ID_SQL (stable sort ties by id).
            rows.sort_by(|a, b| {
                (
                    a.status.as_str(),
                    a.sort_order,
                    a.created_at.as_str(),
                    &a.id,
                )
                    .cmp(&(
                        b.status.as_str(),
                        b.sort_order,
                        b.created_at.as_str(),
                        &b.id,
                    ))
            });
            Ok(rows)
        }

        async fn list_in_progress(&self) -> Result<Vec<Task>, RepoError> {
            // Mirrors TASK_LIST_IN_PROGRESS_SQL: living rows with the
            // IN_PROGRESS status, across all users.
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.status == TASK_STATUS_IN_PROGRESS && row.deleted_at.is_none())
                .cloned()
                .collect())
        }

        async fn get_by_id(&self, id: &str) -> Result<Option<Task>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|row| row.id == id && row.deleted_at.is_none())
                .cloned())
        }

        async fn insert(&self, task: NewTask) -> Result<Task, RepoError> {
            let mut next = self.next_id.lock().unwrap();
            let row = Task {
                id: format!("task-{next}"),
                user_id: task.user_id.clone(),
                title: task.title.clone(),
                description: task.description.clone(),
                duration_minutes: task.duration_minutes,
                priority: task.priority.clone(),
                difficulty: task.difficulty.clone(),
                sort_order: task.sort_order,
                status: TASK_STATUS_OPEN.to_string(),
                created_at: "2026-08-18T00:00:00Z".to_string(),
                updated_at: "2026-08-18T00:00:00Z".to_string(),
                deleted_at: None,
            };
            *next += 1;
            self.stored.lock().unwrap().push(row.clone());
            Ok(row)
        }

        async fn shift_sort_order(
            &self,
            user_id: &str,
            status: &str,
            from_inclusive: i64,
        ) -> Result<(), RepoError> {
            // Same semantics as TASK_SHIFT_SORT_ORDER_SQL: `i64::MAX` opens
            // the range top for the +1 shift.
            self.shift_sort_order_by(user_id, status, from_inclusive, i64::MAX, 1)
                .await
        }

        async fn shift_sort_order_by(
            &self,
            user_id: &str,
            status: &str,
            from_inclusive: i64,
            to_inclusive: i64,
            delta: i64,
        ) -> Result<(), RepoError> {
            // Mirrors TASK_SHIFT_SORT_ORDER_RANGE_SQL.
            let mut stored = self.stored.lock().unwrap();
            for row in stored.iter_mut() {
                if row.user_id == user_id
                    && row.status == status
                    && row.deleted_at.is_none()
                    && row.sort_order >= from_inclusive
                    && row.sort_order <= to_inclusive
                {
                    row.sort_order += delta;
                }
            }
            Ok(())
        }

        async fn update(
            &self,
            id: &str,
            updates: &UpdateTask,
        ) -> Result<Option<Task>, RepoError> {
            let mut stored = self.stored.lock().unwrap();
            let Some(row) = stored
                .iter_mut()
                .find(|row| row.id == id && row.deleted_at.is_none())
            else {
                return Ok(None);
            };
            if let Some(title) = &updates.title {
                row.title = title.clone();
            }
            if let Some(description) = &updates.description {
                row.description = description.clone();
            }
            if let Some(duration_minutes) = updates.duration_minutes {
                row.duration_minutes = duration_minutes;
            }
            if let Some(priority) = &updates.priority {
                row.priority = priority.clone();
            }
            if let Some(difficulty) = &updates.difficulty {
                row.difficulty = difficulty.clone();
            }
            row.updated_at = "2026-08-18T01:00:00Z".to_string();
            Ok(Some(row.clone()))
        }

        async fn set_status(
            &self,
            id: &str,
            status: &str,
            now_rfc3339: &str,
        ) -> Result<Option<Task>, RepoError> {
            let mut stored = self.stored.lock().unwrap();
            let Some(row) = stored
                .iter_mut()
                .find(|row| row.id == id && row.deleted_at.is_none())
            else {
                return Ok(None);
            };
            row.status = status.to_string();
            row.updated_at = now_rfc3339.to_string();
            Ok(Some(row.clone()))
        }

        async fn set_sort_order(&self, id: &str, sort_order: i64) -> Result<Option<Task>, RepoError> {
            // Mirrors TASK_SET_SORT_ORDER_SQL: rank only — no status, no
            // `updated_at`.
            let mut stored = self.stored.lock().unwrap();
            let Some(row) = stored
                .iter_mut()
                .find(|row| row.id == id && row.deleted_at.is_none())
            else {
                return Ok(None);
            };
            row.sort_order = sort_order;
            Ok(Some(row.clone()))
        }

        async fn soft_delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
            if let Some(row) = self
                .stored
                .lock()
                .unwrap()
                .iter_mut()
                .find(|row| row.id == id && row.deleted_at.is_none())
            {
                row.deleted_at = Some(now_rfc3339.to_string());
                row.updated_at = now_rfc3339.to_string();
            }
            Ok(())
        }
    }

    /// In-memory `TaskListRepo` for seeding via `list_lists`.
    struct FakeTaskListRepo {
        stored: Mutex<Vec<TaskList>>,
        next_id: Mutex<u64>,
    }

    impl FakeTaskListRepo {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl TaskListRepo for FakeTaskListRepo {
        async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<TaskList>, RepoError> {
            let mut rows: Vec<TaskList> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.deleted_at.is_none())
                .cloned()
                .collect();
            rows.sort_by(|a, b| (a.sort_order, &a.name).cmp(&(b.sort_order, &b.name)));
            Ok(rows)
        }

        async fn get_by_id(&self, id: &str) -> Result<Option<TaskList>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|row| row.id == id && row.deleted_at.is_none())
                .cloned())
        }

        async fn insert(&self, list: NewTaskList) -> Result<TaskList, RepoError> {
            let mut next = self.next_id.lock().unwrap();
            let row = TaskList {
                id: format!("list-{next}"),
                user_id: list.user_id,
                name: list.name,
                color: list.color,
                sort_order: list.sort_order,
                created_at: "2026-08-18T00:00:00Z".to_string(),
                updated_at: "2026-08-18T00:00:00Z".to_string(),
                deleted_at: None,
            };
            *next += 1;
            self.stored.lock().unwrap().push(row.clone());
            Ok(row)
        }

        async fn update(
            &self,
            _id: &str,
            _updates: &UpdateTaskList,
        ) -> Result<Option<TaskList>, RepoError> {
            Ok(None)
        }

        async fn soft_delete(&self, _id: &str, _now_rfc3339: &str) -> Result<(), RepoError> {
            Ok(())
        }

        async fn count_by_user_id(&self, user_id: &str) -> Result<i64, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.deleted_at.is_none())
                .count() as i64)
        }

        async fn count_root_categories_for_list(&self, _list_id: &str) -> Result<i64, RepoError> {
            Ok(0)
        }
    }

    /// In-memory `TaskCategoryRepo` with real insert/pattern replacement so
    /// `ensure_taxonomy` and child creation work end to end.
    struct FakeTaskCategoryRepo {
        stored: Mutex<Vec<TaskCategory>>,
        patterns: Mutex<HashMap<String, Vec<TaskCategoryPattern>>>,
        next_id: Mutex<u64>,
        // Call counters locking the N+1 regression: `load_taxonomy` must
        // never fall back to per-category pattern queries.
        list_by_user_id_calls: Mutex<usize>,
        list_patterns_by_user_id_calls: Mutex<usize>,
        list_patterns_by_category_id_calls: Mutex<usize>,
    }

    impl FakeTaskCategoryRepo {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                patterns: Mutex::new(HashMap::new()),
                next_id: Mutex::new(1),
                list_by_user_id_calls: Mutex::new(0),
                list_patterns_by_user_id_calls: Mutex::new(0),
                list_patterns_by_category_id_calls: Mutex::new(0),
            }
        }

        /// Ids of the user's living categories matching a predicate.
        fn ids(&self, user_id: &str, is_untracked: bool) -> Vec<String> {
            self.stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.is_untracked == is_untracked)
                .map(|row| row.id.clone())
                .collect()
        }
    }

    #[async_trait::async_trait(?Send)]
    impl TaskCategoryRepo for FakeTaskCategoryRepo {
        async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<TaskCategory>, RepoError> {
            *self.list_by_user_id_calls.lock().unwrap() += 1;
            let mut rows: Vec<TaskCategory> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.deleted_at.is_none())
                .cloned()
                .collect();
            rows.sort_by(|a, b| (a.sort_order, &a.title).cmp(&(b.sort_order, &b.title)));
            Ok(rows)
        }

        async fn get_by_id(&self, id: &str) -> Result<Option<TaskCategory>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|row| row.id == id && row.deleted_at.is_none())
                .cloned())
        }

        async fn insert(&self, category: NewTaskCategory) -> Result<TaskCategory, RepoError> {
            let mut next = self.next_id.lock().unwrap();
            let row = TaskCategory {
                id: format!("cat-{next}"),
                user_id: category.user_id,
                list_id: category.list_id,
                parent_id: category.parent_id,
                title: category.title,
                slug: category.slug,
                color: category.color,
                is_productive: category.is_productive,
                google_calendar_id: category.google_calendar_id,
                google_color_id: category.google_color_id,
                sort_order: category.sort_order,
                is_untracked: category.is_untracked,
                created_at: "2026-08-18T00:00:00Z".to_string(),
                updated_at: "2026-08-18T00:00:00Z".to_string(),
                deleted_at: None,
            };
            *next += 1;
            self.stored.lock().unwrap().push(row.clone());
            Ok(row)
        }

        async fn update(
            &self,
            _id: &str,
            _updates: &UpdateTaskCategory,
        ) -> Result<Option<TaskCategory>, RepoError> {
            Ok(None)
        }

        async fn soft_delete(&self, _id: &str, _now_rfc3339: &str) -> Result<(), RepoError> {
            Ok(())
        }

        async fn count_by_user_id(&self, user_id: &str) -> Result<i64, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.deleted_at.is_none())
                .count() as i64)
        }

        async fn count_children(&self, _category_id: &str) -> Result<i64, RepoError> {
            Ok(0)
        }

        async fn get_untracked(&self, user_id: &str) -> Result<Option<TaskCategory>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|row| row.user_id == user_id && row.is_untracked && row.deleted_at.is_none())
                .cloned())
        }

        async fn list_patterns_by_category_id(
            &self,
            category_id: &str,
        ) -> Result<Vec<TaskCategoryPattern>, RepoError> {
            *self.list_patterns_by_category_id_calls.lock().unwrap() += 1;
            let mut rows = self
                .patterns
                .lock()
                .unwrap()
                .get(category_id)
                .cloned()
                .unwrap_or_default();
            rows.sort_by_key(|row| row.sort_order);
            Ok(rows)
        }

        async fn list_patterns_by_user_id(
            &self,
            user_id: &str,
        ) -> Result<Vec<TaskCategoryPattern>, RepoError> {
            *self.list_patterns_by_user_id_calls.lock().unwrap() += 1;
            // Mirrors TASK_CATEGORY_PATTERNS_LIST_BY_USER_ID_SQL: only living
            // categories' patterns, ordered by category then sort_order.
            let living: Vec<String> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.deleted_at.is_none())
                .map(|row| row.id.clone())
                .collect();
            let patterns = self.patterns.lock().unwrap();
            let mut rows: Vec<TaskCategoryPattern> = living
                .iter()
                .filter_map(|category_id| patterns.get(category_id))
                .flatten()
                .cloned()
                .collect();
            rows.sort_by(|a, b| {
                a.category_id
                    .cmp(&b.category_id)
                    .then_with(|| a.sort_order.cmp(&b.sort_order))
            });
            Ok(rows)
        }

        async fn replace_patterns(
            &self,
            category_id: &str,
            patterns: Vec<NewTaskCategoryPattern>,
        ) -> Result<(), RepoError> {
            let mut stored = self.patterns.lock().unwrap();
            stored.remove(category_id);
            let rows: Vec<TaskCategoryPattern> = patterns
                .into_iter()
                .enumerate()
                .map(|(sort_order, input)| TaskCategoryPattern {
                    id: format!("pat-{category_id}-{sort_order}"),
                    category_id: category_id.to_string(),
                    regex: input.regex,
                    google_calendar_id: input.google_calendar_id,
                    sort_order: sort_order as i64,
                    created_at: "2026-08-18T00:00:00Z".to_string(),
                    updated_at: "2026-08-18T00:00:00Z".to_string(),
                })
                .collect();
            stored.insert(category_id.to_string(), rows);
            Ok(())
        }

        async fn delete_patterns_by_category_id(&self, category_id: &str) -> Result<(), RepoError> {
            self.patterns.lock().unwrap().remove(category_id);
            Ok(())
        }
    }

    // ──────────────────────────────────────────
    // Helpers
    // ──────────────────────────────────────────

    fn pattern(regex: &str) -> NewTaskCategoryPattern {
        NewTaskCategoryPattern {
            regex: regex.to_string(),
            google_calendar_id: None,
        }
    }

    fn input(title: &str) -> NewTaskInput {
        NewTaskInput {
            title: title.to_string(),
            description: None,
            duration_minutes: None,
            priority: None,
            difficulty: None,
        }
    }

    /// Seeds a fresh user's taxonomy (4 lists, 4 roots + untracked, 8
    /// patterns) through the exact path the app uses, returning the repos.
    fn seeded() -> (FakeTaskListRepo, FakeTaskCategoryRepo, FakeTaskRepo) {
        let lists = FakeTaskListRepo::new();
        let categories = FakeTaskCategoryRepo::new();
        let tasks = FakeTaskRepo::new();
        pollster::block_on(crate::list_lists(&lists, &categories, "u-1")).unwrap();
        (lists, categories, tasks)
    }

    fn category_ids_by_slug(repo: &FakeTaskCategoryRepo) -> HashMap<String, String> {
        repo.stored
            .lock()
            .unwrap()
            .iter()
            .filter(|row| row.user_id == "u-1" && row.deleted_at.is_none())
            .filter_map(|row| Some((row.slug.clone(), row.id.clone())))
            .collect()
    }

    // ──────────────────────────────────────────
    // Create
    // ──────────────────────────────────────────

    #[test]
    fn create_matches_root_exact_title_and_pipe_suffix() {
        let (lists, categories, tasks) = seeded();
        let work = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap();
        assert_eq!(work.task.status, "OPEN");
        assert_eq!(work.task.duration_minutes, 15, "duration defaults to 15");
        assert_eq!(work.task.priority, "medium", "priority defaults to medium");
        assert_eq!(work.task.difficulty, "easy", "difficulty defaults to easy");
        assert_eq!(work.task.category.title, "Work");
        assert!(!work.task.category.is_untracked);
        assert!(work.task.category.inherited_list_id.is_some(), "root owns a list");

        let review = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Review Q3 | Work"),
        ))
        .unwrap();
        assert_eq!(review.task.category.title, "Work");
        assert_ne!(work.task.id, review.task.id, "titles are not unique");
        assert_eq!(work.task.category.id, review.task.category.id);
    }

    #[test]
    fn create_task_prepends_backlog_sort_order() {
        let (lists, categories, tasks) = seeded();
        let first = work_task(&lists, &categories, &tasks);
        let second = work_task(&lists, &categories, &tasks);

        // The newest task sits at the front of the Backlog pile (0); the
        // previous front task shifts one back. TaskView is a snapshot, so the
        // shifted rank of `first` is asserted on the persisted row.
        assert_eq!(second.sort_order, 0, "new task response ranks 0");
        let stored = tasks.stored.lock().unwrap();
        let first_row = stored.iter().find(|row| row.id == first.id).unwrap();
        let second_row = stored.iter().find(|row| row.id == second.id).unwrap();
        assert_eq!(second_row.sort_order, 0, "new task prepends Backlog");
        assert_eq!(first_row.sort_order, 1, "existing OPEN task shifted up");
    }

    #[test]
    fn create_unmatched_title_is_invalid() {
        let (lists, categories, tasks) = seeded();
        let err = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("asdf"),
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "title does not match a category"));
        assert_eq!(tasks.stored.lock().unwrap().len(), 0, "nothing persisted");
    }

    #[test]
    fn create_title_matching_two_roots_is_invalid() {
        let (lists, categories, tasks) = seeded();
        // Give Fitness the same pattern as Work so "Work" matches two roots.
        let ids = category_ids_by_slug(&categories);
        pollster::block_on(categories.replace_patterns(
            &ids["fitness"],
            vec![pattern("^Work$")],
        ))
        .unwrap();
        let err = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "title matches multiple categories"));
    }

    #[test]
    fn create_title_matching_untracked_is_invalid() {
        let (lists, categories, tasks) = seeded();
        // The sink normally has no patterns; give it one to prove the guard.
        let untracked_id = categories.ids("u-1", true).pop().unwrap();
        pollster::block_on(categories.replace_patterns(
            &untracked_id,
            vec![pattern("^Untracked$")],
        ))
        .unwrap();
        let err = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Untracked"),
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "title matches untracked"));
    }

    #[test]
    fn create_parent_remainder_is_allowed() {
        let (lists, categories, tasks) = seeded();
        // A child whose own pattern does not match the parent's title.
        let ids = category_ids_by_slug(&categories);
        let child = pollster::block_on(crate::categories::create_category(
            &categories,
            &lists,
            "u-1",
            &NewTaskCategoryInput {
                title: "Coding".to_string(),
                slug: None,
                color: "#2a5c8a".to_string(),
                is_productive: None,
                google_calendar_id: None,
                google_color_id: None,
                list_id: None,
                parent_id: Some(ids["work"].clone()),
                sort_order: None,
                is_untracked: None,
                patterns: vec![pattern("^Code$")],
            },
        ))
        .unwrap()
        .category;

        // "Work" matches only the root (parent remainder) → allowed, root.
        let work = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap();
        assert_eq!(work.task.category.id, ids["work"]);
        assert_eq!(work.task.category.inherited_list_id, categories.stored.lock().unwrap().iter().find(|c| c.id == ids["work"]).unwrap().list_id);

        // "Code" matches only the child → allowed, child (inherits the root's
        // list).
        let code = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Code"),
        ))
        .unwrap();
        assert_eq!(code.task.category.id, child.id);
        assert_eq!(code.task.category.inherited_list_id, work.task.category.inherited_list_id);
    }

    #[test]
    fn create_validates_duration_and_priority() {
        let (lists, categories, tasks) = seeded();
        let zero_duration = NewTaskInput {
            title: "Work".to_string(),
            description: None,
            duration_minutes: Some(0),
            priority: None,
            difficulty: None,
        };
        let err = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &zero_duration,
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "duration must be at least 1 minute"));

        let bad_priority = NewTaskInput {
            title: "Work".to_string(),
            description: None,
            duration_minutes: None,
            priority: Some("urgent".to_string()),
            difficulty: None,
        };
        let err = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &bad_priority,
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "priority must be one of high, medium, low"));

        // No case-coercion: uppercase and non-enum values are 400 too.
        for difficulty in ["HARD", "urgent"] {
            let bad_difficulty = NewTaskInput {
                title: "Work".to_string(),
                description: None,
                duration_minutes: None,
                priority: None,
                difficulty: Some(difficulty.to_string()),
            };
            let err = pollster::block_on(create_task(
                &lists, &categories, &tasks, "u-1", &bad_difficulty,
            ))
            .unwrap_err();
            assert!(
                matches!(err, TasksError::Invalid(m) if m == "difficulty must be one of easy, medium, hard"),
                "{difficulty} is invalid"
            );
        }
        assert_eq!(tasks.stored.lock().unwrap().len(), 0, "nothing persisted");
    }

    #[test]
    fn create_stores_explicit_difficulty() {
        let (lists, categories, tasks) = seeded();
        let input = NewTaskInput {
            title: "Work".to_string(),
            description: None,
            duration_minutes: None,
            priority: None,
            difficulty: Some("medium".to_string()),
        };
        let response = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input,
        ))
        .unwrap();
        assert_eq!(
            response.task.difficulty, "medium",
            "explicit difficulty is stored, not overwritten by the default"
        );
    }

    #[test]
    fn create_blank_title_is_invalid() {
        let (lists, categories, tasks) = seeded();
        let err = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("   "),
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "title must not be empty"));
    }

    #[test]
    fn first_create_seeds_taxonomy_so_fresh_user_can_file_tasks() {
        let lists = FakeTaskListRepo::new();
        let categories = FakeTaskCategoryRepo::new();
        let tasks = FakeTaskRepo::new();
        // No prior visit to /api/lists — create must seed internally.
        let response = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap();
        assert_eq!(response.task.category.title, "Work");
        assert_eq!(
            pollster::block_on(categories.count_by_user_id("u-1")).unwrap(),
            5,
            "four roots + untracked seeded"
        );
    }

    // ──────────────────────────────────────────
    // Classify (blur preview)
    // ──────────────────────────────────────────

    #[test]
    fn classify_title_blank_title_is_invalid() {
        let (lists, categories, _tasks) = seeded();
        for title in ["", "   "] {
            let err =
                pollster::block_on(classify_title(&lists, &categories, "u-1", title)).unwrap_err();
            assert!(
                matches!(err, TasksError::Invalid(m) if m == "title must not be empty"),
                "{title:?}"
            );
        }
    }

    #[test]
    fn classify_title_unique_match_returns_the_category_summary() {
        let (lists, categories, _tasks) = seeded();
        let response =
            pollster::block_on(classify_title(&lists, &categories, "u-1", "Work")).unwrap();
        match response {
            ClassifyResponse::Matched { category } => {
                assert_eq!(category.title, "Work");
                assert!(!category.is_untracked);
                assert!(category.inherited_list_id.is_some(), "root owns a list");
            }
            other => panic!("expected Matched, got {other:?}"),
        }
    }

    #[test]
    fn classify_title_no_match_reports_untracked_without_conflict() {
        let (lists, categories, _tasks) = seeded();
        let response =
            pollster::block_on(classify_title(&lists, &categories, "u-1", "asdf")).unwrap();
        assert_eq!(
            response,
            ClassifyResponse::Untracked {
                conflict: false,
                categories: Vec::new()
            }
        );
    }

    #[test]
    fn classify_title_two_roots_conflict_names_both_categories() {
        let (lists, categories, _tasks) = seeded();
        // Give Fitness the same pattern as Work so "Work" matches two roots
        // (mirrors create_title_matching_two_roots_is_invalid).
        let ids = category_ids_by_slug(&categories);
        pollster::block_on(categories.replace_patterns(&ids["fitness"], vec![pattern("^Work$")]))
            .unwrap();
        let response =
            pollster::block_on(classify_title(&lists, &categories, "u-1", "Work")).unwrap();
        match response {
            ClassifyResponse::Untracked {
                conflict: true,
                categories,
            } => {
                assert_eq!(categories.len(), 2);
                let mut titles: Vec<&str> = categories.iter().map(|c| c.title.as_str()).collect();
                titles.sort();
                assert_eq!(titles, vec!["Fitness", "Work"]);
            }
            other => panic!("expected untracked conflict, got {other:?}"),
        }
    }

    // ──────────────────────────────────────────
    // Update
    // ──────────────────────────────────────────

    #[test]
    fn update_title_to_unmatched_is_invalid() {
        let (lists, categories, tasks) = seeded();
        let created = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap()
        .task;
        let updates = UpdateTask {
            title: Some("asdf".to_string()),
            ..UpdateTask::default()
        };
        let err = pollster::block_on(update_task(
            &categories, &tasks, "u-1", &created.id, &updates,
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "title does not match a category"));
        // The row is untouched.
        let stored = pollster::block_on(tasks.get_by_id(&created.id)).unwrap().unwrap();
        assert_eq!(stored.title, "Work");
    }

    #[test]
    fn update_title_to_another_category_recomputes_category() {
        let (lists, categories, tasks) = seeded();
        let created = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Review | Work"),
        ))
        .unwrap()
        .task;
        assert_eq!(created.category.title, "Work");
        let updates = UpdateTask {
            title: Some("Fitness".to_string()),
            ..UpdateTask::default()
        };
        let response = pollster::block_on(update_task(
            &categories, &tasks, "u-1", &created.id, &updates,
        ))
        .unwrap();
        assert_eq!(response.task.category.title, "Fitness");
        assert_eq!(response.task.status, "OPEN", "status untouched");
    }

    #[test]
    fn update_without_title_preserves_category_and_applies_fields() {
        let (lists, categories, tasks) = seeded();
        let created = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap()
        .task;
        let updates = UpdateTask {
            title: None,
            description: Some("Deep focus session".to_string()),
            duration_minutes: Some(25),
            priority: Some("high".to_string()),
            difficulty: Some("hard".to_string()),
        };
        let response = pollster::block_on(update_task(
            &categories, &tasks, "u-1", &created.id, &updates,
        ))
        .unwrap();
        assert_eq!(response.task.description, "Deep focus session");
        assert_eq!(response.task.duration_minutes, 25);
        assert_eq!(response.task.priority, "high");
        assert_eq!(response.task.difficulty, "hard");
        assert_eq!(response.task.category.id, created.category.id, "category preserved");
    }

    #[test]
    fn update_rejects_empty_body_bad_duration_and_bad_priority() {
        let (lists, categories, tasks) = seeded();
        let created = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap()
        .task;
        assert!(matches!(
            pollster::block_on(update_task(&categories, &tasks, "u-1", &created.id, &UpdateTask::default())),
            Err(TasksError::Invalid(m)) if m == "nothing to update"
        ));
        let bad_duration = UpdateTask {
            duration_minutes: Some(0),
            ..UpdateTask::default()
        };
        assert!(matches!(
            pollster::block_on(update_task(&categories, &tasks, "u-1", &created.id, &bad_duration)),
            Err(TasksError::Invalid(m)) if m == "duration must be at least 1 minute"
        ));
        let bad_priority = UpdateTask {
            priority: Some("urgent".to_string()),
            ..UpdateTask::default()
        };
        assert!(matches!(
            pollster::block_on(update_task(&categories, &tasks, "u-1", &created.id, &bad_priority)),
            Err(TasksError::Invalid(m)) if m == "priority must be one of high, medium, low"
        ));
        // Difficulty alone in the body is still a valid update; only the value
        // is rejected (no case-coercion for HARD).
        let bad_difficulty = UpdateTask {
            difficulty: Some("HARD".to_string()),
            ..UpdateTask::default()
        };
        assert!(matches!(
            pollster::block_on(update_task(&categories, &tasks, "u-1", &created.id, &bad_difficulty)),
            Err(TasksError::Invalid(m)) if m == "difficulty must be one of easy, medium, hard"
        ));
    }

    #[test]
    fn update_is_not_found_for_missing_and_other_users_task() {
        let (lists, categories, tasks) = seeded();
        let created = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap()
        .task;
        let updates = UpdateTask {
            description: Some("x".to_string()),
            ..UpdateTask::default()
        };
        assert!(matches!(
            pollster::block_on(update_task(&categories, &tasks, "u-2", &created.id, &updates)),
            Err(TasksError::NotFound)
        ));
        assert!(matches!(
            pollster::block_on(update_task(&categories, &tasks, "u-1", "nope", &updates)),
            Err(TasksError::NotFound)
        ));
    }

    // ──────────────────────────────────────────
    // Delete
    // ──────────────────────────────────────────

    #[test]
    fn delete_soft_deletes_and_404s_on_second_try() {
        let (lists, categories, tasks) = seeded();
        let created = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap()
        .task;
        let response = pollster::block_on(delete_task(
            &tasks, "u-1", &created.id, "2026-08-18T02:00:00Z",
        ))
        .unwrap();
        assert!(response.success);
        assert_eq!(
            pollster::block_on(tasks.list_by_user_id("u-1")).unwrap().len(),
            0,
            "no living tasks remain"
        );
        assert!(matches!(
            pollster::block_on(delete_task(&tasks, "u-1", &created.id, "2026-08-18T02:00:00Z")),
            Err(TasksError::NotFound)
        ));
    }

    #[test]
    fn delete_is_not_found_for_missing_and_other_users_task() {
        let (lists, categories, tasks) = seeded();
        let created = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap()
        .task;
        assert!(matches!(
            pollster::block_on(delete_task(&tasks, "u-2", &created.id, "2026-08-18T02:00:00Z")),
            Err(TasksError::NotFound)
        ));
        assert!(matches!(
            pollster::block_on(delete_task(&tasks, "u-1", "nope", "2026-08-18T02:00:00Z")),
            Err(TasksError::NotFound)
        ));
    }

    // ──────────────────────────────────────────
    // List
    // ──────────────────────────────────────────

    #[test]
    fn list_includes_computed_category_per_task() {
        let (lists, categories, tasks) = seeded();
        pollster::block_on(create_task(&lists, &categories, &tasks, "u-1", &input("Work"))).unwrap();
        pollster::block_on(create_task(&lists, &categories, &tasks, "u-1", &input("Dinner | Family"))).unwrap();

        let response = pollster::block_on(list_tasks(&lists, &categories, &tasks, "u-1")).unwrap();
        assert_eq!(response.tasks.len(), 2);
        let by_title = |title: &str| response.tasks.iter().find(|t| t.title == title).unwrap();
        assert_eq!(by_title("Work").category.title, "Work");
        assert_eq!(by_title("Dinner | Family").category.title, "Family");
        assert!(response.tasks.iter().all(|t| t.status == "OPEN"));
    }

    #[test]
    fn list_keeps_tasks_that_no_longer_match_under_untracked() {
        let (lists, categories, tasks) = seeded();
        pollster::block_on(create_task(&lists, &categories, &tasks, "u-1", &input("Work"))).unwrap();
        // The user deletes Work's patterns: the task still lists, as untracked.
        let ids = category_ids_by_slug(&categories);
        pollster::block_on(categories.replace_patterns(&ids["work"], vec![])).unwrap();

        let response = pollster::block_on(list_tasks(&lists, &categories, &tasks, "u-1")).unwrap();
        assert_eq!(response.tasks.len(), 1, "read never drops tasks");
        assert!(response.tasks[0].category.is_untracked);
        assert_eq!(response.tasks[0].category.title, "Untracked");
    }

    #[test]
    fn list_seeds_taxonomy_for_tasks_only_client() {
        let lists = FakeTaskListRepo::new();
        let categories = FakeTaskCategoryRepo::new();
        let tasks = FakeTaskRepo::new();
        let response = pollster::block_on(list_tasks(&lists, &categories, &tasks, "u-1")).unwrap();
        assert!(response.tasks.is_empty());
        assert_eq!(
            pollster::block_on(categories.count_by_user_id("u-1")).unwrap(),
            5,
            "tasks-only client still gets a matcher"
        );
    }

    #[test]
    fn list_is_scoped_to_the_user() {
        let (lists, categories, tasks) = seeded();
        pollster::block_on(create_task(&lists, &categories, &tasks, "u-1", &input("Work"))).unwrap();
        let response = pollster::block_on(list_tasks(&lists, &categories, &tasks, "u-2")).unwrap();
        assert!(response.tasks.is_empty());
    }

    #[test]
    fn list_tasks_loads_taxonomy_patterns_in_one_query() {
        let (lists, categories, tasks) = seeded();
        // Seed path already queried; isolate the list_tasks call.
        *categories.list_by_user_id_calls.lock().unwrap() = 0;
        *categories.list_patterns_by_user_id_calls.lock().unwrap() = 0;
        *categories.list_patterns_by_category_id_calls.lock().unwrap() = 0;

        pollster::block_on(list_tasks(&lists, &categories, &tasks, "u-1")).unwrap();

        assert_eq!(*categories.list_patterns_by_user_id_calls.lock().unwrap(), 1);
        assert_eq!(
            *categories.list_patterns_by_category_id_calls.lock().unwrap(),
            0,
            "load_taxonomy must never fall back to per-category pattern queries"
        );
    }

    // ──────────────────────────────────────────
    // Timer fakes
    // ──────────────────────────────────────────

    /// Scripted HTTP fake with POST/PATCH routes — enough for the timer's
    /// `events.insert` and `events.patch` calls.
    struct FakeHttp {
        routes: Vec<(String, u16, String)>,
        posts: Mutex<Vec<(String, String)>>,
        patches: Mutex<Vec<(String, String)>>,
    }

    impl FakeHttp {
        fn new(routes: Vec<(&str, u16, &str)>) -> Self {
            Self {
                routes: routes
                    .into_iter()
                    .map(|(substr, status, body)| {
                        (substr.to_string(), status, body.to_string())
                    })
                    .collect(),
                posts: Mutex::new(Vec::new()),
                patches: Mutex::new(Vec::new()),
            }
        }

        fn route(&self, url: &str) -> (u16, Vec<u8>) {
            for (substr, status, body) in &self.routes {
                if url.contains(substr) {
                    return (*status, body.clone().into_bytes());
                }
            }
            panic!("no route for {url}");
        }
    }

    #[async_trait::async_trait(?Send)]
    impl HttpClient for FakeHttp {
        async fn post_form(&self, _url: &str, _form: &[(&str, &str)]) -> Result<Vec<u8>, HttpError> {
            Ok(Vec::new())
        }

        async fn get_bearer(&self, _url: &str, _token: &str) -> Result<Vec<u8>, HttpError> {
            Ok(Vec::new())
        }

        async fn get_bearer_raw(
            &self,
            _url: &str,
            _token: &str,
        ) -> Result<(u16, Vec<u8>), HttpError> {
            Ok((200, Vec::new()))
        }

        async fn post_json(
            &self,
            url: &str,
            _token: &str,
            body: &[u8],
        ) -> Result<(u16, Vec<u8>), HttpError> {
            self.posts
                .lock()
                .unwrap()
                .push((url.to_string(), String::from_utf8_lossy(body).to_string()));
            Ok(self.route(url))
        }

        async fn patch_json(
            &self,
            url: &str,
            _token: &str,
            body: &[u8],
        ) -> Result<(u16, Vec<u8>), HttpError> {
            self.patches
                .lock()
                .unwrap()
                .push((url.to_string(), String::from_utf8_lossy(body).to_string()));
            Ok(self.route(url))
        }
    }

    const NOW_UNIX: i64 = 1_700_000_000; // 2023-11-14T22:13:20Z (the raw clock)
    const NOW: &str = "2023-11-14T22:13:20Z";
    /// `NOW_UNIX` snapped to the nearest minute — every timer Google write
    /// lands here (22:13:20 → 22:13:00).
    const NOW_SNAPPED: &str = "2023-11-14T22:13:00Z";
    /// Snapped 15-minute end (`T + DEFAULT_DURATION_MINUTES * 60`).
    const NOW_END: &str = "2023-11-14T22:28:00Z";

    fn access() -> GoogleAccess {
        GoogleAccess {
            access_token: "at-1".to_string(),
            token_type: "Bearer".to_string(),
        }
    }

    /// A writable primary calendar for `u-1` — the default target.
    fn calendar(google_cal_id: &str, is_primary: bool) -> GoogleCalendar {
        GoogleCalendar {
            id: format!("cal-{google_cal_id}"),
            user_id: "u-1".to_string(),
            google_calendar_id: google_cal_id.to_string(),
            summary: "Work".to_string(),
            time_zone: "UTC".to_string(),
            is_primary,
            access_role: "owner".to_string(),
            sync_enabled: true,
            sync_token: String::new(),
            last_synced_at: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    struct FakeCalendarRepo {
        stored: Mutex<Vec<GoogleCalendar>>,
    }

    impl FakeCalendarRepo {
        fn with(calendars: Vec<GoogleCalendar>) -> Self {
            Self {
                stored: Mutex::new(calendars),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl CalendarRepo for FakeCalendarRepo {
        async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<GoogleCalendar>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|cal| cal.user_id == user_id && cal.deleted_at.is_none())
                .cloned()
                .collect())
        }

        async fn list_sync_enabled(&self) -> Result<Vec<GoogleCalendar>, RepoError> {
            Ok(self.stored.lock().unwrap().clone())
        }

        async fn get_by_id(&self, id: &str) -> Result<Option<GoogleCalendar>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|cal| cal.id == id && cal.deleted_at.is_none())
                .cloned())
        }

        async fn get_by_google_cal_id(
            &self,
            _user_id: &str,
            google_cal_id: &str,
        ) -> Result<Option<GoogleCalendar>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|cal| cal.google_calendar_id == google_cal_id)
                .cloned())
        }

        async fn upsert(&self, _calendar: NewCalendar) -> Result<(), RepoError> {
            Ok(())
        }

        async fn upsert_batch(&self, _calendars: Vec<NewCalendar>) -> Result<(), RepoError> {
            Ok(())
        }

        async fn update_sync_state(
            &self,
            _id: &str,
            _sync_token: &str,
            _last_synced_at_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn set_sync_enabled(
            &self,
            _id: &str,
            _enabled: bool,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn delete(&self, _id: &str, _now_rfc3339: &str) -> Result<(), RepoError> {
            Ok(())
        }
    }

    /// In-memory event repo: upserts materialize `CalendarEvent` rows (so the
    /// running query sees them) and every write is recorded.
    struct FakeEventRepo {
        stored: Mutex<Vec<CalendarEvent>>,
        upserted: Mutex<Vec<NewCalendarEvent>>,
        next_id: Mutex<u64>,
    }

    impl FakeEventRepo {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                upserted: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl CalendarEventRepo for FakeEventRepo {
        async fn upsert(
            &self,
            event: NewCalendarEvent,
            now_rfc3339: &str,
        ) -> Result<String, RepoError> {
            let mut next = self.next_id.lock().unwrap();
            let id = format!("evt-{next}");
            *next += 1;
            *self.upserted.lock().unwrap() = vec![event.clone()];
            let mut stored = self.stored.lock().unwrap();
            // Mirrors the ON CONFLICT(calendar_id, google_event_id) replace:
            // the patched echo must supersede the row it came from.
            stored.retain(|row| {
                !(row.calendar_id == event.calendar_id && row.google_event_id == event.google_event_id)
            });
            stored.push(CalendarEvent {
                id: id.clone(),
                calendar_id: event.calendar_id.clone(),
                google_event_id: event.google_event_id.clone(),
                google_etag: event.google_etag.clone(),
                google_updated_at: event.google_updated_at.clone(),
                last_synced_at: event.last_synced_at.clone(),
                title: event.title.clone(),
                description: event.description.clone(),
                start_time: event.start_time.clone(),
                end_time: event.end_time.clone(),
                recurrence: event.recurrence.clone(),
                task_id: event.task_id.clone(),
                created_at: now_rfc3339.to_string(),
                updated_at: now_rfc3339.to_string(),
                deleted_at: None,
            });
            Ok(id)
        }

        async fn upsert_batch(
            &self,
            _events: Vec<NewCalendarEvent>,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn get_by_id(&self, _id: &str) -> Result<Option<CalendarEvent>, RepoError> {
            Ok(None)
        }

        async fn get_by_calendar_and_google_id(
            &self,
            calendar_id: &str,
            google_event_id: &str,
        ) -> Result<Option<CalendarEvent>, RepoError> {
            // Same semantics as EVENT_GET_BY_CALENDAR_AND_GOOGLE_ID_SQL.
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|event| {
                    event.deleted_at.is_none()
                        && event.calendar_id == calendar_id
                        && event.google_event_id == google_event_id
                })
                .cloned())
        }

        async fn list_by_user_id_and_time_range(
            &self,
            _user_id: &str,
            _start_rfc3339: &str,
            _end_rfc3339: &str,
        ) -> Result<Vec<CalendarEvent>, RepoError> {
            Ok(self.stored.lock().unwrap().clone())
        }

        async fn list_running_by_user_id(
            &self,
            _user_id: &str,
            now_rfc3339: &str,
        ) -> Result<Vec<CalendarEvent>, RepoError> {
            // Same semantics as EVENT_LIST_RUNNING_BY_USER_ID_SQL.
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|event| {
                    event.deleted_at.is_none()
                        && !event.task_id.is_empty()
                        && event.start_time.as_str() <= now_rfc3339
                        && event.end_time.as_str() > now_rfc3339
                })
                .cloned()
                .collect())
        }

        async fn delete(&self, _id: &str, _now_rfc3339: &str) -> Result<(), RepoError> {
            Ok(())
        }

        async fn delete_by_google_event_id(
            &self,
            _calendar_id: &str,
            _google_event_id: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn delete_stale(
            &self,
            _calendar_id: &str,
            _older_than_rfc3339: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeTaskLogRepo {
        inserted: Mutex<Vec<NewTaskLog>>,
    }

    #[async_trait::async_trait(?Send)]
    impl TaskLogRepo for FakeTaskLogRepo {
        async fn insert(&self, log: NewTaskLog, _now_rfc3339: &str) -> Result<String, RepoError> {
            self.inserted.lock().unwrap().push(log);
            Ok("log-1".to_string())
        }

        async fn latest_started_by_task_id(
            &self,
            task_id: &str,
        ) -> Result<Option<TaskLog>, RepoError> {
            // Mirrors TASK_LOG_LATEST_STARTED_BY_TASK_ID_SQL: scan the
            // inserted rows in reverse for the task's newest `started` row and
            // synthesize a `TaskLog` from the insert input.
            let inserted = self.inserted.lock().unwrap();
            Ok(inserted
                .iter()
                .rev()
                .find(|log| log.task_id == task_id && log.r#type == TASK_LOG_STARTED)
                .map(|log| TaskLog {
                    id: "log-1".to_string(),
                    task_id: log.task_id.clone(),
                    user_id: log.user_id.clone(),
                    r#type: log.r#type.clone(),
                    at: log.at.clone(),
                    calendar_id: log.calendar_id.clone().unwrap_or_default(),
                    google_event_id: log.google_event_id.clone().unwrap_or_default(),
                    created_at: log.at.clone(),
                }))
        }
    }

    /// Token repo for the elongate cron tests: returns a stored token per
    /// user (expiring far in the future, so `refresh_if_needed` never POSTs)
    /// — or `None` for users without one, which fails that user's refresh
    /// without touching anyone else.
    struct FakeTokenRepo {
        stored: Mutex<HashMap<String, GoogleOAuthToken>>,
    }

    impl FakeTokenRepo {
        fn with(tokens: Vec<GoogleOAuthToken>) -> Self {
            let stored = tokens
                .into_iter()
                .map(|token| (token.user_id.clone(), token))
                .collect();
            Self {
                stored: Mutex::new(stored),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl TokenRepo for FakeTokenRepo {
        async fn get_by_user_id(
            &self,
            user_id: &str,
        ) -> Result<Option<GoogleOAuthToken>, RepoError> {
            Ok(self.stored.lock().unwrap().get(user_id).cloned())
        }

        async fn upsert(&self, _token: NewToken) -> Result<(), RepoError> {
            Ok(())
        }

        async fn delete(&self, _user_id: &str, _now_rfc3339: &str) -> Result<(), RepoError> {
            Ok(())
        }
    }

    /// A stored OAuth token for `user_id` whose expiry is centuries out, so
    /// `refresh_if_needed` returns it as-is (no refresh POST).
    fn fresh_token(user_id: &str, access_token: &str) -> GoogleOAuthToken {
        GoogleOAuthToken {
            id: format!("tok-{user_id}"),
            user_id: user_id.to_string(),
            access_token: access_token.to_string(),
            refresh_token: Some("rt-1".to_string()),
            expiry: "2099-01-01T00:00:00Z".to_string(),
            token_type: "Bearer".to_string(),
            scope: Some("calendar".to_string()),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    /// OAuth client credentials for `run_elongate_cron` tests; the fresh
    /// tokens above mean `refresh_if_needed` never uses them.
    fn oauth_config() -> OAuthConfig {
        OAuthConfig {
            client_id: "client-id.apps.googleusercontent.com".to_string(),
            client_secret: "client-secret".to_string(),
            redirect_url: "http://localhost:5173/auth/google/callback".to_string(),
        }
    }

    /// Creates a "Work" task for `u-1` on the seeded taxonomy and returns its
    /// view (the persisted row is reachable through `tasks`).
    fn work_task(
        lists: &FakeTaskListRepo,
        categories: &FakeTaskCategoryRepo,
        tasks: &FakeTaskRepo,
    ) -> TaskView {
        pollster::block_on(create_task(lists, categories, tasks, "u-1", &input("Work")))
            .unwrap()
            .task
    }

    /// Creates the SpicyHome child under the seeded Work root for `u-1` with
    /// the single `^.* [|] SpicyHome$` pattern, returning its category view.
    /// `pattern_calendar`/`category_calendar` set the child's pattern /
    /// category calendar ids (`None` for absent) — the inheritance-chain
    /// fixture shared by the calendar-pick tests.
    fn spicyhome_child(
        lists: &FakeTaskListRepo,
        categories: &FakeTaskCategoryRepo,
        pattern_calendar: Option<&str>,
        category_calendar: Option<&str>,
    ) -> crate::categories::CategoryView {
        let ids = category_ids_by_slug(categories);
        pollster::block_on(crate::categories::create_category(
            categories,
            lists,
            "u-1",
            &NewTaskCategoryInput {
                title: "SpicyHome".to_string(),
                slug: None,
                color: "#2a5c8a".to_string(),
                is_productive: None,
                google_calendar_id: category_calendar.map(str::to_string),
                google_color_id: None,
                list_id: None,
                parent_id: Some(ids["work"].clone()),
                sort_order: None,
                is_untracked: None,
                patterns: vec![NewTaskCategoryPattern {
                    regex: "^.* [|] SpicyHome$".to_string(),
                    google_calendar_id: pattern_calendar.map(str::to_string),
                }],
            },
        ))
        .unwrap()
        .category
    }

    /// The Google `events.insert` echo for `task_id`, `start` and `end` — the
    /// exact shape Google returns (including the shared carrier).
    fn created_event_json(task_id: &str, start: &str, end: &str) -> String {
        format!(
            r#"{{"id":"g-1","summary":"Work","start":{{"dateTime":"{start}"}},"end":{{"dateTime":"{end}"}},"extendedProperties":{{"shared":{{"sanctuary_task_id":"{task_id}"}}}}}}"#
        )
    }

    /// The `events.patch` echo: same event with a new end.
    fn patched_event_json(task_id: &str, start: &str, end: &str) -> String {
        format!(
            r#"{{"id":"g-1","summary":"Work","start":{{"dateTime":"{start}"}},"end":{{"dateTime":"{end}"}},"extendedProperties":{{"shared":{{"sanctuary_task_id":"{task_id}"}}}}}}"#
        )
    }

    /// `patched_event_json` for a specific Google event id (the multi-user
    /// elongate tests need more than the fixed `g-1`).
    fn patched_event_json_for(google_id: &str, task_id: &str, start: &str, end: &str) -> String {
        format!(
            r#"{{"id":"{google_id}","summary":"Work","start":{{"dateTime":"{start}"}},"end":{{"dateTime":"{end}"}},"extendedProperties":{{"shared":{{"sanctuary_task_id":"{task_id}"}}}}}}"#
        )
    }

    /// A cached `calendar_events` row the elongate cron resolves through a
    /// task's `started` log.
    fn cached_event(
        calendar_id: &str,
        google_id: &str,
        task_id: &str,
        start: &str,
        end: &str,
    ) -> CalendarEvent {
        CalendarEvent {
            id: "evt-1".to_string(),
            calendar_id: calendar_id.to_string(),
            google_event_id: google_id.to_string(),
            google_etag: String::new(),
            google_updated_at: String::new(),
            last_synced_at: "2026-08-18T00:00:00Z".to_string(),
            title: "Work".to_string(),
            description: String::new(),
            start_time: start.to_string(),
            end_time: end.to_string(),
            recurrence: String::new(),
            task_id: task_id.to_string(),
            created_at: "2026-08-18T00:00:00Z".to_string(),
            updated_at: "2026-08-18T00:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    /// Inserts the `started` log row that ties `task_id` to a cached event —
    /// the event identity the exit verbs and the elongate cron resolve.
    fn log_started(
        logs: &FakeTaskLogRepo,
        task_id: &str,
        user_id: &str,
        at: &str,
        calendar_id: &str,
        google_id: &str,
    ) {
        pollster::block_on(logs.insert(
            NewTaskLog {
                task_id: task_id.to_string(),
                user_id: user_id.to_string(),
                r#type: TASK_LOG_STARTED.to_string(),
                at: at.to_string(),
                calendar_id: Some(calendar_id.to_string()),
                google_event_id: Some(google_id.to_string()),
            },
            at,
        ))
        .unwrap();
    }

    // ──────────────────────────────────────────
    // start_task
    // ──────────────────────────────────────────

    #[test]
    fn start_opens_event_with_carrier_and_marks_in_progress() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();

        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);

        let response = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        // Status flipped and the response carries the created event.
        assert_eq!(response.task.status, TASK_STATUS_IN_PROGRESS);
        let event = response.event.expect("start always returns the event");
        assert_eq!(event.google_event_id, "g-1");
        assert_eq!(event.task_id, task.id, "carrier mapped onto the cache row");
        // Timer Google writes snap to the nearest minute: 22:13:20 → 22:13:00.
        assert_eq!(event.start_time, NOW_SNAPPED);
        assert_eq!(event.end_time, NOW_END, "snapped now + 15 min");

        // The POST body carries the same snapped window.
        let (_, body) = http.posts.lock().unwrap().first().unwrap().clone();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["start"]["dateTime"], NOW_SNAPPED);
        assert_eq!(body["end"]["dateTime"], NOW_END);
        // Summary is the title EXACTLY — no `| Category` suffix.
        assert_eq!(body["summary"], "Work");
        assert_eq!(
            body["extendedProperties"]["shared"]["sanctuary_task_id"],
            task.id
        );
        assert!(body.get("private").is_none(), "carrier is shared, not private");
        // The matched Work category's stored color (seed hex #2a5c8a → "9").
        assert_eq!(body["colorId"], "9");

        // `started` log with the event's calendar + google ids.
        let inserted = logs.inserted.lock().unwrap().clone();
        assert_eq!(inserted.len(), 1);
        assert_eq!(inserted[0].r#type, TASK_LOG_STARTED);
        assert_eq!(inserted[0].task_id, task.id);
        assert_eq!(inserted[0].at, NOW);
        assert_eq!(inserted[0].calendar_id.as_deref(), Some("cal-primary@example.com"));
        assert_eq!(inserted[0].google_event_id.as_deref(), Some("g-1"));
    }

    #[test]
    fn start_uses_category_calendar_when_writable() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        // Point the Work category at a non-primary calendar (direct store
        // mutation — the fake's `update` is a stub).
        {
            let mut stored = categories.stored.lock().unwrap();
            let work = stored
                .iter_mut()
                .find(|row| row.slug == "work" && row.user_id == "u-1")
                .unwrap();
            work.google_calendar_id = Some("work@example.com".to_string());
        }
        let calendars = FakeCalendarRepo::with(vec![
            calendar("primary@example.com", true),
            calendar("work@example.com", false),
        ]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();

        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);

        let response = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        let (url, _) = http.posts.lock().unwrap().first().unwrap().clone();
        assert!(url.contains("work%40example.com/events"), "{url}");
        assert_eq!(
            logs.inserted.lock().unwrap()[0].calendar_id.as_deref(),
            Some("cal-work@example.com")
        );
        assert_eq!(response.task.status, TASK_STATUS_IN_PROGRESS);
    }

    #[test]
    fn start_uses_first_matching_pattern_calendar_over_category_and_parent() {
        // The SpicyHome case: the child has no category calendar, the parent
        // root names one, and the matching pattern carries the destination.
        // The pattern slot wins the inheritance chain.
        let (lists, categories, tasks) = seeded();
        {
            let mut stored = categories.stored.lock().unwrap();
            stored
                .iter_mut()
                .find(|row| row.slug == "work" && row.user_id == "u-1")
                .unwrap()
                .google_calendar_id = Some("work@example.com".to_string());
        }
        spicyhome_child(&lists, &categories, Some("spicy@example.com"), None);
        let task = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Test | SpicyHome"),
        ))
        .unwrap()
        .task;

        // All three named calendars exist and are writable, so the pattern
        // win is unambiguous.
        let calendars = FakeCalendarRepo::with(vec![
            calendar("primary@example.com", true),
            calendar("work@example.com", false),
            calendar("spicy@example.com", false),
        ]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);

        let response = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        let (url, _) = http.posts.lock().unwrap().first().unwrap().clone();
        assert!(url.contains("spicy%40example.com/events"), "{url}");
        assert_eq!(
            logs.inserted.lock().unwrap()[0].calendar_id.as_deref(),
            Some("cal-spicy@example.com")
        );
        assert_eq!(response.task.status, TASK_STATUS_IN_PROGRESS);
    }

    #[test]
    fn start_uses_category_calendar_when_matching_pattern_has_none() {
        // The matching pattern names no calendar (its None is taken as-is),
        // so the chain falls to the child category.
        let (lists, categories, tasks) = seeded();
        {
            let mut stored = categories.stored.lock().unwrap();
            stored
                .iter_mut()
                .find(|row| row.slug == "work" && row.user_id == "u-1")
                .unwrap()
                .google_calendar_id = Some("work@example.com".to_string());
        }
        spicyhome_child(&lists, &categories, None, Some("child@example.com"));
        let task = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Test | SpicyHome"),
        ))
        .unwrap()
        .task;

        let calendars = FakeCalendarRepo::with(vec![
            calendar("primary@example.com", true),
            calendar("work@example.com", false),
            calendar("child@example.com", false),
        ]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);

        let response = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        let (url, _) = http.posts.lock().unwrap().first().unwrap().clone();
        assert!(url.contains("child%40example.com/events"), "{url}");
        assert_eq!(
            logs.inserted.lock().unwrap()[0].calendar_id.as_deref(),
            Some("cal-child@example.com")
        );
        assert_eq!(response.task.status, TASK_STATUS_IN_PROGRESS);
    }

    #[test]
    fn start_uses_parent_calendar_when_child_and_pattern_have_none() {
        // Neither the matching pattern nor the child category names a
        // calendar, so the chain falls to the parent root.
        let (lists, categories, tasks) = seeded();
        {
            let mut stored = categories.stored.lock().unwrap();
            stored
                .iter_mut()
                .find(|row| row.slug == "work" && row.user_id == "u-1")
                .unwrap()
                .google_calendar_id = Some("work@example.com".to_string());
        }
        spicyhome_child(&lists, &categories, None, None);
        let task = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Test | SpicyHome"),
        ))
        .unwrap()
        .task;

        let calendars = FakeCalendarRepo::with(vec![
            calendar("primary@example.com", true),
            calendar("work@example.com", false),
        ]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);

        let response = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        let (url, _) = http.posts.lock().unwrap().first().unwrap().clone();
        assert!(url.contains("work%40example.com/events"), "{url}");
        assert_eq!(
            logs.inserted.lock().unwrap()[0].calendar_id.as_deref(),
            Some("cal-work@example.com")
        );
        assert_eq!(response.task.status, TASK_STATUS_IN_PROGRESS);
    }

    #[test]
    fn start_takes_first_matching_pattern_even_when_it_has_no_calendar() {
        // Two patterns both match "Work": sort 0 names no calendar, sort 1
        // names one. The first MATCH wins — its None falls through to the
        // category, never ahead to sort 1.
        let (lists, categories, tasks) = seeded();
        let ids = category_ids_by_slug(&categories);
        pollster::block_on(categories.replace_patterns(
            &ids["work"],
            vec![
                NewTaskCategoryPattern {
                    regex: "^Work$".to_string(),
                    google_calendar_id: None,
                },
                NewTaskCategoryPattern {
                    regex: "^.*Work.*$".to_string(),
                    google_calendar_id: Some("later@example.com".to_string()),
                },
            ],
        ))
        .unwrap();
        {
            let mut stored = categories.stored.lock().unwrap();
            stored
                .iter_mut()
                .find(|row| row.slug == "work" && row.user_id == "u-1")
                .unwrap()
                .google_calendar_id = Some("cat@example.com".to_string());
        }
        let task = work_task(&lists, &categories, &tasks);

        let calendars = FakeCalendarRepo::with(vec![
            calendar("primary@example.com", true),
            calendar("cat@example.com", false),
            calendar("later@example.com", false),
        ]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);

        let response = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        let (url, _) = http.posts.lock().unwrap().first().unwrap().clone();
        assert!(url.contains("cat%40example.com/events"), "{url}");
        assert_eq!(
            logs.inserted.lock().unwrap()[0].calendar_id.as_deref(),
            Some("cal-cat@example.com")
        );
        assert_eq!(response.task.status, TASK_STATUS_IN_PROGRESS);
    }

    #[test]
    fn start_unwritable_pattern_calendar_jumps_to_primary_not_next_slot() {
        // The pattern names a reader-only calendar; the child category names
        // a writable one. The named destination is used as-is or NOT AT ALL:
        // a read-only pattern calendar goes to primary, never walking on to
        // the category slot.
        let (lists, categories, tasks) = seeded();
        spicyhome_child(&lists, &categories, Some("spicy@example.com"), Some("child@example.com"));
        let task = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Test | SpicyHome"),
        ))
        .unwrap()
        .task;

        let calendars = FakeCalendarRepo::with(vec![
            calendar("primary@example.com", true),
            GoogleCalendar {
                access_role: "reader".to_string(),
                ..calendar("spicy@example.com", false)
            },
            calendar("child@example.com", false),
        ]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);

        let response = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        let (url, _) = http.posts.lock().unwrap().first().unwrap().clone();
        assert!(url.contains("primary%40example.com/events"), "{url}");
        assert_eq!(
            logs.inserted.lock().unwrap()[0].calendar_id.as_deref(),
            Some("cal-primary@example.com")
        );
        assert_eq!(response.task.status, TASK_STATUS_IN_PROGRESS);
    }

    #[test]
    fn start_omits_color_id_when_category_has_none() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        // Erase the stored color (direct store mutation — the fake's update
        // is a stub), same pattern as the writable-calendar test.
        {
            let mut stored = categories.stored.lock().unwrap();
            let work = stored
                .iter_mut()
                .find(|row| row.slug == "work" && row.user_id == "u-1")
                .unwrap();
            work.google_color_id = None;
        }
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();

        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);

        let response = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(response.task.status, TASK_STATUS_IN_PROGRESS);
        let (_, body) = http.posts.lock().unwrap().first().unwrap().clone();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(
            body.get("colorId").is_none(),
            "no stored color → the event insert omits colorId"
        );
    }

    #[test]
    fn start_falls_back_to_primary_when_category_calendar_missing_or_readonly() {
        // Missing: the category names a calendar we never imported.
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        {
            let mut stored = categories.stored.lock().unwrap();
            stored
                .iter_mut()
                .find(|row| row.slug == "work" && row.user_id == "u-1")
                .unwrap()
                .google_calendar_id = Some("never-imported@example.com".to_string());
        }
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);
        let response = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();
        let (url, _) = http.posts.lock().unwrap().first().unwrap().clone();
        assert!(url.contains("primary%40example.com/events"), "{url}");
        assert_eq!(response.task.status, TASK_STATUS_IN_PROGRESS);

        // Readonly: the category calendar exists but the user only reads it.
        let calendars = FakeCalendarRepo::with(vec![
            calendar("primary@example.com", true),
            GoogleCalendar {
                access_role: "reader".to_string(),
                ..calendar("work@example.com", false)
            },
        ]);
        // Fresh event/log fakes: the previous scenario left an event running.
        // Status is the lock now, so the row's IN_PROGRESS from scenario 1 is
        // reset first (the equivalent of the old cache-freshness reset).
        pollster::block_on(tasks.set_status(&task.id, TASK_STATUS_OPEN, NOW)).unwrap();
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);
        let response = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();
        let (url, _) = http.posts.lock().unwrap().first().unwrap().clone();
        assert!(url.contains("primary%40example.com/events"), "{url}");
        assert_eq!(response.task.status, TASK_STATUS_IN_PROGRESS);
    }

    #[test]
    fn start_without_any_writable_calendar_is_invalid() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![GoogleCalendar {
            access_role: "reader".to_string(),
            ..calendar("primary@example.com", true)
        }]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();

        let http = FakeHttp::new(vec![]);
        let err = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "no writable calendar"));
        assert!(http.posts.lock().unwrap().is_empty(), "no Google call");
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&task.id)).unwrap().unwrap().status,
            TASK_STATUS_OPEN,
            "status untouched"
        );
    }

    #[test]
    fn start_while_another_task_runs_is_conflict_and_never_inserts() {
        let (lists, categories, tasks) = seeded();
        let first = work_task(&lists, &categories, &tasks);
        let second = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Fitness"),
        ))
        .unwrap()
        .task;
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();

        // First start succeeds (event g-1 opens).
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&first.id, NOW_SNAPPED, NOW_END),
        )]);
        pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &first.id, NOW_UNIX,
        ))
        .unwrap();

        // Second start hits the one-running-task rule → 409.
        let http = FakeHttp::new(vec![]);
        let err = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &second.id, NOW_UNIX,
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Conflict), "got {err:?}");
        assert_eq!(http.posts.lock().unwrap().len(), 0, "no insert attempted");
        assert_eq!(events.upserted.lock().unwrap().len(), 1, "only the first event");
        assert_eq!(logs.inserted.lock().unwrap().len(), 1, "only the first start logged");
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&second.id)).unwrap().unwrap().status,
            TASK_STATUS_OPEN
        );
    }

    #[test]
    fn start_while_same_task_runs_is_also_conflict() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();

        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);
        pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        let http = FakeHttp::new(vec![]);
        let err = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Conflict));
    }

    #[test]
    fn start_conflicts_via_status_lock_even_without_running_event() {
        // The one-running lock is `tasks.status == IN_PROGRESS`, never the
        // event window: a task whose status says IN_PROGRESS but that has no
        // living timed event (stale/missing cache) still blocks a second
        // start.
        let (lists, categories, tasks) = seeded();
        let first = work_task(&lists, &categories, &tasks);
        let second = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Fitness"),
        ))
        .unwrap()
        .task;
        // Mark IN_PROGRESS directly — no event cache, no started log.
        pollster::block_on(tasks.set_status(&first.id, TASK_STATUS_IN_PROGRESS, NOW)).unwrap();

        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![]);
        let err = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &second.id, NOW_UNIX,
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Conflict), "got {err:?}");
        assert!(http.posts.lock().unwrap().is_empty(), "no insert attempted");
        assert!(events.upserted.lock().unwrap().is_empty(), "nothing cached");
        assert!(logs.inserted.lock().unwrap().is_empty(), "nothing logged");
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&second.id)).unwrap().unwrap().status,
            TASK_STATUS_OPEN
        );
    }

    #[test]
    fn start_completed_task_opens_a_new_event() {
        // ADR 0002: nothing is terminal — starting a COMPLETED task opens a
        // NEW event (the reopen-by-start chapter); history stays.
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();

        // Mark COMPLETED directly (complete_task needs Google fakes; the repo
        // fake's `set_status` is the same statement the service uses).
        pollster::block_on(tasks.set_status(&task.id, TASK_STATUS_COMPLETED, NOW)).unwrap();

        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);
        let response = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(response.task.status, TASK_STATUS_IN_PROGRESS);
        let event = response.event.expect("start on a completed task opens a NEW event");
        assert_eq!(event.google_event_id, "g-1");
        assert_eq!(event.task_id, task.id);
        assert_eq!(logs.inserted.lock().unwrap().last().unwrap().r#type, TASK_LOG_STARTED);
        assert!(http.posts.lock().unwrap().len() == 1, "exactly the new insert");
    }

    #[test]
    fn start_discarded_task_opens_a_new_event() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        pollster::block_on(tasks.set_status(&task.id, TASK_STATUS_DISCARDED, NOW)).unwrap();

        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);
        let response = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(response.task.status, TASK_STATUS_IN_PROGRESS);
        let event = response.event.expect("start on a discarded task opens a NEW event");
        assert_eq!(event.google_event_id, "g-1");
        assert_eq!(event.task_id, task.id);
        assert_eq!(logs.inserted.lock().unwrap().last().unwrap().r#type, TASK_LOG_STARTED);
    }

    #[test]
    fn start_missing_or_other_users_task_is_not_found() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![]);

        assert!(matches!(
            pollster::block_on(start_task(
                &http, &calendars, &events, &lists, &categories, &tasks, &logs,
                &access(), "u-2", &task.id, NOW_UNIX,
            )),
            Err(TasksError::NotFound)
        ));
        assert!(matches!(
            pollster::block_on(start_task(
                &http, &calendars, &events, &lists, &categories, &tasks, &logs,
                &access(), "u-1", "nope", NOW_UNIX,
            )),
            Err(TasksError::NotFound)
        ));
    }

    #[test]
    fn start_google_error_surfaces_as_google_api_error() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();

        let http = FakeHttp::new(vec![("/events", 400, r#"{"error":"invalid"}"#)]);
        let err = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::GoogleApi(_)), "got {err:?}");
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&task.id)).unwrap().unwrap().status,
            TASK_STATUS_OPEN,
            "status untouched on google failure"
        );
        assert!(logs.inserted.lock().unwrap().is_empty());
    }

    // ──────────────────────────────────────────
    // stop / pause
    // ──────────────────────────────────────────

    #[test]
    fn stop_patches_end_and_returns_open() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);
        pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        // Stop 5 minutes later (22:18:20 → snapped 22:18:00).
        let stop_unix = NOW_UNIX + 300;
        let http = FakeHttp::new(vec![(
            "/events/g-1",
            200,
            &patched_event_json(&task.id, NOW_SNAPPED, "2023-11-14T22:18:00Z"),
        )]);
        let response = pollster::block_on(stop_task(
            &http, &calendars, &events, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, stop_unix,
        ))
        .unwrap();

        assert_eq!(response.task.status, TASK_STATUS_OPEN);
        let event = response.event.expect("stop returns the patched event");
        assert_eq!(event.end_time, "2023-11-14T22:18:00Z", "PATCH end snaps");

        let patches = http.patches.lock().unwrap();
        assert_eq!(patches.len(), 1);
        let (url, body) = patches.first().unwrap().clone();
        assert!(url.contains("primary%40example.com/events/g-1"), "{url}");
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["end"]["dateTime"], "2023-11-14T22:18:00Z");

        let inserted = logs.inserted.lock().unwrap().clone();
        assert_eq!(inserted.len(), 2, "started then stopped");
        assert_eq!(inserted[1].r#type, TASK_LOG_STOPPED);
        assert_eq!(inserted[1].at, "2023-11-14T22:18:00Z", "log at the patched end");
    }

    #[test]
    fn stop_when_now_equals_start_clamps_end_to_start_plus_60s() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);
        pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        // Stop at 22:13:20 (20s after the snapped 22:13:00 start): snapped
        // T = 22:13:00 <= start → end = start + 60s = 22:14:00 (the invert
        // guard, on the minute grid).
        let http = FakeHttp::new(vec![(
            "/events/g-1",
            200,
            &patched_event_json(&task.id, NOW_SNAPPED, "2023-11-14T22:14:00Z"),
        )]);
        let response = pollster::block_on(stop_task(
            &http, &calendars, &events, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        let event = response.event.expect("patched event returned");
        assert_eq!(event.end_time, "2023-11-14T22:14:00Z", "start + 60s");
        let (_, body) = http.patches.lock().unwrap().first().unwrap().clone();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["end"]["dateTime"], "2023-11-14T22:14:00Z");
        assert_eq!(
            logs.inserted.lock().unwrap()[1].at,
            "2023-11-14T22:14:00Z"
        );
    }

    #[test]
    fn stop_without_open_event_still_flips_status_to_open() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        // No event was ever opened for this task.
        let http = FakeHttp::new(vec![]);

        let response = pollster::block_on(stop_task(
            &http, &calendars, &events, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(response.task.status, TASK_STATUS_OPEN, "idempotent flip");
        assert!(response.event.is_none());
        assert!(http.patches.lock().unwrap().is_empty(), "no Google call");
        let inserted = logs.inserted.lock().unwrap().clone();
        assert_eq!(inserted.len(), 1);
        assert_eq!(inserted[0].r#type, TASK_LOG_STOPPED);
        assert!(inserted[0].calendar_id.is_none());
    }

    #[test]
    fn stop_after_event_window_elapsed_still_patches_via_started_log() {
        // The old exit path scanned `list_running` (`start <= now < end`), so
        // a stop after the duration elapsed found no event and skipped the
        // PATCH. The exit now resolves the event through the task's latest
        // `started` log — the run closes even when the cached end is in the
        // past.
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);
        pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        // Expire the cached window: the event ended 10 minutes ago.
        {
            let mut stored = events.stored.lock().unwrap();
            let event = stored.iter_mut().find(|row| row.task_id == task.id).unwrap();
            event.end_time = "2023-11-14T22:03:00Z".to_string();
        }

        // Stop at 22:13:20: snapped T = 22:13:00 == start → end = start + 60
        // = 22:14:00 — the PATCH happens regardless of the stale end.
        let http = FakeHttp::new(vec![(
            "/events/g-1",
            200,
            &patched_event_json(&task.id, NOW_SNAPPED, "2023-11-14T22:14:00Z"),
        )]);
        let response = pollster::block_on(stop_task(
            &http, &calendars, &events, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(response.task.status, TASK_STATUS_OPEN);
        let patches = http.patches.lock().unwrap();
        assert_eq!(patches.len(), 1, "exit PATCHes via the started log");
        let (_, body) = patches.first().unwrap().clone();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["end"]["dateTime"], "2023-11-14T22:14:00Z");
        let inserted = logs.inserted.lock().unwrap().clone();
        assert_eq!(inserted[1].r#type, TASK_LOG_STOPPED);
        assert_eq!(inserted[1].calendar_id.as_deref(), Some("cal-primary@example.com"));
        assert_eq!(inserted[1].google_event_id.as_deref(), Some("g-1"));
    }

    #[test]
    fn pause_patches_end_and_logs_paused() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);
        pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        // Pause 10 minutes later (22:23:20 → snapped 22:23:00).
        let pause_unix = NOW_UNIX + 600;
        let http = FakeHttp::new(vec![(
            "/events/g-1",
            200,
            &patched_event_json(&task.id, NOW_SNAPPED, "2023-11-14T22:23:00Z"),
        )]);
        let response = pollster::block_on(pause_task(
            &http, &calendars, &events, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, pause_unix,
        ))
        .unwrap();

        // ADR 0002: pause parks the task in the Planned pile, not Backlog.
        assert_eq!(response.task.status, TASK_STATUS_PLANNED);
        assert_eq!(response.event.unwrap().end_time, "2023-11-14T22:23:00Z", "PATCH end snaps");
        let inserted = logs.inserted.lock().unwrap().clone();
        assert_eq!(inserted[1].r#type, TASK_LOG_PAUSED, "{inserted:?}");
        // Ending the event also frees the one-running slot; a new start works
        // (start is allowed from any status since the board slice).
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, "2023-11-14T22:23:00Z", "2023-11-14T22:38:00Z"),
        )]);
        let restarted = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, pause_unix,
        ))
        .unwrap();
        assert_eq!(restarted.task.status, TASK_STATUS_IN_PROGRESS);
    }

    #[test]
    fn stop_on_terminal_task_is_invalid() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        pollster::block_on(tasks.set_status(&task.id, TASK_STATUS_COMPLETED, NOW)).unwrap();

        let http = FakeHttp::new(vec![]);
        let err = pollster::block_on(stop_task(
            &http, &calendars, &events, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(_)), "got {err:?}");
    }

    // ──────────────────────────────────────────
    // complete / discard
    // ──────────────────────────────────────────

    #[test]
    fn complete_while_running_patches_then_completes_with_both_logs() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);
        pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        let complete_unix = NOW_UNIX + 300;
        let http = FakeHttp::new(vec![(
            "/events/g-1",
            200,
            &patched_event_json(&task.id, NOW_SNAPPED, "2023-11-14T22:18:00Z"),
        )]);
        let response = pollster::block_on(complete_task(
            &http, &calendars, &events, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, complete_unix,
        ))
        .unwrap();

        assert_eq!(response.task.status, TASK_STATUS_COMPLETED);
        assert_eq!(response.event.unwrap().end_time, "2023-11-14T22:18:00Z", "PATCH end snaps");
        assert_eq!(http.patches.lock().unwrap().len(), 1, "auto-stop patched");

        // Log order: started, stopped, completed.
        let inserted = logs.inserted.lock().unwrap().clone();
        let types: Vec<&str> = inserted.iter().map(|log| log.r#type.as_str()).collect();
        assert_eq!(types, vec!["started", "stopped", "completed"], "{inserted:?}");
        assert_eq!(inserted[1].calendar_id.as_deref(), Some("cal-primary@example.com"));
        assert_eq!(inserted[1].google_event_id.as_deref(), Some("g-1"));
    }

    #[test]
    fn complete_while_not_running_completes_without_google_call() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        // Task never started — complete has nothing to stop.
        let http = FakeHttp::new(vec![]);

        let response = pollster::block_on(complete_task(
            &http, &calendars, &events, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(response.task.status, TASK_STATUS_COMPLETED);
        assert!(response.event.is_none());
        assert!(http.patches.lock().unwrap().is_empty(), "no Google call");
        let inserted = logs.inserted.lock().unwrap().clone();
        let types: Vec<&str> = inserted.iter().map(|log| log.r#type.as_str()).collect();
        assert_eq!(types, vec!["completed"], "{inserted:?}");
    }

    #[test]
    fn complete_already_completed_is_idempotent_200() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();

        let http = FakeHttp::new(vec![]);
        pollster::block_on(complete_task(
            &http, &calendars, &events, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();
        // Second complete: no-op, still 200.
        let response = pollster::block_on(complete_task(
            &http, &calendars, &events, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();
        assert_eq!(response.task.status, TASK_STATUS_COMPLETED);
        assert_eq!(logs.inserted.lock().unwrap().len(), 1, "no extra log");
    }

    #[test]
    fn discard_while_running_patches_then_discards_with_both_logs() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);
        pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        let discard_unix = NOW_UNIX + 300;
        let http = FakeHttp::new(vec![(
            "/events/g-1",
            200,
            &patched_event_json(&task.id, NOW_SNAPPED, "2023-11-14T22:18:00Z"),
        )]);
        let response = pollster::block_on(discard_task(
            &http, &calendars, &events, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, discard_unix,
        ))
        .unwrap();

        assert_eq!(response.task.status, TASK_STATUS_DISCARDED);
        let inserted = logs.inserted.lock().unwrap().clone();
        let types: Vec<&str> = inserted.iter().map(|log| log.r#type.as_str()).collect();
        assert_eq!(types, vec!["started", "stopped", "discarded"]);
        assert_eq!(http.patches.lock().unwrap().len(), 1);
    }

    #[test]
    fn discard_while_not_running_discards_without_google_call() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![]);

        let response = pollster::block_on(discard_task(
            &http, &calendars, &events, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(response.task.status, TASK_STATUS_DISCARDED);
        assert!(http.patches.lock().unwrap().is_empty());
        assert_eq!(logs.inserted.lock().unwrap()[0].r#type, TASK_LOG_DISCARDED);
    }

    #[test]
    fn complete_and_discard_are_not_found_for_other_users() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![]);
        let access = access();

        let complete_other = complete_task(
            &http, &calendars, &events, &categories, &tasks, &logs, &access, "u-2",
            &task.id, NOW_UNIX,
        );
        assert!(matches!(pollster::block_on(complete_other), Err(TasksError::NotFound)));
        let discard_other = discard_task(
            &http, &calendars, &events, &categories, &tasks, &logs, &access, "u-2",
            &task.id, NOW_UNIX,
        );
        assert!(matches!(pollster::block_on(discard_other), Err(TasksError::NotFound)));
        let complete_missing = complete_task(
            &http, &calendars, &events, &categories, &tasks, &logs, &access, "u-1",
            "nope", NOW_UNIX,
        );
        assert!(matches!(pollster::block_on(complete_missing), Err(TasksError::NotFound)));
    }

    // ──────────────────────────────────────────
    // run_elongate_cron
    // ──────────────────────────────────────────

    /// Marks `task_id` IN_PROGRESS and attaches the started log + cached
    /// event (calendar `cal-primary@example.com`, google event `g-1`) — the
    /// elongate cron's work item. `tz` is the calendar's `time_zone` column,
    /// `end` the stored event end.
    fn elongate_setup(
        tasks: &FakeTaskRepo,
        task_id: &str,
        tz: &str,
        end: &str,
    ) -> (FakeCalendarRepo, FakeEventRepo, FakeTaskLogRepo) {
        pollster::block_on(tasks.set_status(task_id, TASK_STATUS_IN_PROGRESS, NOW)).unwrap();
        let mut cal = calendar("primary@example.com", true);
        cal.time_zone = tz.to_string();
        let calendars = FakeCalendarRepo::with(vec![cal]);
        let events = FakeEventRepo::new();
        events.stored.lock().unwrap().push(cached_event(
            "cal-primary@example.com",
            "g-1",
            task_id,
            NOW_SNAPPED,
            end,
        ));
        let logs = FakeTaskLogRepo::default();
        log_started(&logs, task_id, "u-1", NOW, "cal-primary@example.com", "g-1");
        (calendars, events, logs)
    }

    /// Runs `run_elongate_cron` with the standard fresh token for `u-1` and
    /// one in-progress task; returns the report.
    fn elongate(
        http: &FakeHttp,
        calendars: &FakeCalendarRepo,
        events: &FakeEventRepo,
        logs: &FakeTaskLogRepo,
        tasks: &FakeTaskRepo,
        tokens: &FakeTokenRepo,
        now_unix: i64,
    ) -> ElongateReport {
        pollster::block_on(run_elongate_cron(
            http, calendars, events, logs, tasks, tokens, &oauth_config(), now_unix,
        ))
    }

    #[test]
    fn elongate_patches_end_when_event_fell_behind_the_target() {
        // A 15-minute event started 20 minutes ago (end 22:08:00 is past)
        // while the task stayed IN_PROGRESS: the cron extends it.
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let (calendars, events, logs) =
            elongate_setup(&tasks, &task.id, "UTC", "2023-11-14T22:08:00Z");
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let http = FakeHttp::new(vec![(
            "/events/g-1",
            200,
            &patched_event_json(&task.id, NOW_SNAPPED, "2023-11-14T22:20:00Z"),
        )]);

        let report = elongate(&http, &calendars, &events, &logs, &tasks, &tokens, NOW_UNIX);

        assert_eq!(report.elongated, 1);
        assert_eq!(report.skipped, 0);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        // now 22:13:20 + 5 min = 22:18:20 → ceil 22:20:00Z, persisted UTC Z.
        let (url, body) = http.patches.lock().unwrap().first().unwrap().clone();
        assert!(url.contains("primary%40example.com/events/g-1"), "{url}");
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["end"]["dateTime"], "2023-11-14T22:20:00Z");
        // The Google echo landed in the cache with the new end.
        assert_eq!(events.stored.lock().unwrap()[0].end_time, "2023-11-14T22:20:00Z");
        // Status untouched — the exit verbs own the flip.
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&task.id)).unwrap().unwrap().status,
            TASK_STATUS_IN_PROGRESS
        );
    }

    #[test]
    fn elongate_skips_when_event_end_still_covers_target() {
        // First minutes of a 15-minute task: the planned end (22:28:00) is
        // still ahead of the target (22:20:00) — nothing to extend.
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let (calendars, events, logs) =
            elongate_setup(&tasks, &task.id, "UTC", "2023-11-14T22:28:00Z");
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        // No routes: any HTTP call would panic "no route for …".
        let http = FakeHttp::new(vec![]);

        let report = elongate(&http, &calendars, &events, &logs, &tasks, &tokens, NOW_UNIX);

        assert_eq!(report.elongated, 0);
        assert_eq!(report.skipped, 1);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(http.patches.lock().unwrap().is_empty(), "no PATCH");
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&task.id)).unwrap().unwrap().status,
            TASK_STATUS_IN_PROGRESS
        );
    }

    #[test]
    fn elongate_never_shrinks_a_previously_elongated_end() {
        // A previous elongation pushed the end to 23:30:00 (far beyond the
        // current target) — the cron must not pull it back.
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let (calendars, events, logs) =
            elongate_setup(&tasks, &task.id, "UTC", "2023-11-14T23:30:00Z");
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let http = FakeHttp::new(vec![]);

        let report = elongate(&http, &calendars, &events, &logs, &tasks, &tokens, NOW_UNIX);

        assert_eq!(report.elongated, 0);
        assert_eq!(report.skipped, 1, "never shrink");
        assert!(http.patches.lock().unwrap().is_empty(), "no PATCH");
        assert_eq!(
            events.stored.lock().unwrap()[0].end_time,
            "2023-11-14T23:30:00Z",
            "cached end untouched"
        );
    }

    #[test]
    fn elongate_skips_task_without_started_log() {
        // The task never started (no `started` log, no event): skip — do not
        // recreate the event, do not flip status.
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        pollster::block_on(tasks.set_status(&task.id, TASK_STATUS_IN_PROGRESS, NOW)).unwrap();
        let calendars = FakeCalendarRepo::with(vec![]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let http = FakeHttp::new(vec![]);

        let report = elongate(&http, &calendars, &events, &logs, &tasks, &tokens, NOW_UNIX);

        assert_eq!(report.elongated, 0);
        assert_eq!(report.skipped, 1);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(http.patches.lock().unwrap().is_empty(), "no HTTP");
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&task.id)).unwrap().unwrap().status,
            TASK_STATUS_IN_PROGRESS,
            "status untouched"
        );
    }

    #[test]
    fn elongate_skips_when_cached_event_is_missing() {
        // The `started` log names an event that is gone from the cache (the
        // user deleted it in Google): skip, never recreate.
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        pollster::block_on(tasks.set_status(&task.id, TASK_STATUS_IN_PROGRESS, NOW)).unwrap();
        let calendars = FakeCalendarRepo::with(vec![]);
        let events = FakeEventRepo::new(); // no cached row
        let logs = FakeTaskLogRepo::default();
        log_started(&logs, &task.id, "u-1", NOW, "cal-primary@example.com", "g-1");
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let http = FakeHttp::new(vec![]);

        let report = elongate(&http, &calendars, &events, &logs, &tasks, &tokens, NOW_UNIX);

        assert_eq!(report.elongated, 0);
        assert_eq!(report.skipped, 1);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(http.patches.lock().unwrap().is_empty(), "no HTTP");
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&task.id)).unwrap().unwrap().status,
            TASK_STATUS_IN_PROGRESS
        );
    }

    #[test]
    fn elongate_skips_unparseable_end_time() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let (calendars, events, logs) =
            elongate_setup(&tasks, &task.id, "UTC", "not-a-date");
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let http = FakeHttp::new(vec![]);

        let report = elongate(&http, &calendars, &events, &logs, &tasks, &tokens, NOW_UNIX);

        assert_eq!(report.elongated, 0);
        assert_eq!(report.skipped, 1);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(http.patches.lock().unwrap().is_empty(), "no HTTP");
    }

    #[test]
    fn elongate_skips_on_google_404_and_keeps_status() {
        // The event vanished on Google's side: 404 → skip (never recreate,
        // never flip status).
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let (calendars, events, logs) =
            elongate_setup(&tasks, &task.id, "UTC", "2023-11-14T22:08:00Z");
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let http = FakeHttp::new(vec![("/events/g-1", 404, r#"{"error":"not found"}"#)]);

        let report = elongate(&http, &calendars, &events, &logs, &tasks, &tokens, NOW_UNIX);

        assert_eq!(report.elongated, 0);
        assert_eq!(report.skipped, 1, "404 is a skip, not an error");
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&task.id)).unwrap().unwrap().status,
            TASK_STATUS_IN_PROGRESS
        );
    }

    #[test]
    fn elongate_uses_calendar_time_zone() {
        // Asia/Kolkata's +05:30 (19_800s = 66 * 300) is a multiple of 5
        // minutes, so the IST 5-minute grid is numerically the same set of
        // instants as UTC's — the zone path (offset → local ceil → convert
        // back) must not shift the target.
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let (calendars, events, logs) =
            elongate_setup(&tasks, &task.id, "Asia/Kolkata", "2023-11-14T22:08:00Z");
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let http = FakeHttp::new(vec![(
            "/events/g-1",
            200,
            &patched_event_json(&task.id, NOW_SNAPPED, "2023-11-14T22:20:00Z"),
        )]);

        let report = elongate(&http, &calendars, &events, &logs, &tasks, &tokens, NOW_UNIX);

        assert_eq!(report.elongated, 1);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let (_, body) = http.patches.lock().unwrap().first().unwrap().clone();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        // 22:18:20Z + 05:30 = 03:48:20 IST (next day) → ceil 03:50:00 IST =
        // 22:20:00Z — the same instant as the UTC ceil (the offset is a
        // multiple of 5 minutes, so both grids share the same instants).
        assert_eq!(body["end"]["dateTime"], "2023-11-14T22:20:00Z");
    }

    #[test]
    fn elongate_target_with_empty_calendar_tz_falls_back_to_utc() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let (calendars, events, logs) =
            elongate_setup(&tasks, &task.id, "", "2023-11-14T22:08:00Z");
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]);
        let http = FakeHttp::new(vec![(
            "/events/g-1",
            200,
            &patched_event_json(&task.id, NOW_SNAPPED, "2023-11-14T22:20:00Z"),
        )]);

        let report = elongate(&http, &calendars, &events, &logs, &tasks, &tokens, NOW_UNIX);

        assert_eq!(report.elongated, 1, "empty TZ behaves as UTC");
        let (_, body) = http.patches.lock().unwrap().first().unwrap().clone();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["end"]["dateTime"], "2023-11-14T22:20:00Z");
    }

    #[test]
    fn elongate_token_failure_for_one_user_does_not_abort_the_rest() {
        let (lists, categories, tasks) = seeded();
        let task_a = work_task(&lists, &categories, &tasks);
        // u-2's task inserts straight into the repo (no taxonomy needed).
        let task_b = pollster::block_on(tasks.insert(NewTask {
            user_id: "u-2".to_string(),
            title: "Fitness".to_string(),
            description: String::new(),
            duration_minutes: DEFAULT_DURATION_MINUTES,
            priority: "medium".to_string(),
            difficulty: "easy".to_string(),
            sort_order: 0,
        }))
        .unwrap();
        for task_id in [&task_a.id, &task_b.id] {
            pollster::block_on(tasks.set_status(task_id, TASK_STATUS_IN_PROGRESS, NOW)).unwrap();
        }
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        events.stored.lock().unwrap().push(cached_event(
            "cal-primary@example.com",
            "g-1",
            &task_a.id,
            NOW_SNAPPED,
            "2023-11-14T22:08:00Z",
        ));
        events.stored.lock().unwrap().push(cached_event(
            "cal-primary@example.com",
            "g-2",
            &task_b.id,
            NOW_SNAPPED,
            "2023-11-14T22:08:00Z",
        ));
        let logs = FakeTaskLogRepo::default();
        log_started(&logs, &task_a.id, "u-1", NOW, "cal-primary@example.com", "g-1");
        log_started(&logs, &task_b.id, "u-2", NOW, "cal-primary@example.com", "g-2");
        // u-1 has NO token → its refresh fails; u-2 proceeds.
        let tokens = FakeTokenRepo::with(vec![fresh_token("u-2", "at-b")]);
        let http = FakeHttp::new(vec![(
            "/events/g-2",
            200,
            &patched_event_json_for("g-2", &task_b.id, NOW_SNAPPED, "2023-11-14T22:20:00Z"),
        )]);

        let report = elongate(&http, &calendars, &events, &logs, &tasks, &tokens, NOW_UNIX);

        assert_eq!(report.elongated, 1, "u-2's event extended despite u-1's failure");
        assert_eq!(report.skipped, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("u-1"), "{}", report.errors[0]);
        let patches = http.patches.lock().unwrap();
        assert_eq!(patches.len(), 1, "only u-2's event patched");
        assert!(patches[0].0.contains("g-2"), "{}", patches[0].0);
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&task_a.id)).unwrap().unwrap().status,
            TASK_STATUS_IN_PROGRESS
        );
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&task_b.id)).unwrap().unwrap().status,
            TASK_STATUS_IN_PROGRESS
        );
    }

    #[test]
    fn elongate_extends_both_dirty_in_progress_tasks() {
        // Two IN_PROGRESS rows at once (dirty data — status is the lock, not
        // an event window): both elongate.
        let (lists, categories, tasks) = seeded();
        let task_a = work_task(&lists, &categories, &tasks);
        let task_b = pollster::block_on(tasks.insert(NewTask {
            user_id: "u-2".to_string(),
            title: "Fitness".to_string(),
            description: String::new(),
            duration_minutes: DEFAULT_DURATION_MINUTES,
            priority: "medium".to_string(),
            difficulty: "easy".to_string(),
            sort_order: 0,
        }))
        .unwrap();
        for task_id in [&task_a.id, &task_b.id] {
            pollster::block_on(tasks.set_status(task_id, TASK_STATUS_IN_PROGRESS, NOW)).unwrap();
        }
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        events.stored.lock().unwrap().push(cached_event(
            "cal-primary@example.com",
            "g-1",
            &task_a.id,
            NOW_SNAPPED,
            "2023-11-14T22:08:00Z",
        ));
        events.stored.lock().unwrap().push(cached_event(
            "cal-primary@example.com",
            "g-2",
            &task_b.id,
            NOW_SNAPPED,
            "2023-11-14T22:08:00Z",
        ));
        let logs = FakeTaskLogRepo::default();
        log_started(&logs, &task_a.id, "u-1", NOW, "cal-primary@example.com", "g-1");
        log_started(&logs, &task_b.id, "u-2", NOW, "cal-primary@example.com", "g-2");
        let tokens = FakeTokenRepo::with(vec![
            fresh_token("u-1", "at-a"),
            fresh_token("u-2", "at-b"),
        ]);
        let http = FakeHttp::new(vec![
            (
                "/events/g-1",
                200,
                &patched_event_json(&task_a.id, NOW_SNAPPED, "2023-11-14T22:20:00Z"),
            ),
            (
                "/events/g-2",
                200,
                &patched_event_json_for("g-2", &task_b.id, NOW_SNAPPED, "2023-11-14T22:20:00Z"),
            ),
        ]);

        let report = elongate(&http, &calendars, &events, &logs, &tasks, &tokens, NOW_UNIX);

        assert_eq!(report.elongated, 2, "both IN_PROGRESS rows grow");
        assert_eq!(report.skipped, 0);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(http.patches.lock().unwrap().len(), 2);
    }

    // ──────────────────────────────────────────
    // move_task (ADR 0002 § Move API)
    // ──────────────────────────────────────────

    /// Calls `move_task` for `u-1` at `NOW_UNIX` with no displace.
    fn move_to(
        http: &FakeHttp,
        calendars: &FakeCalendarRepo,
        events: &FakeEventRepo,
        lists: &FakeTaskListRepo,
        categories: &FakeTaskCategoryRepo,
        tasks: &FakeTaskRepo,
        logs: &FakeTaskLogRepo,
        access: Option<&GoogleAccess>,
        task_id: &str,
        status: &str,
        sort_order: i64,
    ) -> Result<MoveTaskResponse, TasksError> {
        pollster::block_on(move_task(
            Some(http),
            calendars,
            events,
            lists,
            categories,
            tasks,
            logs,
            access,
            "u-1",
            task_id,
            NOW_UNIX,
            &MoveTaskInput {
                status: status.to_string(),
                sort_order,
                displace: None,
            },
        ))
    }

    /// Starts `task_id` against the default Google stack (writable primary
    /// calendar, one `/events` route) and returns the fresh fakes — the
    /// running-task setup behind the exit/displace tests.
    fn start_running(
        lists: &FakeTaskListRepo,
        categories: &FakeTaskCategoryRepo,
        tasks: &FakeTaskRepo,
        task_id: &str,
    ) -> (FakeHttp, FakeCalendarRepo, FakeEventRepo, FakeTaskLogRepo) {
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(task_id, NOW_SNAPPED, NOW_END),
        )]);
        pollster::block_on(start_task(
            &http, &calendars, &events, lists, categories, tasks, &logs,
            &access(), "u-1", task_id, NOW_UNIX,
        ))
        .unwrap();
        (http, calendars, events, logs)
    }

    #[test]
    fn move_open_to_planned_is_plan() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let peer = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![]);

        // Park the peer in PLANNED first (session-only, no Google).
        let parked = move_to(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs, None,
            &peer.id, TASK_STATUS_PLANNED, 0,
        )
        .unwrap();
        assert_eq!(parked.task.status, TASK_STATUS_PLANNED);

        // Plan `task` at the front: the PLANNED peer shifts up to 1.
        let response = move_to(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs, None,
            &task.id, TASK_STATUS_PLANNED, 0,
        )
        .unwrap();
        assert_eq!(response.task.status, TASK_STATUS_PLANNED);
        assert_eq!(response.task.sort_order, 0);
        assert!(response.displaced.is_none());
        assert!(response.event.is_none());

        let stored = tasks.stored.lock().unwrap();
        assert_eq!(stored.iter().find(|row| row.id == task.id).unwrap().sort_order, 0);
        assert_eq!(
            stored.iter().find(|row| row.id == peer.id).unwrap().sort_order,
            1,
            "PLANNED peer shifted up by the insertion"
        );
        drop(stored);

        assert!(http.posts.lock().unwrap().is_empty(), "plan touches no Google");
        assert!(http.patches.lock().unwrap().is_empty(), "plan touches no Google");
        let inserted = logs.inserted.lock().unwrap().clone();
        assert_eq!(inserted.last().unwrap().r#type, TASK_LOG_PLANNED, "{inserted:?}");
    }

    #[test]
    fn move_planned_to_open_is_unplan() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![]);

        let planned = move_to(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs, None,
            &task.id, TASK_STATUS_PLANNED, 0,
        )
        .unwrap();
        assert_eq!(planned.task.status, TASK_STATUS_PLANNED);

        let response = move_to(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs, None,
            &task.id, TASK_STATUS_OPEN, 0,
        )
        .unwrap();
        assert_eq!(response.task.status, TASK_STATUS_OPEN);
        assert_eq!(response.task.sort_order, 0);
        assert!(response.event.is_none());
        let inserted = logs.inserted.lock().unwrap().clone();
        assert_eq!(inserted.last().unwrap().r#type, TASK_LOG_UNPLANNED, "{inserted:?}");
        assert!(http.posts.lock().unwrap().is_empty(), "unplan touches no Google");
    }

    #[test]
    fn move_completed_to_planned_is_reopen() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        pollster::block_on(tasks.set_status(&task.id, TASK_STATUS_COMPLETED, NOW)).unwrap();
        let http = FakeHttp::new(vec![]);

        let response = move_to(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs, None,
            &task.id, TASK_STATUS_PLANNED, 0,
        )
        .unwrap();
        assert_eq!(response.task.status, TASK_STATUS_PLANNED);
        let inserted = logs.inserted.lock().unwrap().clone();
        assert_eq!(inserted.last().unwrap().r#type, TASK_LOG_REOPENED, "{inserted:?}");
        assert!(http.posts.lock().unwrap().is_empty(), "reopen touches no Google");
        assert!(http.patches.lock().unwrap().is_empty());
    }

    #[test]
    fn move_open_to_in_progress_is_start() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW_SNAPPED, NOW_END),
        )]);

        let response = move_to(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            Some(&access()), &task.id, TASK_STATUS_IN_PROGRESS, 0,
        )
        .unwrap();
        assert_eq!(response.task.status, TASK_STATUS_IN_PROGRESS);
        let event = response.event.expect("start leg returns the created event");
        assert_eq!(event.task_id, task.id);
        assert!(response.displaced.is_none());
        let inserted = logs.inserted.lock().unwrap().clone();
        assert_eq!(inserted.last().unwrap().r#type, TASK_LOG_STARTED, "{inserted:?}");
    }

    #[test]
    fn move_in_progress_to_planned_is_pause() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let (_start_http, calendars, events, logs) =
            start_running(&lists, &categories, &tasks, &task.id);
        drop(_start_http);

        let pause_unix = NOW_UNIX + 600;
        let http = FakeHttp::new(vec![(
            "/events/g-1",
            200,
            &patched_event_json(&task.id, NOW_SNAPPED, "2023-11-14T22:23:00Z"),
        )]);
        let response = pollster::block_on(move_task(
            Some(&http),
            &calendars,
            &events,
            &lists,
            &categories,
            &tasks,
            &logs,
            Some(&access()),
            "u-1",
            &task.id,
            pause_unix,
            &MoveTaskInput {
                status: TASK_STATUS_PLANNED.to_string(),
                sort_order: 0,
                displace: None,
            },
        ))
        .unwrap();

        assert_eq!(response.task.status, TASK_STATUS_PLANNED);
        let event = response.event.expect("pause leg patches and returns the event");
        assert_eq!(event.end_time, "2023-11-14T22:23:00Z", "PATCH end snaps");
        assert_eq!(http.patches.lock().unwrap().len(), 1, "exit from IN_PROGRESS patches");
        let inserted = logs.inserted.lock().unwrap().clone();
        assert_eq!(inserted.last().unwrap().r#type, TASK_LOG_PAUSED, "{inserted:?}");
    }

    #[test]
    fn move_in_progress_to_open_is_stop() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let (_start_http, calendars, events, logs) =
            start_running(&lists, &categories, &tasks, &task.id);
        drop(_start_http);

        let stop_unix = NOW_UNIX + 300;
        let http = FakeHttp::new(vec![(
            "/events/g-1",
            200,
            &patched_event_json(&task.id, NOW_SNAPPED, "2023-11-14T22:18:00Z"),
        )]);
        let response = pollster::block_on(move_task(
            Some(&http),
            &calendars,
            &events,
            &lists,
            &categories,
            &tasks,
            &logs,
            Some(&access()),
            "u-1",
            &task.id,
            stop_unix,
            &MoveTaskInput {
                status: TASK_STATUS_OPEN.to_string(),
                sort_order: 4,
                displace: None,
            },
        ))
        .unwrap();

        assert_eq!(response.task.status, TASK_STATUS_OPEN);
        assert_eq!(response.task.sort_order, 4, "stop then place at the request rank");
        assert!(response.event.is_some(), "stop leg returns the patched event");
        let inserted = logs.inserted.lock().unwrap().clone();
        assert_eq!(inserted.last().unwrap().r#type, TASK_LOG_STOPPED, "{inserted:?}");
    }

    #[test]
    fn move_to_in_progress_without_displace_conflicts() {
        let (lists, categories, tasks) = seeded();
        let first = work_task(&lists, &categories, &tasks);
        let second = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Fitness"),
        ))
        .unwrap()
        .task;
        let (_start_http, calendars, events, logs) =
            start_running(&lists, &categories, &tasks, &first.id);
        drop(_start_http);

        let http = FakeHttp::new(vec![]);
        let err = pollster::block_on(move_task(
            Some(&http),
            &calendars,
            &events,
            &lists,
            &categories,
            &tasks,
            &logs,
            Some(&access()),
            "u-1",
            &second.id,
            NOW_UNIX,
            &MoveTaskInput {
                status: TASK_STATUS_IN_PROGRESS.to_string(),
                sort_order: 0,
                displace: None,
            },
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Conflict), "got {err:?}");
        assert!(http.posts.lock().unwrap().is_empty(), "no insert attempted");
        assert_eq!(events.upserted.lock().unwrap().len(), 1, "only the first event");
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&second.id)).unwrap().unwrap().status,
            TASK_STATUS_OPEN
        );
    }

    #[test]
    fn move_to_in_progress_with_displace_parks_then_starts() {
        let (lists, categories, tasks) = seeded();
        let a = work_task(&lists, &categories, &tasks);
        let b = work_task(&lists, &categories, &tasks);
        let (_start_http, calendars, events, logs) =
            start_running(&lists, &categories, &tasks, &a.id);
        drop(_start_http);

        // Route order matters: the patch URL contains "/events" too. A's park
        // PATCHes end to 22:14:00 (the invert guard: snapped T = 22:13:00 ==
        // A.start → end = A.start + 60); B starts at the same instant on the
        // minute grid (rule: A.end == B.start).
        let http = FakeHttp::new(vec![
            (
                "/events/g-1",
                200,
                &patched_event_json(&a.id, NOW_SNAPPED, "2023-11-14T22:14:00Z"),
            ),
            (
                "/events",
                200,
                &created_event_json(&b.id, "2023-11-14T22:14:00Z", "2023-11-14T22:29:00Z"),
            ),
        ]);
        let response = pollster::block_on(move_task(
            Some(&http),
            &calendars,
            &events,
            &lists,
            &categories,
            &tasks,
            &logs,
            Some(&access()),
            "u-1",
            &b.id,
            NOW_UNIX,
            &MoveTaskInput {
                status: TASK_STATUS_IN_PROGRESS.to_string(),
                sort_order: 0,
                displace: Some(DisplaceInput {
                    id: a.id.clone(),
                    status: TASK_STATUS_PLANNED.to_string(),
                    sort_order: 0,
                }),
            },
        ))
        .unwrap();

        assert_eq!(response.task.id, b.id);
        assert_eq!(response.task.status, TASK_STATUS_IN_PROGRESS);
        assert_eq!(response.task.sort_order, 0);
        let displaced = response.displaced.expect("A is returned as displaced");
        assert_eq!(displaced.id, a.id);
        assert_eq!(displaced.status, TASK_STATUS_PLANNED);
        assert_eq!(displaced.sort_order, 0);
        let event = response.event.expect("B's new event");
        assert_eq!(event.task_id, b.id);

        // Persisted state: A parked, B running, one patch + one insert.
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&a.id)).unwrap().unwrap().status,
            TASK_STATUS_PLANNED
        );
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&b.id)).unwrap().unwrap().status,
            TASK_STATUS_IN_PROGRESS
        );
        assert_eq!(http.patches.lock().unwrap().len(), 1, "A's event closed");
        assert_eq!(http.posts.lock().unwrap().len(), 1, "B's event inserted");
        let inserted = logs.inserted.lock().unwrap().clone();
        let types: Vec<&str> = inserted.iter().map(|log| log.r#type.as_str()).collect();
        assert_eq!(types, vec!["started", "paused", "started"], "{inserted:?}");
    }

    #[test]
    fn move_displace_then_start_failure_keeps_displaced() {
        let (lists, categories, tasks) = seeded();
        let a = work_task(&lists, &categories, &tasks);
        let b = work_task(&lists, &categories, &tasks);
        let (_start_http, calendars, events, logs) =
            start_running(&lists, &categories, &tasks, &a.id);
        drop(_start_http);

        // A's park patch succeeds (echo end = 22:14:00 — the invert guard on
        // the minute grid); B's start insert then fails with a Google 400.
        let http = FakeHttp::new(vec![
            (
                "/events/g-1",
                200,
                &patched_event_json(&a.id, NOW_SNAPPED, "2023-11-14T22:14:00Z"),
            ),
            ("/events", 400, r#"{"error":"invalid"}"#),
        ]);
        let err = pollster::block_on(move_task(
            Some(&http),
            &calendars,
            &events,
            &lists,
            &categories,
            &tasks,
            &logs,
            Some(&access()),
            "u-1",
            &b.id,
            NOW_UNIX,
            &MoveTaskInput {
                status: TASK_STATUS_IN_PROGRESS.to_string(),
                sort_order: 0,
                displace: Some(DisplaceInput {
                    id: a.id.clone(),
                    status: TASK_STATUS_PLANNED.to_string(),
                    sort_order: 0,
                }),
            },
        ))
        .unwrap_err();
        let TasksError::AfterDisplace { displaced, source } = err else {
            panic!("expected AfterDisplace, got {err:?}");
        };
        assert_eq!(displaced.id, a.id, "the parked task is reported");
        assert_eq!(displaced.status, TASK_STATUS_PLANNED);
        let message = match source.as_ref() {
            TasksError::GoogleApi(message) => message,
            other => panic!("inner error is {other:?}"),
        };
        assert_eq!(message, "google events.insert returned 400");

        // A STAYS parked (no rollback); B never started; nothing is running.
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&a.id)).unwrap().unwrap().status,
            TASK_STATUS_PLANNED,
            "displaced task stays parked after the failed start"
        );
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&b.id)).unwrap().unwrap().status,
            TASK_STATUS_OPEN
        );
        assert_eq!(events.upserted.lock().unwrap().len(), 1, "only A's closed-event echo");
        let inserted = logs.inserted.lock().unwrap().clone();
        assert_eq!(inserted[1].r#type, TASK_LOG_PAUSED, "{inserted:?}");
        assert!(
            pollster::block_on(events.list_running_by_user_id(
                "u-1",
                &unix_secs_to_rfc3339(NOW_UNIX + 360),
            ))
            .unwrap()
            .is_empty(),
            "A's event is closed, nothing runs"
        );
    }

    #[test]
    fn move_displace_wrong_id_is_invalid() {
        let (lists, categories, tasks) = seeded();
        let a = work_task(&lists, &categories, &tasks);
        let b = work_task(&lists, &categories, &tasks);
        let (_start_http, calendars, events, logs) =
            start_running(&lists, &categories, &tasks, &a.id);
        drop(_start_http);

        // `displace.id` = the moved task itself, which is NOT running → 400.
        let http = FakeHttp::new(vec![]);
        let err = pollster::block_on(move_task(
            Some(&http),
            &calendars,
            &events,
            &lists,
            &categories,
            &tasks,
            &logs,
            Some(&access()),
            "u-1",
            &b.id,
            NOW_UNIX,
            &MoveTaskInput {
                status: TASK_STATUS_IN_PROGRESS.to_string(),
                sort_order: 0,
                displace: Some(DisplaceInput {
                    id: b.id.clone(),
                    status: TASK_STATUS_PLANNED.to_string(),
                    sort_order: 0,
                }),
            },
        ))
        .unwrap_err();
        let message = match &err {
            TasksError::Invalid(message) => message.as_str(),
            other => panic!("expected the locked displace error, got {other:?}"),
        };
        assert_eq!(message, "displace id is not the running task");
        assert!(http.posts.lock().unwrap().is_empty(), "nothing dispatched");
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&a.id)).unwrap().unwrap().status,
            TASK_STATUS_IN_PROGRESS,
            "A untouched"
        );
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&b.id)).unwrap().unwrap().status,
            TASK_STATUS_OPEN,
            "B untouched"
        );

        // With nothing running at all, any displace id fails the same way.
        let (lists2, categories2, tasks2) = seeded();
        let c = work_task(&lists2, &categories2, &tasks2);
        let calendars2 = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events2 = FakeEventRepo::new();
        let logs2 = FakeTaskLogRepo::default();
        let err = pollster::block_on(move_task(
            Some(&http),
            &calendars2,
            &events2,
            &lists2,
            &categories2,
            &tasks2,
            &logs2,
            Some(&access()),
            "u-1",
            &c.id,
            NOW_UNIX,
            &MoveTaskInput {
                status: TASK_STATUS_IN_PROGRESS.to_string(),
                sort_order: 0,
                displace: Some(DisplaceInput {
                    id: "nope".to_string(),
                    status: TASK_STATUS_PLANNED.to_string(),
                    sort_order: 0,
                }),
            },
        ))
        .unwrap_err();
        let message = match &err {
            TasksError::Invalid(message) => message.as_str(),
            other => panic!("expected the locked displace error, got {other:?}"),
        };
        assert_eq!(message, "displace id is not the running task");
    }

    #[test]
    fn move_displace_succeeds_when_running_events_end_is_in_the_past() {
        // The production 400 this slice fixes: A's stored status is
        // IN_PROGRESS but its cached event window already lapsed, so the old
        // `list_running` check rejected the displace. Status is the lock now:
        // the displace proceeds and A's run closes via its started log.
        let (lists, categories, tasks) = seeded();
        let a = work_task(&lists, &categories, &tasks);
        let b = work_task(&lists, &categories, &tasks);
        let (_start_http, calendars, events, logs) =
            start_running(&lists, &categories, &tasks, &a.id);
        drop(_start_http);

        // Expire A's cached event end (as if the original duration passed
        // while the row stayed IN_PROGRESS) — `list_running` would not find
        // it anymore.
        {
            let mut stored = events.stored.lock().unwrap();
            let event = stored.iter_mut().find(|row| row.task_id == a.id).unwrap();
            event.end_time = "2023-11-14T21:00:00Z".to_string();
        }

        // A's park PATCHes end to 22:14:00 (invert guard: snapped T =
        // 22:13:00 == the snapped A.start → end = A.start + 60); B starts at
        // the same instant.
        let http = FakeHttp::new(vec![
            (
                "/events/g-1",
                200,
                &patched_event_json(&a.id, NOW_SNAPPED, "2023-11-14T22:14:00Z"),
            ),
            (
                "/events",
                200,
                &created_event_json(&b.id, "2023-11-14T22:14:00Z", "2023-11-14T22:29:00Z"),
            ),
        ]);
        let response = pollster::block_on(move_task(
            Some(&http),
            &calendars,
            &events,
            &lists,
            &categories,
            &tasks,
            &logs,
            Some(&access()),
            "u-1",
            &b.id,
            NOW_UNIX,
            &MoveTaskInput {
                status: TASK_STATUS_IN_PROGRESS.to_string(),
                sort_order: 0,
                displace: Some(DisplaceInput {
                    id: a.id.clone(),
                    status: TASK_STATUS_PLANNED.to_string(),
                    sort_order: 0,
                }),
            },
        ))
        .unwrap();

        assert_eq!(response.task.id, b.id);
        assert_eq!(response.task.status, TASK_STATUS_IN_PROGRESS);
        assert_eq!(
            response.displaced.expect("A is returned as displaced").id,
            a.id
        );
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&a.id)).unwrap().unwrap().status,
            TASK_STATUS_PLANNED,
            "A parked despite the expired window"
        );
        assert_eq!(
            pollster::block_on(tasks.get_by_id(&b.id)).unwrap().unwrap().status,
            TASK_STATUS_IN_PROGRESS
        );
        assert_eq!(http.patches.lock().unwrap().len(), 1, "A's event closed via the log");
        assert_eq!(http.posts.lock().unwrap().len(), 1, "B's event inserted");
    }

    #[test]
    fn move_same_column_reorder_shifts_neighbors() {
        let (lists, categories, tasks) = seeded();
        // Create C then B then A: create prepends, so A=0, B=1, C=2.
        let c = work_task(&lists, &categories, &tasks);
        let b = work_task(&lists, &categories, &tasks);
        let a = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![]);

        // Drag A (rank 0) down to rank 2: the peers in (0, 2] shift down one.
        let response = move_to(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs, None,
            &a.id, TASK_STATUS_OPEN, 2,
        )
        .unwrap();
        assert_eq!(response.task.sort_order, 2);
        let stored = tasks.stored.lock().unwrap();
        let rank = |id: &str| stored.iter().find(|row| row.id == id).unwrap().sort_order;
        assert_eq!(rank(&a.id), 2);
        assert_eq!(rank(&b.id), 0, "B shifts down one");
        assert_eq!(rank(&c.id), 1, "C shifts down one");
        drop(stored);

        // And back up: A (rank 2) to rank 0 — peers in [0, 2) shift up one.
        let response = move_to(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs, None,
            &a.id, TASK_STATUS_OPEN, 0,
        )
        .unwrap();
        assert_eq!(response.task.sort_order, 0);
        let stored = tasks.stored.lock().unwrap();
        assert_eq!(stored.iter().find(|row| row.id == a.id).unwrap().sort_order, 0);
        assert_eq!(stored.iter().find(|row| row.id == b.id).unwrap().sort_order, 1);
        assert_eq!(stored.iter().find(|row| row.id == c.id).unwrap().sort_order, 2);
        drop(stored);

        // Reorder is log-free and Google-free.
        assert!(logs.inserted.lock().unwrap().is_empty(), "reorder logs nothing");
        assert!(http.posts.lock().unwrap().is_empty());
        assert!(http.patches.lock().unwrap().is_empty());
    }

    #[test]
    fn move_in_progress_to_in_progress_is_noop() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let (_start_http, calendars, events, logs) =
            start_running(&lists, &categories, &tasks, &task.id);
        drop(_start_http);

        let http = FakeHttp::new(vec![]);
        let response = pollster::block_on(move_task(
            Some(&http),
            &calendars,
            &events,
            &lists,
            &categories,
            &tasks,
            &logs,
            None, // the no-op never dispatches, so even Google-less works
            "u-1",
            &task.id,
            NOW_UNIX,
            &MoveTaskInput {
                status: TASK_STATUS_IN_PROGRESS.to_string(),
                sort_order: 99,
                displace: None,
            },
        ))
        .unwrap();
        assert_eq!(response.task.id, task.id);
        assert_eq!(response.task.status, TASK_STATUS_IN_PROGRESS);
        assert_eq!(response.task.sort_order, 0, "sort_order ignored by the no-op");
        assert!(response.displaced.is_none());
        assert!(response.event.is_none());
        assert_eq!(logs.inserted.lock().unwrap().len(), 1, "only the original start log");
        assert!(http.posts.lock().unwrap().is_empty());
        assert!(http.patches.lock().unwrap().is_empty());
    }

    #[test]
    fn move_validates_status_rank_and_displace() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![]);

        let unknown = MoveTaskInput {
            status: "GONE".to_string(),
            sort_order: 0,
            displace: None,
        };
        let err = pollster::block_on(move_task(
            Some(&http), &calendars, &events, &lists, &categories, &tasks, &logs, None,
            "u-1", &task.id, NOW_UNIX, &unknown,
        ))
        .unwrap_err();
        let message = match &err {
            TasksError::Invalid(message) => message.as_str(),
            other => panic!("expected the unknown-status error, got {other:?}"),
        };
        assert_eq!(message, "unknown task status");

        let negative = MoveTaskInput {
            status: TASK_STATUS_OPEN.to_string(),
            sort_order: -1,
            displace: None,
        };
        let err = pollster::block_on(move_task(
            Some(&http), &calendars, &events, &lists, &categories, &tasks, &logs, None,
            "u-1", &task.id, NOW_UNIX, &negative,
        ))
        .unwrap_err();
        let message = match &err {
            TasksError::Invalid(message) => message.as_str(),
            other => panic!("expected the negative-rank error, got {other:?}"),
        };
        assert_eq!(message, "sort_order must not be negative");

        // displace.status is locked to PLANNED/COMPLETED/DISCARDED.
        let bad_displace = MoveTaskInput {
            status: TASK_STATUS_IN_PROGRESS.to_string(),
            sort_order: 0,
            displace: Some(DisplaceInput {
                id: "x".to_string(),
                status: TASK_STATUS_OPEN.to_string(),
                sort_order: 0,
            }),
        };
        let err = pollster::block_on(move_task(
            Some(&http), &calendars, &events, &lists, &categories, &tasks, &logs, None,
            "u-1", &task.id, NOW_UNIX, &bad_displace,
        ))
        .unwrap_err();
        let message = match &err {
            TasksError::Invalid(message) => message.as_str(),
            other => panic!("expected the displace-status error, got {other:?}"),
        };
        assert_eq!(message, "displace status must be planned, completed, or discarded");

        // displace only makes sense when the target is IN_PROGRESS.
        let misplaced = MoveTaskInput {
            status: TASK_STATUS_COMPLETED.to_string(),
            sort_order: 0,
            displace: Some(DisplaceInput {
                id: "x".to_string(),
                status: TASK_STATUS_PLANNED.to_string(),
                sort_order: 0,
            }),
        };
        let err = pollster::block_on(move_task(
            Some(&http), &calendars, &events, &lists, &categories, &tasks, &logs, None,
            "u-1", &task.id, NOW_UNIX, &misplaced,
        ))
        .unwrap_err();
        let message = match &err {
            TasksError::Invalid(message) => message.as_str(),
            other => panic!("expected the misplaced-displace error, got {other:?}"),
        };
        assert_eq!(message, "displace is only allowed when moving to in progress");

        assert_eq!(tasks.stored.lock().unwrap().len(), 1, "nothing changed");
    }

    #[test]
    fn move_missing_or_other_users_task_is_not_found() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![]);
        let input = MoveTaskInput {
            status: TASK_STATUS_OPEN.to_string(),
            sort_order: 0,
            displace: None,
        };

        assert!(matches!(
            pollster::block_on(move_task(
                Some(&http), &calendars, &events, &lists, &categories, &tasks, &logs,
                None, "u-2", &task.id, NOW_UNIX, &input,
            )),
            Err(TasksError::NotFound)
        ));
        assert!(matches!(
            pollster::block_on(move_task(
                Some(&http), &calendars, &events, &lists, &categories, &tasks, &logs,
                None, "u-1", "nope", NOW_UNIX, &input,
            )),
            Err(TasksError::NotFound)
        ));
    }
}
