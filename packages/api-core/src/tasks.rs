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
//!   title via [`crate::categories::classify`] with `event_google_calendar_id
//!   = None` (calendar-scoped patterns never match task titles).
//! - Create/update reject a title that does not uniquely match a non-untracked
//!   category (400). `Untracked { conflict: false }` (0 matches),
//!   `Untracked { conflict: true }` (cross-tree conflict), and a match on the
//!   `untracked` sink itself are all invalid. A title matching only a root
//!   whose children do not match is **allowed** (parent remainder).
//! - Titles are not unique. Create always stores `status = "OPEN"`.
//! - `duration_minutes` defaults to 15 and must be >= 1; `priority` must be
//!   `high|medium|low` (default `medium`).
//! - Delete is SOFT; a missing/soft-deleted/other-user task is always
//!   [`TasksError::NotFound`].
//! - List returns each living task with its **computed** category
//!   ([`TaskCategorySummary`]) so the client never reimplements the regex
//!   matching. A task whose title no longer matches anything (e.g. the user
//!   deleted the pattern) is still returned with the `untracked` summary —
//!   listing is a read, not a validation.
//!
//! Timer rules (locked):
//! - `start_task` opens a Google Calendar event `now … now + duration_minutes`
//!   with `extendedProperties.shared.sanctuary_task_id` = task UUID (never
//!   `private`, never a description footer). Summary is the task **title**
//!   exactly — no `| Category` suffix.
//! - Calendar pick: the matched category's `google_calendar_id` **when that
//!   calendar exists for this user, is not soft-deleted, and is writable**
//!   (`access_role` `owner` or `writer`); otherwise the user's **primary**
//!   calendar. No writable calendar → 400.
//! - One running task per user: a second start raises [`TasksError::Conflict`]
//!   (409) even when the same task is already running. "Running" is **derived
//!   from the cache** — `calendar_events.task_id` set AND
//!   `start_time <= now < end_time` — never from `tasks.status`.
//! - `stop_task`/`pause_task` PATCH the event's end to now (start + 60s when
//!   `now <= start`) and flip status to OPEN — even when no open event exists
//!   for the task (the user closed it in Google): the flip is idempotent.
//! - `complete_task`/`discard_task` auto-stop a running event first (a
//!   `stopped` log precedes the terminal log), then set the terminal status.
//!   Repeating the same terminal action is an idempotent 200 no-op.
//! - Start on COMPLETED/DISCARDED is a 400; missing/other-user/soft-deleted
//!   tasks are 404.
//! - Every transition appends to `task_logs` (an audit trail, not a
//!   timesheet): `started|stopped|paused|completed|discarded`.

use thiserror::Error;

use crate::calendar::{create_event, patch_event, CalendarError};
use crate::categories::{ensure_taxonomy, classify, CategoryWithPatterns, ClassifyOutcome};
use crate::models::{
    CalendarEvent, NewEventInput, NewTask, NewTaskInput, NewTaskLog, Task, TaskCategory,
    UpdateTask,
};
use crate::oauth::HttpClient;
use crate::repo::{
    CalendarEventRepo, CalendarRepo, RepoError, TaskCategoryRepo, TaskListRepo, TaskLogRepo,
    TaskRepo,
};
use crate::time::{rfc3339_to_unix_secs, unix_secs_to_rfc3339};
use crate::token::GoogleAccess;

/// Default planned duration for a new task, in minutes.
pub const DEFAULT_DURATION_MINUTES: i64 = 15;
/// Minimum planned duration, in minutes.
pub const MIN_DURATION_MINUTES: i64 = 1;
/// The status every created task gets.
pub const TASK_STATUS_OPEN: &str = "OPEN";
/// A running task (an open timed event exists in the cache).
pub const TASK_STATUS_IN_PROGRESS: &str = "IN_PROGRESS";
/// A finished task.
pub const TASK_STATUS_COMPLETED: &str = "COMPLETED";
/// A discarded task.
pub const TASK_STATUS_DISCARDED: &str = "DISCARDED";

/// `task_logs.type` values (the audit trail is append-only).
pub const TASK_LOG_STARTED: &str = "started";
pub const TASK_LOG_STOPPED: &str = "stopped";
pub const TASK_LOG_PAUSED: &str = "paused";
pub const TASK_LOG_COMPLETED: &str = "completed";
pub const TASK_LOG_DISCARDED: &str = "discarded";

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
/// one round-trip pair — the unit of work for every classify here.
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
///
/// `ensure_taxonomy` runs first so the very first task of a fresh user finds
/// a seeded matcher.
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

    let lists = list_repo.list_by_user_id(user_id).await?;
    let categories = category_repo.list_by_user_id(user_id).await?;
    ensure_taxonomy(list_repo, category_repo, &lists, &categories, user_id).await?;
    let taxonomy = load_taxonomy(category_repo, user_id).await?;
    // Validates the rules; the response view re-classifies the stored title.
    resolve_category(&title, &taxonomy)?;

    let task = task_repo
        .insert(NewTask {
            user_id: user_id.to_string(),
            title,
            description: input.description.as_deref().unwrap_or("").trim().to_string(),
            duration_minutes,
            priority,
        })
        .await?;
    Ok(TaskResponse {
        task: to_view(&task, &taxonomy),
    })
}

/// Updates a task's `title`/`description`/`duration_minutes`/`priority`
/// (`None` = unchanged; status is never updated this slice).
///
/// - A body with nothing to update is 400.
/// - When `title` is present it must be non-blank AND uniquely match a
///   non-untracked category (same rules as create, same messages).
/// - `duration_minutes` must be >= 1 when present; `priority` must be
///   `high|medium|low` when present.
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
/// Order of operations:
/// 1. Load the task — missing, soft-deleted, or another user's task → 404.
/// 2. COMPLETED/DISCARDED tasks cannot start → 400.
/// 3. If ANY open timed event exists for the user → 409 (`Conflict`) — even
///    when it belongs to this very task.
/// 4. Classify the title (seeding the taxonomy like every other read) to pick
///    the category; its `google_calendar_id` is the calendar *candidate*.
/// 5. Resolve the target calendar: the candidate when it exists for this
///    user and is writable (`access_role` owner/writer), else the user's
///    primary calendar. No writable calendar → 400.
/// 6. `create_event` (now … now + duration_minutes, task carrier attached).
/// 7. `tasks.status` → IN_PROGRESS and a `started` log row, both in this
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
    if task.status == TASK_STATUS_COMPLETED {
        return Err(TasksError::Invalid("cannot start a completed task".to_string()));
    }
    if task.status == TASK_STATUS_DISCARDED {
        return Err(TasksError::Invalid("cannot start a discarded task".to_string()));
    }

    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    // Running is derived from the cache, never from `tasks.status`: any open
    // timed event blocks a new start, whatever this task's stored status.
    let running = events.list_running_by_user_id(user_id, &now_rfc3339).await?;
    if !running.is_empty() {
        return Err(TasksError::Conflict);
    }

    let taxonomy = load_taxonomy_seeded(list_repo, category_repo, user_id).await?;
    let target = resolve_target_calendar(calendars, &taxonomy, &task.title, user_id).await?;

    let end_rfc3339 = unix_secs_to_rfc3339(now_unix + task.duration_minutes * 60);
    let output = create_event(
        http,
        calendars,
        events,
        access,
        &NewEventInput {
            calendar_id: target.calendar_id.clone(),
            summary: task.title.clone(),
            description: None,
            start: now_rfc3339.clone(),
            end: end_rfc3339,
            task_id: Some(task.id.clone()),
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
        now_unix, TASK_LOG_STOPPED,
    )
    .await
}

/// Pauses a task: identical to [`stop_task`] (the event's end is patched to
/// now) except the log row says `paused`. Reopening later is simply Start
/// again (logged `started`).
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
        now_unix, TASK_LOG_PAUSED,
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
        stop_running_event(http, calendars, events, access, user_id, task_id, now_unix).await?;

    let Some(updated) = task_repo
        .set_status(task_id, TASK_STATUS_OPEN, &now_rfc3339)
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
        stop_running_event(http, calendars, events, access, user_id, task_id, now_unix).await?;
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

/// Finds the task's open timed event in the user's running set and PATCHes
/// its end to now (or `start + 60s` when `now <= start`, keeping the Google
/// event valid). Returns the patched event (None when the task has no open
/// event — the user closed it in Google), the closing instant, and the
/// calendar/google ids for the log row.
async fn stop_running_event(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    access: &GoogleAccess,
    user_id: &str,
    task_id: &str,
    now_unix: i64,
) -> Result<(Option<CalendarEvent>, String, Option<String>, Option<String>), TasksError> {
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    let running = events.list_running_by_user_id(user_id, &now_rfc3339).await?;
    let Some(found) = running.into_iter().find(|event| event.task_id == task_id) else {
        return Ok((None, now_rfc3339, None, None));
    };
    let start_unix = rfc3339_to_unix_secs(&found.start_time).unwrap_or(now_unix);
    let end_unix = if now_unix <= start_unix {
        start_unix + 60
    } else {
        now_unix
    };
    let end_rfc3339 = unix_secs_to_rfc3339(end_unix);
    let output = patch_event(
        http,
        calendars,
        events,
        access,
        &found.calendar_id,
        &found.google_event_id,
        &end_rfc3339,
        now_unix,
    )
    .await?;
    Ok((
        Some(output.event),
        end_rfc3339,
        Some(found.calendar_id),
        Some(found.google_event_id),
    ))
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
/// the matched category's `google_calendar_id` when that calendar exists for
/// the user, is not soft-deleted (`list_by_user_id` already filters), and is
/// writable (`access_role` owner/writer); otherwise the user's **primary**
/// calendar (also writable). No writable calendar → 400.
async fn resolve_target_calendar(
    calendars: &dyn CalendarRepo,
    taxonomy: &Taxonomy,
    title: &str,
    user_id: &str,
) -> Result<TargetCalendar, TasksError> {
    let user_cals = calendars.list_by_user_id(user_id).await?;
    let category = match classify(title, None, &taxonomy.matchers) {
        ClassifyOutcome::Matched { category_id } => taxonomy
            .categories
            .iter()
            .find(|category| category.id == category_id),
        // A title that matches nothing (or conflicts) falls back to the
        // primary calendar: starting is a read, never a validation.
        ClassifyOutcome::Untracked { .. } => None,
    };
    let candidate = category
        .and_then(|category| category.google_calendar_id.as_deref())
        .and_then(|wanted| {
            user_cals
                .iter()
                .find(|cal| cal.google_calendar_id == wanted && is_writable(cal))
        });
    let target = candidate
        .or_else(|| {
            user_cals
                .iter()
                .find(|cal| cal.is_primary && is_writable(cal))
        })
        .ok_or_else(|| TasksError::Invalid("no writable calendar".to_string()))?;
    Ok(TargetCalendar {
        calendar_id: target.id.clone(),
    })
}

/// A resolved target calendar for a started event (local `google_calendars.id`
/// — the Google id itself lives on the row the event insert re-reads).
#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetCalendar {
    calendar_id: String,
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

fn to_view(task: &Task, taxonomy: &Taxonomy) -> TaskView {
    let outcome = classify(&task.title, None, &taxonomy.matchers);
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
    match classify(title, None, &taxonomy.matchers) {
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

/// Loads the living categories AND their patterns in one pass, mirroring the
/// two queries `list_categories` performs per category (list + patterns).
async fn load_taxonomy(
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
) -> Result<Taxonomy, RepoError> {
    let categories = category_repo.list_by_user_id(user_id).await?;
    let mut matchers = Vec::with_capacity(categories.len());
    for category in &categories {
        let patterns = category_repo.list_patterns_by_category_id(&category.id).await?;
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
        GoogleCalendar, NewCalendar, NewCalendarEvent, NewTaskCategory, NewTaskCategoryInput,
        NewTaskCategoryPattern, NewTaskList, TaskCategory, TaskCategoryPattern, TaskList,
        UpdateTaskCategory, UpdateTaskList,
    };
    use crate::oauth::{HttpClient, HttpError};
    use crate::repo::{CalendarEventRepo, CalendarRepo, TaskCategoryRepo, TaskLogRepo};

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
                (b.updated_at.as_str(), b.created_at.as_str(), &b.id)
                    .cmp(&(a.updated_at.as_str(), a.created_at.as_str(), &a.id))
            });
            Ok(rows)
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
                status: TASK_STATUS_OPEN.to_string(),
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
    }

    impl FakeTaskCategoryRepo {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                patterns: Mutex::new(HashMap::new()),
                next_id: Mutex::new(1),
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
        };
        let err = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &bad_priority,
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "priority must be one of high, medium, low"));
        assert_eq!(tasks.stored.lock().unwrap().len(), 0, "nothing persisted");
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
        };
        let response = pollster::block_on(update_task(
            &categories, &tasks, "u-1", &created.id, &updates,
        ))
        .unwrap();
        assert_eq!(response.task.description, "Deep focus session");
        assert_eq!(response.task.duration_minutes, 25);
        assert_eq!(response.task.priority, "high");
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

    const NOW_UNIX: i64 = 1_700_000_000; // 2023-11-14T22:13:20Z
    const NOW: &str = "2023-11-14T22:13:20Z";

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
            &created_event_json(
                &task.id,
                "2023-11-14T22:13:20Z",
                "2023-11-14T22:28:20Z",
            ),
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
        assert_eq!(event.start_time, NOW);
        assert_eq!(event.end_time, "2023-11-14T22:28:20Z", "now + 15 min");

        // Summary is the title EXACTLY — no `| Category` suffix.
        let (_, body) = http.posts.lock().unwrap().first().unwrap().clone();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["summary"], "Work");
        assert_eq!(
            body["extendedProperties"]["shared"]["sanctuary_task_id"],
            task.id
        );
        assert!(body.get("private").is_none(), "carrier is shared, not private");

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
            &created_event_json(&task.id, NOW, "2023-11-14T22:28:20Z"),
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
            &created_event_json(&task.id, NOW, "2023-11-14T22:28:20Z"),
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
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW, "2023-11-14T22:28:20Z"),
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
            &created_event_json(&first.id, NOW, "2023-11-14T22:28:20Z"),
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
            &created_event_json(&task.id, NOW, "2023-11-14T22:28:20Z"),
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
    fn start_completed_task_is_invalid() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();

        // Mark COMPLETED directly (complete_task needs Google fakes; the repo
        // fake's `set_status` is the same statement the service uses).
        pollster::block_on(tasks.set_status(&task.id, TASK_STATUS_COMPLETED, NOW)).unwrap();

        let http = FakeHttp::new(vec![]);
        let err = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "cannot start a completed task"));
        assert!(http.posts.lock().unwrap().is_empty());
    }

    #[test]
    fn start_discarded_task_is_invalid() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        pollster::block_on(tasks.set_status(&task.id, TASK_STATUS_DISCARDED, NOW)).unwrap();

        let http = FakeHttp::new(vec![]);
        let err = pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "cannot start a discarded task"));
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
            &created_event_json(&task.id, NOW, "2023-11-14T22:28:20Z"),
        )]);
        pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        // Stop 5 minutes later.
        let stop_unix = NOW_UNIX + 300;
        let http = FakeHttp::new(vec![(
            "/events/g-1",
            200,
            &patched_event_json(&task.id, NOW, "2023-11-14T22:18:20Z"),
        )]);
        let response = pollster::block_on(stop_task(
            &http, &calendars, &events, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, stop_unix,
        ))
        .unwrap();

        assert_eq!(response.task.status, TASK_STATUS_OPEN);
        let event = response.event.expect("stop returns the patched event");
        assert_eq!(event.end_time, "2023-11-14T22:18:20Z");

        let patches = http.patches.lock().unwrap();
        assert_eq!(patches.len(), 1);
        let (url, body) = patches.first().unwrap().clone();
        assert!(url.contains("primary%40example.com/events/g-1"), "{url}");
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["end"]["dateTime"], "2023-11-14T22:18:20Z");

        let inserted = logs.inserted.lock().unwrap().clone();
        assert_eq!(inserted.len(), 2, "started then stopped");
        assert_eq!(inserted[1].r#type, TASK_LOG_STOPPED);
        assert_eq!(inserted[1].at, "2023-11-14T22:18:20Z", "log at the patched end");
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
            &created_event_json(&task.id, NOW, "2023-11-14T22:28:20Z"),
        )]);
        pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        // Stop at the exact start instant: `now <= start` → end = start + 60s.
        let http = FakeHttp::new(vec![(
            "/events/g-1",
            200,
            &patched_event_json(&task.id, NOW, "2023-11-14T22:14:20Z"),
        )]);
        let response = pollster::block_on(stop_task(
            &http, &calendars, &events, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        let event = response.event.expect("patched event returned");
        assert_eq!(event.end_time, "2023-11-14T22:14:20Z", "start + 60s");
        let (_, body) = http.patches.lock().unwrap().first().unwrap().clone();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["end"]["dateTime"], "2023-11-14T22:14:20Z");
        assert_eq!(
            logs.inserted.lock().unwrap()[1].at,
            "2023-11-14T22:14:20Z"
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
    fn pause_patches_end_and_logs_paused() {
        let (lists, categories, tasks) = seeded();
        let task = work_task(&lists, &categories, &tasks);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        let logs = FakeTaskLogRepo::default();
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, NOW, "2023-11-14T22:28:20Z"),
        )]);
        pollster::block_on(start_task(
            &http, &calendars, &events, &lists, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, NOW_UNIX,
        ))
        .unwrap();

        let pause_unix = NOW_UNIX + 600;
        let http = FakeHttp::new(vec![(
            "/events/g-1",
            200,
            &patched_event_json(&task.id, NOW, "2023-11-14T22:23:20Z"),
        )]);
        let response = pollster::block_on(pause_task(
            &http, &calendars, &events, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, pause_unix,
        ))
        .unwrap();

        assert_eq!(response.task.status, TASK_STATUS_OPEN);
        assert_eq!(response.event.unwrap().end_time, "2023-11-14T22:23:20Z");
        let inserted = logs.inserted.lock().unwrap().clone();
        assert_eq!(inserted[1].r#type, TASK_LOG_PAUSED, "{inserted:?}");
        // Ending the event also frees the one-running slot: a new start works.
        let http = FakeHttp::new(vec![(
            "/events",
            200,
            &created_event_json(&task.id, "2023-11-14T22:23:20Z", "2023-11-14T22:38:20Z"),
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
            &created_event_json(&task.id, NOW, "2023-11-14T22:28:20Z"),
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
            &patched_event_json(&task.id, NOW, "2023-11-14T22:18:20Z"),
        )]);
        let response = pollster::block_on(complete_task(
            &http, &calendars, &events, &categories, &tasks, &logs,
            &access(), "u-1", &task.id, complete_unix,
        ))
        .unwrap();

        assert_eq!(response.task.status, TASK_STATUS_COMPLETED);
        assert_eq!(response.event.unwrap().end_time, "2023-11-14T22:18:20Z");
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
            &created_event_json(&task.id, NOW, "2023-11-14T22:28:20Z"),
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
            &patched_event_json(&task.id, NOW, "2023-11-14T22:18:20Z"),
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
}
