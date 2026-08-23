//! Agenda + occurrence service (ADR 0004): the date-scoped run-of-show and
//! its occurrence verbs, including the slice-6 Google writes: `/start`
//! creates the one-shot log, complete/skip close a running chip, the title
//! PATCH updates a chip's `summary`, and the elongate cron grows living
//! in_progress occurrence events.
//!
//! Pure Rust and unit-testable: persistence goes through [`OccurrenceRepo`] /
//! [`AgendaItemRepo`] / [`RoutineRepo`] / [`TaskRepo`] (faked with in-memory
//! impls in tests), Google HTTP through [`HttpClient`], and "now" comes from
//! the caller — never `SystemTime`. The Worker layers session checks and
//! token refresh on top (`apps/worker/src/agenda.rs`).
//!
//! Domain rules (ADR 0004, locked):
//! - `GET /api/agenda?date=` **seeds on read** (not a cron): for every living
//!   routine whose RRULE + exdates covers the date, ensure the occurrence row
//!   (idempotent via `UNIQUE (routine_id, local_date)`) and append an
//!   occurrence agenda item **after** already-present items, in standing
//!   `routines.sort_order` relative to each other. A list the user already
//!   reordered is never reshuffled. Tasks never auto-land.
//! - The response omits occurrence items whose routine is missing/
//!   soft-deleted and task items whose task is missing/soft-deleted; the
//!   orphan membership rows stay in D1 (membership is hard-deleted only by
//!   unpin).
//! - `POST /api/agenda/items` is tasks-only in v1 (400 otherwise), living
//!   `OPEN | PLANNED | IN_PROGRESS` tasks only (404 missing/other-user/
//!   soft-deleted, 400 terminal), idempotent 200 when already on the date,
//!   and never touches `tasks.status`. The body **must name the `date`**.
//! - `POST /api/agenda/items/:id/move` reorders that item's date pile only;
//!   peers at/after the insert rank shift up (no `updated_at` bump).
//! - `POST /api/agenda/items/:id/reschedule { date }` relocates a slot to
//!   another day (ADR 0004 amendment): an occurrence **appends its source
//!   date to the routine's `exdates`** (else the next GET on the source date
//!   re-seeds a fresh occurrence — the RRULE still matches), rewrites
//!   `local_date`, and moves the agenda row to the target pile appended;
//!   `pending | skipped` are the only reschedulable statuses (`skipped` →
//!   `pending` on the new date), `in_progress`/`done` → 400, and a target
//!   date already holding this routine → 400. A task reschedule moves the
//!   **membership slot** only — task status unchanged, no `task_logs` row
//!   (the agenda is an overlay; `task_logs` has no civil-date column) — and
//!   an already-on-target task unpins the source and returns the existing
//!   target item. Same date → 200 no-op. Session-only, never a Google write.
//! - `DELETE` on an occurrence-kind item is refused (400 "skip is the
//!   decline"); task-kind items are hard-deleted (unpin).
//! - `PATCH /api/occurrences/:id { title }` writes the override; `""`/
//!   whitespace clears it back to inheritance (NULL). When `google_event_id`
//!   is set and the **resolved** title actually changed, the Google event's
//!   `summary` is PATCHed too (unlike tasks — locked in the ADR). A Google
//!   404 proceeds (the chip is gone; the override still writes).
//!   Missing/other-user/soft-deleted-routine → 404.
//! - `complete`/`skip` follow the locked verb matrix. From `in_progress`
//!   **with** stored ids the open event's end is PATCHed closed (snapped to
//!   now, `start + 60s` when now <= start — the same invert guard as task
//!   exits) before the status flip; a Google 404 still flips. Without ids
//!   (or with `http`/`access` `None`) the flip is session-only.
//! - `/start` is **today-only** (`local_date == civil date of now in
//!   Asia/Kolkata`) and `pending`-only: it creates the one-shot Google log
//!   (summary = the **resolved** title, carriers
//!   `sanctuary_routine_id`/`sanctuary_occurrence_id`, never a task_id,
//!   never an RRULE, `T … T + START_EVENT_MINUTES` on the minute grid),
//!   stores `calendar_id` + `google_event_id` on the occurrence, and flips it
//!   to `in_progress`. Repeating on `in_progress` is a 200 no-op (no second
//!   event); `done`/`skipped` → 400. Calendar pick is the same inheritance as
//!   `start_task` (pattern → category → parent root → primary; missing/
//!   read-only named calendar → primary; no writable calendar → 400).
//! - The elongate cron ([`run_elongate_occurrences`]) grows every living
//!   in_progress occurrence event the same way it grows task events
//!   (`ceil_5min_unix_in_zone`, never shrink, never recreate, never flip
//!   status); token refresh per occurrence owner.
//! - Occurrence category is classified from the **resolved** title
//!   (`override ?? routine.title`) with the same matcher as tasks; a read
//!   never 400s on classification (untracked summary when nothing matches).

use std::collections::HashMap;

use thiserror::Error;

use crate::calendar::{
    create_event, patch_event, patch_event_summary, CalendarError, CreateEventOutput,
};
use crate::categories::{
    classify, ensure_taxonomy, first_matching_pattern, CalendarScope, CategoryWithPatterns,
    ClassifyOutcome,
};
use crate::config::OAuthConfig;
use crate::models::{
    AgendaItem, CalendarEvent, GoogleCalendar, NewAgendaItem, NewAgendaItemInput,
    NewEventInput, NewRoutineOccurrence, Routine, RoutineOccurrence, Task, TaskCategory,
    TaskCategoryPattern, UpdateOccurrence, UpdateRoutine,
};
use crate::oauth::HttpClient;
use crate::repo::{
    AgendaItemRepo, CalendarEventRepo, CalendarRepo, OccurrenceRepo, RepoError, RoutineRepo,
    TaskCategoryRepo, TaskListRepo, TaskRepo, TokenRepo,
};
use crate::routines::occurrence_dates;
use crate::tasks::{ElongateReport, START_EVENT_MINUTES, TaskCategorySummary, TaskView};
use crate::time::{
    ceil_5min_unix_in_zone, civil_date_in_offset, nearest_minute_unix, rfc3339_to_unix_secs,
    unix_secs_to_rfc3339,
};
use crate::token::{refresh_if_needed, GoogleAccess};

/// Occurrence states (lowercase on purpose — these are NOT task statuses).
pub const OCCURRENCE_STATUS_PENDING: &str = "pending";
pub const OCCURRENCE_STATUS_IN_PROGRESS: &str = "in_progress";
pub const OCCURRENCE_STATUS_DONE: &str = "done";
pub const OCCURRENCE_STATUS_SKIPPED: &str = "skipped";

/// Agenda item kinds.
pub const AGENDA_KIND_TASK: &str = "task";
pub const AGENDA_KIND_OCCURRENCE: &str = "occurrence";

/// Errors produced by the agenda/occurrence service. The Worker maps Invalid →
/// 400, NotFound → 404, GoogleApi → 502, Calendar (any other calendar-layer
/// surprise) and Repo → 500.
///
/// No `PartialEq`/`Eq`: the `Calendar` variant wraps [`CalendarError`] (which
/// carries non-`Eq` HTTP errors) — tests match with `matches!`, never `==`.
#[derive(Debug, Clone, Error)]
pub enum AgendaError {
    #[error("{0}")]
    Invalid(String),
    #[error("not found")]
    NotFound,
    #[error("google api error: {0}")]
    GoogleApi(String),
    #[error("calendar error: {0}")]
    Calendar(CalendarError),
    #[error("database error: {0}")]
    Repo(#[from] RepoError),
}

/// Mirrors `tasks.rs`: a Google `GoogleApi` failure surfaces its message
/// (the worker maps it to 502); any other calendar-layer error is a
/// 500-shaped surprise (missing calendar row, HTTP transport failure…).
impl From<CalendarError> for AgendaError {
    fn from(err: CalendarError) -> Self {
        match err {
            CalendarError::GoogleApi(message) => AgendaError::GoogleApi(message),
            other => AgendaError::Calendar(other),
        }
    }
}

/// `ensure_taxonomy` errors fold into [`AgendaError`] (same mapping as the
/// routines service: 400 / 404 / 500).
impl From<crate::categories::CategoriesError> for AgendaError {
    fn from(err: crate::categories::CategoriesError) -> Self {
        match err {
            crate::categories::CategoriesError::Invalid(message) => AgendaError::Invalid(message),
            crate::categories::CategoriesError::NotFound => AgendaError::NotFound,
            crate::categories::CategoriesError::Conflict(message) => AgendaError::Invalid(message),
            crate::categories::CategoriesError::Repo(err) => AgendaError::Repo(err),
        }
    }
}

/// Response envelope for `GET /api/agenda?date=…`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AgendaResponse {
    pub items: Vec<AgendaItemView>,
}

/// Response envelope for `POST /api/agenda/items` and
/// `POST /api/agenda/items/:id/move`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AgendaItemResponse {
    pub item: AgendaItemView,
}

/// Response envelope for `DELETE /api/agenda/items/:id`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeleteAgendaItemResponse {
    pub success: bool,
}

/// Response envelope for `PATCH /api/occurrences/:id`,
/// `POST /api/occurrences/:id/complete` and `POST /api/occurrences/:id/skip`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OccurrenceResponse {
    pub occurrence: OccurrenceView,
}

/// Response envelope for `POST /api/occurrences/:id/start`: the fresh
/// occurrence plus the one-shot Google log this start created (`None` on the
/// idempotent in_progress no-op — no second event was opened).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OccurrenceActionResponse {
    pub occurrence: OccurrenceView,
    pub event: Option<CalendarEvent>,
}

/// HTTP shape of one agenda membership row with its embed: the task (kind
/// `task`) or the occurrence (kind `occurrence`) — exactly one is `Some`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AgendaItemView {
    pub id: String,
    pub user_id: String,
    /// Local civil date `YYYY-MM-DD`.
    pub local_date: String,
    pub kind: String,
    pub ref_id: String,
    pub sort_order: i64,
    /// `Some` for kind=task (full `TaskView`, `focused` included).
    pub task: Option<TaskView>,
    /// `Some` for kind=occurrence.
    pub occurrence: Option<OccurrenceView>,
}

/// HTTP shape of an occurrence: every `routine_occurrences` column plus the
/// routine's display fields (`estimated_minutes`, `dtstart`/`rrule`/parsed
/// `exdates` for display), the **resolved** title and its computed category.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OccurrenceView {
    pub id: String,
    pub routine_id: String,
    pub user_id: String,
    /// Local civil date `YYYY-MM-DD`.
    pub local_date: String,
    /// Stored override; `None` = inherit the routine title.
    pub title: Option<String>,
    /// `title ?? routine.title` — the display title and the classify input.
    pub resolved_title: String,
    /// `pending | in_progress | done | skipped`.
    pub status: String,
    /// From the routine (the estimate lives on the standing definition).
    pub estimated_minutes: i64,
    /// Naive local civil datetime `YYYY-MM-DDTHH:MM:SS` (routine, for display).
    pub dtstart: String,
    /// RFC 5545 RRULE body (routine, for display).
    pub rrule: String,
    /// Parsed local `YYYY-MM-DD` exclusions (routine, for display).
    pub exdates: Vec<String>,
    /// Local calendar id once started (slice 6); `None` until then.
    pub calendar_id: Option<String>,
    /// Google event id of the one-shot log (slice 6); `None` until then.
    pub google_event_id: Option<String>,
    /// RFC 3339 instant.
    pub created_at: String,
    /// RFC 3339 instant.
    pub updated_at: String,
    /// Computed from the **resolved** title with the same matcher as tasks.
    /// A title that no longer uniquely matches keeps the `untracked` summary —
    /// a read never 400s on classification.
    pub category: TaskCategorySummary,
}

// ──────────────────────────────────────────
// GET /api/agenda — ensure-for-date, seed, respond
// ──────────────────────────────────────────

/// `GET /api/agenda?date=YYYY-MM-DD` → `{"items":[…]}`.
///
/// Seeds on read: every living routine whose rule covers `date` gets its
/// occurrence ensured (idempotent) and, when not already on the date, an
/// agenda item **appended after** already-present items (standing
/// `routines.sort_order` relative to each other). A user-reordered list is
/// never reshuffled. Tasks never auto-land.
///
/// `focused_task_id` is the caller's pointer into this user's focus lock —
/// it only paints the embedded tasks' `focused` flag (reads never write the
/// users row), the same contract as `tasks::list_tasks`.
pub async fn get_agenda(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    agenda_repo: &dyn AgendaItemRepo,
    task_repo: &dyn TaskRepo,
    user_id: &str,
    date: &str,
    focused_task_id: Option<&str>,
) -> Result<AgendaResponse, AgendaError> {
    let date = parse_date(date)?;
    let taxonomy = load_taxonomy_seeded(list_repo, category_repo, user_id).await?;

    // Living routines in standing order — the seed order.
    let routines = routine_repo.list_by_user_id(user_id).await?;
    for routine in &routines {
        // A stored rule that no longer parses (hand-edited row) degrades to
        // "no seed" — a read never 400s on stored-data decay.
        let covers = occurrence_dates(
            &routine.dtstart,
            &routine.rrule,
            &date,
            &date,
            &routine_exdates(routine),
        )
        .map(|dates| dates.contains(&date))
        .unwrap_or(false);
        if !covers {
            continue;
        }
        // Ensure the occurrence (idempotent: INSERT OR IGNORE + re-read).
        let occurrence = occurrence_repo
            .insert(NewRoutineOccurrence {
                routine_id: routine.id.clone(),
                user_id: user_id.to_string(),
                local_date: date.clone(),
            })
            .await?;
        // Append the membership row only when this date does not have one
        // yet — a list the user reordered keeps its ranks.
        if agenda_repo
            .get_by_key(user_id, &date, AGENDA_KIND_OCCURRENCE, &occurrence.id)
            .await?
            .is_none()
        {
            let rank = agenda_repo
                .max_sort_order(user_id, &date)
                .await?
                .map(|max| max + 1)
                .unwrap_or(0);
            agenda_repo
                .insert(NewAgendaItem {
                    user_id: user_id.to_string(),
                    local_date: date.clone(),
                    kind: AGENDA_KIND_OCCURRENCE.to_string(),
                    ref_id: occurrence.id.clone(),
                    sort_order: rank,
                })
                .await?;
        }
    }

    // The response: pile order, omitting orphaned embeds (missing/soft-deleted
    // task or routine) while their membership rows stay in D1.
    let items = agenda_repo.list_by_user_and_date(user_id, &date).await?;
    let mut views = Vec::with_capacity(items.len());
    for item in &items {
        if let Some(view) =
            embed_item(task_repo, occurrence_repo, routine_repo, item, &taxonomy, focused_task_id)
                .await?
        {
            views.push(view);
        }
    }
    Ok(AgendaResponse { items: views })
}

// ──────────────────────────────────────────
// POST /api/agenda/items — add a task (v1: tasks only)
// ──────────────────────────────────────────

/// `POST /api/agenda/items` → `{"item":…}`.
///
/// `kind` must be `task` in v1 (occurrences are auto-seeded). The task must
/// be a living `OPEN | PLANNED | IN_PROGRESS` row of this user. Already on
/// the date → **idempotent 200** with the existing item (its stored
/// `sort_order` wins). Otherwise inserts at `sort_order` when named, else
/// appends at `max+1` for the date (0 when empty). Never changes
/// `tasks.status` — Home is an overlay, not a sixth column.
pub async fn add_agenda_item(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    agenda_repo: &dyn AgendaItemRepo,
    task_repo: &dyn TaskRepo,
    user_id: &str,
    input: &NewAgendaItemInput,
    focused_task_id: Option<&str>,
) -> Result<AgendaItemResponse, AgendaError> {
    let date = parse_date(&input.date)?;
    if input.kind != AGENDA_KIND_TASK {
        return Err(AgendaError::Invalid("kind must be task".to_string()));
    }
    let Some(task) = task_repo.get_by_id(&input.ref_id).await? else {
        return Err(AgendaError::NotFound); // missing / soft-deleted
    };
    if task.user_id != user_id {
        return Err(AgendaError::NotFound); // other user — never leak existence
    }
    if !matches!(task.status.as_str(), "OPEN" | "PLANNED" | "IN_PROGRESS") {
        return Err(AgendaError::Invalid(
            "task cannot be added to the agenda".to_string(),
        ));
    }
    let taxonomy = load_taxonomy_seeded(list_repo, category_repo, user_id).await?;

    // Idempotent 200: already on that date → return the existing item as-is
    // (never touch its sort_order).
    if let Some(existing) = agenda_repo
        .get_by_key(user_id, &date, AGENDA_KIND_TASK, &task.id)
        .await?
    {
        return Ok(AgendaItemResponse {
            item: task_item_view(&existing, &task, &taxonomy, focused_task_id),
        });
    }

    let rank = match input.sort_order {
        Some(rank) if rank >= 0 => rank,
        Some(_) => {
            return Err(AgendaError::Invalid(
                "sort_order must be a non-negative integer".to_string(),
            ))
        }
        None => agenda_repo
            .max_sort_order(user_id, &date)
            .await?
            .map(|max| max + 1)
            .unwrap_or(0),
    };
    let item = agenda_repo
        .insert(NewAgendaItem {
            user_id: user_id.to_string(),
            local_date: date.clone(),
            kind: AGENDA_KIND_TASK.to_string(),
            ref_id: task.id.clone(),
            sort_order: rank,
        })
        .await?;
    Ok(AgendaItemResponse {
        item: task_item_view(&item, &task, &taxonomy, focused_task_id),
    })
}

// ──────────────────────────────────────────
// POST /api/agenda/items/:id/move — reorder within the date's pile
// ──────────────────────────────────────────

/// `POST /api/agenda/items/:id/move` → `{"item":…}`.
///
/// Reorders **that item's date pile only**: every peer ranked at or after the
/// target rank shifts up one (no `updated_at` bump, same spirit as the task
/// board), then the item lands on the target rank. Same-item same-rank is a
/// 200 no-op. Missing/other-user → 404.
///
/// `focused_task_id` only paints the embedded task's `focused` flag (same
/// contract as `get_agenda`).
pub async fn move_agenda_item(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    agenda_repo: &dyn AgendaItemRepo,
    task_repo: &dyn TaskRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    routine_repo: &dyn RoutineRepo,
    user_id: &str,
    id: &str,
    sort_order: i64,
    focused_task_id: Option<&str>,
) -> Result<AgendaItemResponse, AgendaError> {
    if sort_order < 0 {
        return Err(AgendaError::Invalid(
            "sort_order must be a non-negative integer".to_string(),
        ));
    }
    let Some(item) = agenda_repo.get_by_id(id).await? else {
        return Err(AgendaError::NotFound);
    };
    if item.user_id != user_id {
        return Err(AgendaError::NotFound);
    }
    if item.sort_order != sort_order {
        agenda_repo
            .shift_sort_order(user_id, &item.local_date, sort_order, 1)
            .await?;
        agenda_repo.set_sort_order(id, sort_order).await?;
    }
    let taxonomy = load_taxonomy_seeded(list_repo, category_repo, user_id).await?;
    let moved = agenda_repo.get_by_id(id).await?.ok_or(AgendaError::NotFound)?;
    let view = embed_item(
        task_repo,
        occurrence_repo,
        routine_repo,
        &moved,
        &taxonomy,
        focused_task_id,
    )
    .await?
    .ok_or(AgendaError::NotFound)?;
    Ok(AgendaItemResponse { item: view })
}

// ──────────────────────────────────────────
// POST /api/agenda/items/:id/reschedule — relocate a slot to another day
// ──────────────────────────────────────────

/// `POST /api/agenda/items/:id/reschedule { date }` → `{"item":…}` (the same
/// embed as add/move).
///
/// Relocates the item's slot to `date` (ADR 0004 amendment). Session-only —
/// no Google write of any kind, no RRULE anywhere.
///
/// **Occurrence** (kind `occurrence`):
/// 1. The **source date is appended to the routine's `exdates`** (no-op when
///    already present). Without this the next `GET /api/agenda?date=<source>`
///    would re-seed a **new** source-date occurrence — the RRULE still
///    matches and the moved-away day resurrects itself. Next week's same
///    weekday is a different `YYYY-MM-DD` and still seeds.
/// 2. `local_date` is rewritten; `pending | skipped` are the only
///    reschedulable statuses — `skipped` becomes `pending` on the new date
///    (deferred, not declined); `in_progress`/`done` → 400. A target date
///    that already holds an occurrence of this `routine_id` → 400. Title
///    override and any stored google ids travel with the row (never cleared).
///
/// **Task** (kind `task`): the **membership slot** moves, not the task —
/// `tasks.status` is unchanged and **no `task_logs` row is written** (the
/// agenda is an overlay, and `task_logs` has no civil-date column). A target
/// date that already has this task **unpins the source** (hard-delete this
/// item) and returns the **existing** target item — never a duplicate.
///
/// Same date → 200 no-op returning the item unchanged. Missing item /
/// other-user / missing or soft-deleted task / soft-deleted routine → 404.
/// Missing/invalid `date` → 400.
pub async fn reschedule_agenda_item(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    agenda_repo: &dyn AgendaItemRepo,
    task_repo: &dyn TaskRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    routine_repo: &dyn RoutineRepo,
    user_id: &str,
    id: &str,
    date: &str,
    focused_task_id: Option<&str>,
) -> Result<AgendaItemResponse, AgendaError> {
    let date = parse_date(date)?;
    let Some(item) = agenda_repo.get_by_id(id).await? else {
        return Err(AgendaError::NotFound);
    };
    if item.user_id != user_id {
        return Err(AgendaError::NotFound);
    }
    // Same date → 200 no-op: nothing moves, nothing is exdated.
    if item.local_date == date {
        let taxonomy = load_taxonomy_seeded(list_repo, category_repo, user_id).await?;
        let view = embed_item(
            task_repo,
            occurrence_repo,
            routine_repo,
            &item,
            &taxonomy,
            focused_task_id,
        )
        .await?
        .ok_or(AgendaError::NotFound)?;
        return Ok(AgendaItemResponse { item: view });
    }

    match item.kind.as_str() {
        AGENDA_KIND_OCCURRENCE => {
            reschedule_occurrence(
                agenda_repo,
                occurrence_repo,
                routine_repo,
                user_id,
                &item,
                &date,
            )
            .await?;
        }
        AGENDA_KIND_TASK => {
            // `Some(existing)` when the source was unpinned and the existing
            // target item is the answer; `None` when the slot moved.
            if let Some(existing) =
                reschedule_task_slot(agenda_repo, task_repo, user_id, &item, &date).await?
            {
                let taxonomy = load_taxonomy_seeded(list_repo, category_repo, user_id).await?;
                let view = embed_item(
                    task_repo,
                    occurrence_repo,
                    routine_repo,
                    &existing,
                    &taxonomy,
                    focused_task_id,
                )
                .await?
                .ok_or(AgendaError::NotFound)?;
                return Ok(AgendaItemResponse { item: view });
            }
        }
        _ => return Err(AgendaError::NotFound),
    }

    let taxonomy = load_taxonomy_seeded(list_repo, category_repo, user_id).await?;
    let moved = agenda_repo
        .get_by_id(&item.id)
        .await?
        .ok_or(AgendaError::NotFound)?;
    let view = embed_item(
        task_repo,
        occurrence_repo,
        routine_repo,
        &moved,
        &taxonomy,
        focused_task_id,
    )
    .await?
    .ok_or(AgendaError::NotFound)?;
    Ok(AgendaItemResponse { item: view })
}

/// The occurrence half of a reschedule:
/// 1. verify the occurrence + its routine are alive and owned (404 otherwise),
/// 2. gate the status (`pending | skipped` only; `skipped` → `pending`),
/// 3. refuse a target date already holding this routine (400),
/// 4. append the **source** date to the routine's `exdates` (no-op when
///    already present) — the anti-re-seed lock,
/// 5. rewrite `local_date` and move the agenda row to the target pile
///    appended (`max+1`).
async fn reschedule_occurrence(
    agenda_repo: &dyn AgendaItemRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    routine_repo: &dyn RoutineRepo,
    user_id: &str,
    item: &AgendaItem,
    date: &str,
) -> Result<(), AgendaError> {
    let Some(occurrence) = occurrence_repo.get_by_id(&item.ref_id).await? else {
        return Err(AgendaError::NotFound);
    };
    if occurrence.user_id != user_id {
        return Err(AgendaError::NotFound);
    }
    // A soft-deleted routine makes its materialized occurrences 404 (same
    // gate as every occurrence verb).
    let Some(routine) = routine_repo.get_by_id(&occurrence.routine_id).await? else {
        return Err(AgendaError::NotFound);
    };

    match occurrence.status.as_str() {
        OCCURRENCE_STATUS_IN_PROGRESS => {
            return Err(AgendaError::Invalid(
                "cannot reschedule an in_progress occurrence".to_string(),
            ))
        }
        OCCURRENCE_STATUS_DONE => {
            return Err(AgendaError::Invalid(
                "cannot reschedule a done occurrence".to_string(),
            ))
        }
        // `pending` stays pending; `skipped` is deferred, not declined —
        // it becomes `pending` on the new date.
        OCCURRENCE_STATUS_PENDING | OCCURRENCE_STATUS_SKIPPED => {}
        other => {
            return Err(AgendaError::Invalid(format!(
                "cannot reschedule a {other} occurrence"
            )))
        }
    }
    // The `UNIQUE (routine_id, local_date)` slot on the target date is
    // already taken by this routine → refuse (never collide).
    if occurrence_repo
        .get_by_routine_and_date(&occurrence.routine_id, date)
        .await?
        .is_some()
    {
        return Err(AgendaError::Invalid(
            "routine already has an occurrence on that date".to_string(),
        ));
    }

    // The anti-re-seed lock (ADR 0004 amendment): the source date leaves the
    // rule's expansion, or the next GET on it would seed a fresh occurrence.
    let mut exdates = routine_exdates(&routine);
    if !exdates.iter().any(|excluded| excluded == &occurrence.local_date) {
        exdates.push(occurrence.local_date.clone());
        routine_repo
            .update(
                &routine.id,
                &UpdateRoutine {
                    exdates: Some(exdates),
                    ..Default::default()
                },
            )
            .await?;
    }

    occurrence_repo.set_local_date(&occurrence.id, date).await?;
    if occurrence.status == OCCURRENCE_STATUS_SKIPPED {
        occurrence_repo
            .set_status(&occurrence.id, OCCURRENCE_STATUS_PENDING)
            .await?;
    }
    // The agenda row lands appended on the target pile (the source pile keeps
    // its ranks — same gap semantics as unpin).
    let rank = agenda_repo
        .max_sort_order(user_id, date)
        .await?
        .map(|max| max + 1)
        .unwrap_or(0);
    agenda_repo.set_local_date(&item.id, date, rank).await?;
    Ok(())
}

/// The task half of a reschedule: the **membership slot** moves, never the
/// task — no `tasks.status` change, no `task_logs` row (the agenda is an
/// overlay, and `task_logs` has no civil-date column). Returns:
/// - `Ok(None)` — the slot moved; the caller re-reads `item.id`.
/// - `Ok(Some(existing))` — the target date already had this task; the source
///   was unpinned (hard-delete) and the **existing** target item is the
///   answer. Never a duplicate: the same task MAY appear on several days at
///   once (`UNIQUE (user, date, kind, ref_id)` is per date); reschedule
///   relocates a slot, Add-task clones onto another day.
async fn reschedule_task_slot(
    agenda_repo: &dyn AgendaItemRepo,
    task_repo: &dyn TaskRepo,
    user_id: &str,
    item: &AgendaItem,
    date: &str,
) -> Result<Option<AgendaItem>, AgendaError> {
    let Some(task) = task_repo.get_by_id(&item.ref_id).await? else {
        return Err(AgendaError::NotFound); // missing / soft-deleted
    };
    if task.user_id != user_id {
        return Err(AgendaError::NotFound);
    }
    if let Some(existing) = agenda_repo
        .get_by_key(user_id, date, AGENDA_KIND_TASK, &task.id)
        .await?
    {
        agenda_repo.hard_delete(&item.id).await?;
        return Ok(Some(existing));
    }
    let rank = agenda_repo
        .max_sort_order(user_id, date)
        .await?
        .map(|max| max + 1)
        .unwrap_or(0);
    agenda_repo.set_local_date(&item.id, date, rank).await?;
    Ok(None)
}

// ──────────────────────────────────────────
// DELETE /api/agenda/items/:id — unpin (task-kind only)
// ──────────────────────────────────────────

/// `DELETE /api/agenda/items/:id` → `{"success":true}`.
///
/// Hard-deletes the membership row (unpin). The task stays on the Board.
/// Occurrence-kind items are refused — **skip is the decline**. Missing /
/// other-user → 404.
pub async fn delete_agenda_item(
    agenda_repo: &dyn AgendaItemRepo,
    user_id: &str,
    id: &str,
) -> Result<DeleteAgendaItemResponse, AgendaError> {
    let Some(item) = agenda_repo.get_by_id(id).await? else {
        return Err(AgendaError::NotFound);
    };
    if item.user_id != user_id {
        return Err(AgendaError::NotFound);
    }
    if item.kind == AGENDA_KIND_OCCURRENCE {
        return Err(AgendaError::Invalid("skip is the decline".to_string()));
    }
    agenda_repo.hard_delete(id).await?;
    Ok(DeleteAgendaItemResponse { success: true })
}

// ──────────────────────────────────────────
// PATCH /api/occurrences/:id — title override
// ──────────────────────────────────────────

/// `PATCH /api/occurrences/:id { title }` → `{"occurrence":…}`.
///
/// A present `title` writes the override; `""` or whitespace-only **clears**
/// it back to inheritance (stores NULL, resolved title becomes the routine
/// title again). Empty body → 400 "nothing to update". Missing / other-user /
/// soft-deleted-routine → 404.
///
/// **Google write (slice 6):** when the occurrence already has a chip
/// (`google_event_id` set) AND the **resolved** title actually changed
/// (`override ?? routine.title`, before vs after — clearing back to
/// inheritance counts), the chip's `summary` is PATCHed to the new resolved
/// title — the one difference from tasks, locked in the ADR. A Google 404
/// proceeds (the chip is gone; the override still writes). The worker gates
/// the write on a refreshable token; here `http`/`calendars`/`events`/
/// `access` are `Option` so session-only patches (no chip yet) keep working.
pub async fn patch_occurrence(
    http: Option<&dyn HttpClient>,
    calendars: Option<&dyn CalendarRepo>,
    events: Option<&dyn CalendarEventRepo>,
    access: Option<&GoogleAccess>,
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    user_id: &str,
    id: &str,
    updates: &UpdateOccurrence,
    now_unix: i64,
) -> Result<OccurrenceResponse, AgendaError> {
    if updates.title.is_none() {
        return Err(AgendaError::Invalid("nothing to update".to_string()));
    }
    let occurrence = load_occurrence_for_user(occurrence_repo, routine_repo, user_id, id).await?;
    let routine = routine_repo
        .get_by_id(&occurrence.routine_id)
        .await?
        .ok_or(AgendaError::NotFound)?;
    let resolved_before = occurrence
        .title
        .as_deref()
        .unwrap_or(&routine.title)
        .to_string();
    let trimmed = updates.title.as_deref().map(str::trim);
    let stored = match trimmed {
        // `""` / whitespace-only → clear the override (inherit again).
        Some("") => None,
        Some(title) => Some(title.to_string()),
        None => None, // guarded above; keeps the match exhaustive
    };
    occurrence_repo.update_title(id, stored.as_deref()).await?;
    let updated = occurrence_repo.get_by_id(id).await?.ok_or(AgendaError::NotFound)?;
    let routine = routine_repo
        .get_by_id(&updated.routine_id)
        .await?
        .ok_or(AgendaError::NotFound)?;
    let resolved_after = updated
        .title
        .as_deref()
        .unwrap_or(&routine.title)
        .to_string();

    // The chip's summary follows the resolved title (ADR 0004: occurrence
    // title PATCH updates `summary` when a chip exists). A 404 means the
    // event is gone on Google's side — proceed, never fail the override on a
    // ghost chip.
    if resolved_before != resolved_after
        && updated.google_event_id.is_some()
        && updated.calendar_id.is_some()
    {
        if let (Some(http), Some(calendars), Some(events), Some(access)) =
            (http, calendars, events, access)
        {
            let output = patch_event_summary(
                http,
                calendars,
                events,
                access,
                updated.calendar_id.as_deref().expect("checked above"),
                updated.google_event_id.as_deref().expect("checked above"),
                &resolved_after,
                now_unix,
            )
            .await;
            match output {
                Ok(_) => {}
                Err(CalendarError::GoogleApi(message)) if message.contains("404") => {}
                Err(err) => return Err(AgendaError::from(err)),
            }
        }
    }

    let taxonomy = load_taxonomy_seeded(list_repo, category_repo, user_id).await?;
    Ok(OccurrenceResponse {
        occurrence: occurrence_view(&updated, &routine, &taxonomy),
    })
}

// ──────────────────────────────────────────
// POST /api/occurrences/:id/start, /complete and /skip — verb matrix + Google
// ──────────────────────────────────────────

/// `POST /api/occurrences/:id/start` → 200 `{"occurrence":…,"event":…}`.
///
/// Starts a **pending, today-only** occurrence: creates the one-shot Google
/// log (summary = the **resolved** title, carriers
/// `sanctuary_routine_id`/`sanctuary_occurrence_id`, never a task_id, never
/// an RRULE, `T … T + START_EVENT_MINUTES` on the minute grid), stores
/// `calendar_id` + `google_event_id` on the occurrence, and flips it to
/// `in_progress`. One Google event per occurrence, ever.
///
/// Verb matrix (locked): `pending` → start; `in_progress` → **200 no-op**
/// (no second event); `done`/`skipped` → 400. Start is valid only when
/// `local_date` is today — the civil date of `now_unix` in `Asia/Kolkata`
/// (production TZ, already in the locked offset table); otherwise 400.
/// Missing / other-user / soft-deleted-routine → 404. No writable calendar →
/// 400. Focus stays task-only: `sanctuary_focus` is never set here.
pub async fn start_occurrence(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    access: &GoogleAccess,
    user_id: &str,
    id: &str,
    now_unix: i64,
) -> Result<OccurrenceActionResponse, AgendaError> {
    let occurrence = load_occurrence_for_user(occurrence_repo, routine_repo, user_id, id).await?;
    let routine = routine_repo
        .get_by_id(&occurrence.routine_id)
        .await?
        .ok_or(AgendaError::NotFound)?;

    // Terminal states are closed for start (the ADR's locked matrix).
    if occurrence.status != OCCURRENCE_STATUS_PENDING {
        if occurrence.status == OCCURRENCE_STATUS_IN_PROGRESS {
            // Idempotent 200 no-op: the chip exists, never open a second one.
            let taxonomy = load_taxonomy_seeded(list_repo, category_repo, user_id).await?;
            return Ok(OccurrenceActionResponse {
                occurrence: occurrence_view(&occurrence, &routine, &taxonomy),
                event: None,
            });
        }
        return Err(AgendaError::Invalid(format!(
            "cannot start a {status} occurrence",
            status = occurrence.status
        )));
    }
    // Today-only (ADR 0004 § Start): the production offset table decides the
    // civil date — a planned occurrence is started the day it is planned.
    if occurrence.local_date != civil_date_in_offset(now_unix, "Asia/Kolkata") {
        return Err(AgendaError::Invalid(
            "occurrence can only be started on its local date".to_string(),
        ));
    }

    let resolved_title = occurrence
        .title
        .as_deref()
        .unwrap_or(&routine.title)
        .to_string();
    let taxonomy = load_taxonomy_seeded(list_repo, category_repo, user_id).await?;
    let target = resolve_occurrence_calendar(calendars, &taxonomy, &resolved_title, user_id).await?;

    // The same minute-grid window as task start — `estimated_minutes` is only
    // a planned estimate on the card and never sizes the chip.
    let t_unix = nearest_minute_unix(now_unix);
    let start_rfc3339 = unix_secs_to_rfc3339(t_unix);
    let end_rfc3339 = unix_secs_to_rfc3339(t_unix + START_EVENT_MINUTES * 60);
    let output: CreateEventOutput = create_event(
        http,
        calendars,
        events,
        access,
        &NewEventInput {
            calendar_id: target.calendar_id.clone(),
            summary: resolved_title,
            description: None,
            start: start_rfc3339,
            end: end_rfc3339,
            task_id: None,
            // The occurrence carriers — the sync path never maps them onto
            // `calendar_events.task_id`, keeping the two worlds apart.
            routine_id: Some(occurrence.routine_id.clone()),
            occurrence_id: Some(occurrence.id.clone()),
            color_id: target.google_color_id,
            // Focus stays task-only (ADR 0004): a running routine never takes
            // `users.focused_task_id`, so the chip is never focused.
            sanctuary_focus: false,
            priority: None,
            difficulty: None,
        },
        now_unix,
    )
    .await?;
    let event = output.event;

    occurrence_repo
        .set_event_ids(&occurrence.id, &target.calendar_id, &event.google_event_id)
        .await?;
    occurrence_repo
        .set_status(&occurrence.id, OCCURRENCE_STATUS_IN_PROGRESS)
        .await?;

    let updated = occurrence_repo.get_by_id(id).await?.ok_or(AgendaError::NotFound)?;
    let routine = routine_repo
        .get_by_id(&updated.routine_id)
        .await?
        .ok_or(AgendaError::NotFound)?;
    Ok(OccurrenceActionResponse {
        occurrence: occurrence_view(&updated, &routine, &taxonomy),
        event: Some(event),
    })
}

/// `POST /api/occurrences/:id/complete` → 200 `{"occurrence":…}`.
///
/// Verb matrix (locked): `pending` → `done`, `in_progress` → `done` (the open
/// chip's end is PATCHed closed first when ids are stored and Google is
/// available), `done` → 200 no-op, `skipped` → `done`. Missing / other-user /
/// soft-deleted-routine → 404.
pub async fn complete_occurrence(
    http: Option<&dyn HttpClient>,
    calendars: Option<&dyn CalendarRepo>,
    events: Option<&dyn CalendarEventRepo>,
    access: Option<&GoogleAccess>,
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    user_id: &str,
    id: &str,
    now_unix: i64,
) -> Result<OccurrenceResponse, AgendaError> {
    exit_occurrence(
        http, calendars, events, access, list_repo, category_repo, routine_repo,
        occurrence_repo, user_id, id, now_unix, OCCURRENCE_STATUS_DONE,
    )
    .await
}

/// `POST /api/occurrences/:id/skip` → 200 `{"occurrence":…}`.
///
/// Verb matrix (locked): `pending` → `skipped`, `in_progress` → `skipped`
/// (the open chip's end is PATCHed closed first when ids are stored and
/// Google is available), `done` → `skipped`, `skipped` → 200 no-op. Missing /
/// other-user / soft-deleted-routine → 404.
pub async fn skip_occurrence(
    http: Option<&dyn HttpClient>,
    calendars: Option<&dyn CalendarRepo>,
    events: Option<&dyn CalendarEventRepo>,
    access: Option<&GoogleAccess>,
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    user_id: &str,
    id: &str,
    now_unix: i64,
) -> Result<OccurrenceResponse, AgendaError> {
    exit_occurrence(
        http, calendars, events, access, list_repo, category_repo, routine_repo,
        occurrence_repo, user_id, id, now_unix, OCCURRENCE_STATUS_SKIPPED,
    )
    .await
}

/// The shared complete/skip machinery: close the running chip when leaving
/// `in_progress` with stored ids (Google optional — `http`/`access` `None`
/// means session-only, the flip still happens), then flip the status.
/// Already in the target status → 200 no-op (no Google write).
async fn exit_occurrence(
    http: Option<&dyn HttpClient>,
    calendars: Option<&dyn CalendarRepo>,
    events: Option<&dyn CalendarEventRepo>,
    access: Option<&GoogleAccess>,
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    user_id: &str,
    id: &str,
    now_unix: i64,
    target_status: &str,
) -> Result<OccurrenceResponse, AgendaError> {
    let occurrence = load_occurrence_for_user(occurrence_repo, routine_repo, user_id, id).await?;
    if occurrence.status == target_status {
        // Idempotent 200 no-op: nothing to flip, no Google write.
        return occurrence_response(list_repo, category_repo, routine_repo, occurrence_repo, user_id, id).await;
    }

    // Status is the lock: only an `in_progress` occurrence has a living chip.
    // When ids are stored AND Google is available, snap the end closed first —
    // the same exit semantics as task stop/pause/complete (never fail the
    // flip on a ghost event: Google 404 proceeds).
    if occurrence.status == OCCURRENCE_STATUS_IN_PROGRESS
        && occurrence.calendar_id.is_some()
        && occurrence.google_event_id.is_some()
    {
        if let (Some(http), Some(calendars), Some(events), Some(access)) =
            (http, calendars, events, access)
        {
            close_occurrence_event(
                http,
                calendars,
                events,
                access,
                occurrence.calendar_id.as_deref().expect("checked above"),
                occurrence.google_event_id.as_deref().expect("checked above"),
                now_unix,
            )
            .await?;
        }
    }

    occurrence_repo.set_status(id, target_status).await?;
    occurrence_response(list_repo, category_repo, routine_repo, occurrence_repo, user_id, id).await
}

/// PATCHes an occurrence chip's `end` to snapped now (`start + 60s` when
/// `now <= start` — the invert guard, on the minute grid, same as task
/// exits). Resolved through the stored ids on the occurrence row — never
/// through the event window — so a run whose window already lapsed still
/// closes. A Google 404 is treated like task exits: the event is gone, the
/// caller proceeds with the status flip.
async fn close_occurrence_event(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    access: &GoogleAccess,
    calendar_id: &str,
    google_event_id: &str,
    now_unix: i64,
) -> Result<(), AgendaError> {
    // The cached row supplies the `start` for the invert guard; a missing
    // cache row still snaps (the guard falls back to `now`, so the PATCH is
    // `start + 60s` from the cache's perspective — skipped, see below).
    let found = events
        .get_by_calendar_and_google_id(calendar_id, google_event_id)
        .await?;
    let start_unix = found
        .as_ref()
        .and_then(|event| rfc3339_to_unix_secs(&event.start_time))
        .unwrap_or(now_unix);
    let snapped = nearest_minute_unix(now_unix);
    let end_unix = if snapped <= start_unix {
        start_unix + 60
    } else {
        snapped
    };
    match patch_event(
        http,
        calendars,
        events,
        access,
        calendar_id,
        google_event_id,
        &unix_secs_to_rfc3339(end_unix),
        now_unix,
    )
    .await
    {
        Ok(_) => Ok(()),
        // The event is gone on Google's side (404): fall through — the
        // status flip still happens, exactly like task exits.
        Err(CalendarError::GoogleApi(message)) if message.contains("404") => Ok(()),
        Err(err) => Err(AgendaError::from(err)),
    }
}

// ──────────────────────────────────────────
// View building + classification plumbing
// ──────────────────────────────────────────

/// Loads the occurrence and verifies ownership AND the routine's living state
/// — the 404 gate shared by every occurrence verb (a soft-deleted routine
/// makes its materialized occurrences 404; missing/other-user → 404, never a
/// leak).
async fn load_occurrence_for_user(
    occurrence_repo: &dyn OccurrenceRepo,
    routine_repo: &dyn RoutineRepo,
    user_id: &str,
    id: &str,
) -> Result<RoutineOccurrence, AgendaError> {
    let Some(occurrence) = occurrence_repo.get_by_id(id).await? else {
        return Err(AgendaError::NotFound);
    };
    if occurrence.user_id != user_id {
        return Err(AgendaError::NotFound);
    }
    if routine_repo.get_by_id(&occurrence.routine_id).await?.is_none() {
        return Err(AgendaError::NotFound);
    }
    Ok(occurrence)
}

/// Reloads the occurrence after a verb and builds its response.
async fn occurrence_response(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    user_id: &str,
    id: &str,
) -> Result<OccurrenceResponse, AgendaError> {
    let taxonomy = load_taxonomy_seeded(list_repo, category_repo, user_id).await?;
    let occurrence = occurrence_repo.get_by_id(id).await?.ok_or(AgendaError::NotFound)?;
    let routine = routine_repo
        .get_by_id(&occurrence.routine_id)
        .await?
        .ok_or(AgendaError::NotFound)?;
    Ok(OccurrenceResponse {
        occurrence: occurrence_view(&occurrence, &routine, &taxonomy),
    })
}

/// Embeds the item's referenced task or occurrence into its view, returning
/// `None` when the embed is missing or its backing row is not living — the
/// agenda read then omits the orphan (membership row stays in D1). For
/// single-item endpoints a `None` becomes the caller's 404.
async fn embed_item(
    task_repo: &dyn TaskRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    routine_repo: &dyn RoutineRepo,
    item: &AgendaItem,
    taxonomy: &Taxonomy,
    focused_task_id: Option<&str>,
) -> Result<Option<AgendaItemView>, RepoError> {
    let (task, occurrence) = match item.kind.as_str() {
        AGENDA_KIND_TASK => {
            // `get_by_id` filters soft-deleted tasks — a deleted/missing task
            // omits the whole item (orphan membership rows stay in D1).
            let Some(task) = task_repo.get_by_id(&item.ref_id).await? else {
                return Ok(None);
            };
            (Some(task_view(&task, taxonomy, focused_task_id)), None)
        }
        AGENDA_KIND_OCCURRENCE => {
            let Some(occurrence) = occurrence_repo.get_by_id(&item.ref_id).await? else {
                return Ok(None);
            };
            // Never leak another user's occurrence through a hand-edited row.
            if occurrence.user_id != item.user_id {
                return Ok(None);
            }
            // `get_by_id` filters soft-deleted routines — a soft-deleted
            // routine omits its leftover agenda item.
            let Some(routine) = routine_repo.get_by_id(&occurrence.routine_id).await? else {
                return Ok(None);
            };
            (None, Some(occurrence_view(&occurrence, &routine, taxonomy)))
        }
        _ => return Ok(None),
    };
    Ok(Some(AgendaItemView {
        id: item.id.clone(),
        user_id: item.user_id.clone(),
        local_date: item.local_date.clone(),
        kind: item.kind.clone(),
        ref_id: item.ref_id.clone(),
        sort_order: item.sort_order,
        task,
        occurrence,
    }))
}

/// A resolved target calendar for a started occurrence (local
/// `google_calendars.id` — the Google id itself lives on the row the event
/// insert re-reads).
#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetCalendar {
    calendar_id: String,
    /// Stored `google_color_id` of the matched category; `None` when the
    /// resolved title is untracked or the category has no stored color.
    google_color_id: Option<String>,
}

/// Resolves the Google calendar a started occurrence's one-shot log lands on
/// — a deliberate copy of `tasks.rs::resolve_target_calendar` (the slice
/// brief: duplicate rather than refactor tasks.rs). `wanted` is the first
/// non-empty of the matched category's first regex-matching pattern's
/// `google_calendar_id` (patterns walked in stored `sort_order`), the
/// matched category's `google_calendar_id`, and the parent root's
/// `google_calendar_id`. The event lands on `wanted` when that calendar
/// exists for the user and is writable (`access_role` owner/writer); a
/// missing or read-only named calendar falls back to the user's **primary**
/// calendar, never to the next inheritance slot. No writable calendar → 400.
async fn resolve_occurrence_calendar(
    calendars: &dyn CalendarRepo,
    taxonomy: &Taxonomy,
    resolved_title: &str,
    user_id: &str,
) -> Result<TargetCalendar, AgendaError> {
    let user_cals = calendars.list_by_user_id(user_id).await?;
    let category = match classify(resolved_title, CalendarScope::Ignore, &taxonomy.matchers) {
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
        .and_then(|matcher| first_matching_pattern(resolved_title, CalendarScope::Ignore, matcher))
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
        .ok_or_else(|| AgendaError::Invalid("no writable calendar".to_string()))?;
    Ok(TargetCalendar {
        calendar_id: target.id.clone(),
        // The matched category's STORED color, or `None` for untracked /
        // categories without one — the event insert omits `colorId` then.
        google_color_id: category.and_then(|category| category.google_color_id.clone()),
    })
}

/// Whether a calendar looks writable: Google `access_role` is `owner` or
/// `writer` (a copy of `tasks.rs::is_writable`).
fn is_writable(calendar: &GoogleCalendar) -> bool {
    calendar.access_role == "owner" || calendar.access_role == "writer"
}

// ──────────────────────────────────────────
// Elongate cron (slice 6): grow in_progress occurrence events
// ──────────────────────────────────────────

/// The occurrence half of the elongate cron (called from the same worker
/// tick as [`crate::tasks::run_elongate_cron`]): while an occurrence stays
/// `in_progress`, grow its one-shot Google log so the live calendar block
/// does not look finished — the exact same rule as tasks.
///
/// Target: `end = max(current_end, ceil_5min(now + 5min)` in the event
/// calendar's IANA time zone), persisted as a UTC `…Z` string.
///
/// Per occurrence, in order:
/// 1. `refresh_if_needed` for the occurrence's owner. On failure the
///    occurrence is skipped entirely (one error) — other users still grow.
/// 2. Resolve the cached event through the occurrence's stored ids
///    (`calendar_id` + `google_event_id` — the work list only returns rows
///    that have both). A missing cache row → skipped: the event is gone,
///    never recreate it, never flip status.
/// 3. Parse `event.end_time`. Unparseable → skipped.
/// 4. Load the event's calendar for `time_zone`; a missing calendar or an
///    empty/unknown zone falls back to UTC.
/// 5. PATCH only when `target > current_end` (never shrink). A Google 404 →
///    skipped (the event vanished; never recreate). Other errors are
///    collected and the loop continues.
///
/// Status is deliberately never touched here, exactly like tasks.
pub async fn run_elongate_occurrences(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    occurrences: &dyn OccurrenceRepo,
    tokens: &dyn TokenRepo,
    oauth: &OAuthConfig,
    now_unix: i64,
) -> ElongateReport {
    let mut report = ElongateReport::default();
    let running = match occurrences.list_in_progress().await {
        Ok(running) => running,
        Err(err) => {
            report
                .errors
                .push(format!("occurrence list_in_progress failed: {err}"));
            return report;
        }
    };

    for occurrence in &running {
        let access = match refresh_if_needed(http, tokens, oauth, &occurrence.user_id, now_unix).await
        {
            Ok(access) => access,
            Err(err) => {
                report.errors.push(format!(
                    "token refresh failed for user {} (occurrence {}): {err}",
                    occurrence.user_id, occurrence.id
                ));
                continue;
            }
        };
        // The ids the work list guaranteed. A missing cache row means there
        // is nothing to grow: skip (do not recreate, do not flip status).
        let Some(calendar_id) = occurrence.calendar_id.as_deref() else {
            report.skipped += 1;
            continue;
        };
        let Some(google_event_id) = occurrence.google_event_id.as_deref() else {
            report.skipped += 1;
            continue;
        };
        let event = match events
            .get_by_calendar_and_google_id(calendar_id, google_event_id)
            .await
        {
            Ok(Some(event)) => event,
            Ok(None) => {
                report.skipped += 1;
                continue;
            }
            Err(err) => {
                report.errors.push(format!(
                    "event lookup failed for occurrence {} (event {}): {err}",
                    occurrence.id, google_event_id
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
                    "calendar lookup failed for occurrence {} (calendar {}): {err}",
                    occurrence.id, event.calendar_id
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
            Ok(_) => report.occurrences_elongated += 1,
            // The event is gone on Google's side (404): skip — never
            // recreate it, never touch the status.
            Err(CalendarError::GoogleApi(message)) if message.contains("404") => {
                report.skipped += 1;
            }
            Err(err) => report.errors.push(format!(
                "elongate failed for occurrence {} (event {}): {err}",
                occurrence.id, google_event_id
            )),
        }
    }
    report
}

/// The task-only embed (add path — the task row is already loaded).
fn task_item_view(
    item: &AgendaItem,
    task: &Task,
    taxonomy: &Taxonomy,
    focused_task_id: Option<&str>,
) -> AgendaItemView {
    AgendaItemView {
        id: item.id.clone(),
        user_id: item.user_id.clone(),
        local_date: item.local_date.clone(),
        kind: item.kind.clone(),
        ref_id: item.ref_id.clone(),
        sort_order: item.sort_order,
        task: Some(task_view(task, taxonomy, focused_task_id)),
        occurrence: None,
    }
}

/// Wraps a stored occurrence into its HTTP view. Classification is a read
/// over the **resolved** title: a title that no longer uniquely matches keeps
/// the `untracked` summary — listing never 400s on classification.
fn occurrence_view(occurrence: &RoutineOccurrence, routine: &Routine, taxonomy: &Taxonomy) -> OccurrenceView {
    let resolved_title = occurrence
        .title
        .as_deref()
        .unwrap_or(&routine.title)
        .to_string();
    let outcome = classify(&resolved_title, CalendarScope::Ignore, &taxonomy.matchers);
    let category = match &outcome {
        ClassifyOutcome::Matched { category_id } => taxonomy
            .categories
            .iter()
            .find(|category| category.id == *category_id)
            // Matched ids always come from the loaded taxonomy; fall back to
            // the sink rather than panic if that invariant ever breaks.
            .map(|category| summary_for(category, &taxonomy.categories))
            .or_else(|| untracked_summary(&taxonomy.categories)),
        ClassifyOutcome::Untracked { .. } => untracked_summary(&taxonomy.categories),
    }
    .expect("ensure_taxonomy guarantees the untracked sink exists");
    OccurrenceView {
        id: occurrence.id.clone(),
        routine_id: occurrence.routine_id.clone(),
        user_id: occurrence.user_id.clone(),
        local_date: occurrence.local_date.clone(),
        title: occurrence.title.clone(),
        resolved_title,
        status: occurrence.status.clone(),
        estimated_minutes: routine.estimated_minutes,
        dtstart: routine.dtstart.clone(),
        rrule: routine.rrule.clone(),
        exdates: routine_exdates(routine),
        calendar_id: occurrence.calendar_id.clone(),
        google_event_id: occurrence.google_event_id.clone(),
        created_at: occurrence.created_at.clone(),
        updated_at: occurrence.updated_at.clone(),
        category,
    }
}

/// Parses the routine's stored `exdates` JSON text; hand-edited rows degrade
/// to an empty array (same as `routines::to_view`).
fn routine_exdates(routine: &Routine) -> Vec<String> {
    serde_json::from_str(&routine.exdates).unwrap_or_default()
}

/// Validates a local civil date string (`YYYY-MM-DD`).
fn parse_date(date: &str) -> Result<String, AgendaError> {
    let date = date.trim();
    if date.is_empty() {
        return Err(AgendaError::Invalid("date must be YYYY-MM-DD".to_string()));
    }
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map(|_| date.to_string())
        .map_err(|_| AgendaError::Invalid("date must be YYYY-MM-DD".to_string()))
}

// ──────────────────────────────────────────
// Classification plumbing
// ──────────────────────────────────────────
//
// Deliberate duplication of `tasks.rs`'s private taxonomy helpers (slice
// brief: duplicate the classify→TaskView mapping in agenda.rs the way
// routines.rs duplicated taxonomy — do not rewrite tasks.rs). If the task
// rules change, these copies change with them.

/// All living categories plus the matcher set built from their patterns, in
/// one round-trip pair — the unit of work for every classify here.
struct Taxonomy {
    categories: Vec<TaskCategory>,
    matchers: Vec<CategoryWithPatterns>,
}

/// Loads living categories and their patterns in two queries, matching
/// `tasks.rs::load_taxonomy`.
async fn load_taxonomy(
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
) -> Result<Taxonomy, RepoError> {
    let categories = category_repo.list_by_user_id(user_id).await?;
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

/// Seeds the taxonomy (count-gated, like `list_tasks`) then loads it — the
/// unit of work for every view-building call here. The untracked sink is
/// guaranteed to exist afterwards, so `untracked_summary` can never be `None`
/// at classify time.
async fn load_taxonomy_seeded(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
) -> Result<Taxonomy, AgendaError> {
    let lists = list_repo.list_by_user_id(user_id).await?;
    let categories = category_repo.list_by_user_id(user_id).await?;
    ensure_taxonomy(list_repo, category_repo, &lists, &categories, user_id).await?;
    load_taxonomy(category_repo, user_id).await.map_err(AgendaError::from)
}

/// The `untracked` sink summary; `None` only before the first seed (callers
/// run `ensure_taxonomy` first, and the sink is undeletable).
fn untracked_summary(categories: &[TaskCategory]) -> Option<TaskCategorySummary> {
    categories
        .iter()
        .find(|category| category.is_untracked)
        .map(|category| summary_for(category, categories))
}

/// Builds the slim summary attached to views — identical field derivation to
/// `tasks.rs::summary_for` (children inherit the parent root's list).
fn summary_for(category: &TaskCategory, categories: &[TaskCategory]) -> TaskCategorySummary {
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

/// The full `TaskView` mapping — a copy of `tasks.rs::to_view` (display title
/// split off the first matching pattern's hole, `focused` painted from the
/// caller's pointer). Reads never write the users row.
fn task_view(task: &Task, taxonomy: &Taxonomy, focused_task_id: Option<&str>) -> TaskView {
    let outcome = classify(&task.title, CalendarScope::Ignore, &taxonomy.matchers);
    let (category, display_title) = match &outcome {
        ClassifyOutcome::Matched { category_id } => {
            let category = taxonomy
                .categories
                .iter()
                .find(|category| category.id == *category_id)
                .map(|category| summary_for(category, &taxonomy.categories))
                .or_else(|| untracked_summary(&taxonomy.categories));
            match category {
                Some(category) if !category.is_untracked => {
                    let display_title = taxonomy
                        .matchers
                        .iter()
                        .find(|matcher| matcher.category_id == category.id)
                        .map(|matcher| split_affixes(&task.title, matcher).2)
                        .unwrap_or_else(|| task.title.clone());
                    (Some(category), display_title)
                }
                _ => (category, task.title.clone()),
            }
        }
        ClassifyOutcome::Untracked { .. } => (
            untracked_summary(&taxonomy.categories),
            task.title.clone(),
        ),
    };
    let category = category.expect("ensure_taxonomy guarantees the untracked sink exists");
    TaskView {
        id: task.id.clone(),
        user_id: task.user_id.clone(),
        title: task.title.clone(),
        display_title,
        description: task.description.clone(),
        duration_minutes: task.duration_minutes,
        priority: task.priority.clone(),
        difficulty: task.difficulty.clone(),
        sort_order: task.sort_order,
        status: task.status.clone(),
        created_at: task.created_at.clone(),
        updated_at: task.updated_at.clone(),
        focused: task.status == "IN_PROGRESS" && Some(task.id.as_str()) == focused_task_id,
        category,
    }
}

/// The chrome around `title` as ACTUALLY spelled under the first matching
/// pattern of `matcher` — a copy of `tasks.rs::split_affixes`.
fn split_affixes(title: &str, matcher: &CategoryWithPatterns) -> (String, String, String) {
    let Some(pattern) = first_matching_pattern(title, CalendarScope::Ignore, matcher) else {
        return (String::new(), String::new(), title.to_string());
    };
    match crate::pattern_gen::split_hole(&pattern.regex, title) {
        Ok(split) => (split.prefix, split.suffix, split.hole),
        Err(_) => (String::new(), String::new(), title.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::models::{
        GoogleCalendar, GoogleOAuthToken, NewCalendar, NewCalendarEvent, NewRoutine, NewTask,
        NewTaskCategory, NewTaskCategoryPattern, NewTaskList, NewToken, Task, TaskList,
        UpdateRoutine, UpdateTask, UpdateTaskCategory, UpdateTaskList,
    };
    use crate::oauth::{HttpClient, HttpError};

    // ──────────────────────────────────────────
    // Fakes
    // ──────────────────────────────────────────

    /// In-memory `TaskListRepo` for the taxonomy seed path (same as the
    /// routines tests).
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

        async fn get_by_id(&self, _id: &str) -> Result<Option<TaskList>, RepoError> {
            Ok(None)
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
    /// `ensure_taxonomy` and the matcher work end to end (same as the
    /// routines tests).
    struct FakeTaskCategoryRepo {
        stored: Mutex<Vec<crate::models::TaskCategory>>,
        patterns: Mutex<HashMap<String, Vec<crate::models::TaskCategoryPattern>>>,
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

        async fn list_patterns_by_user_id(
            &self,
            user_id: &str,
        ) -> Result<Vec<TaskCategoryPattern>, RepoError> {
            let living: Vec<String> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.deleted_at.is_none())
                .map(|row| row.id.clone())
                .collect();
            let patterns = self.patterns.lock().unwrap();
            let mut all: Vec<TaskCategoryPattern> = living
                .iter()
                .filter_map(|id| patterns.get(id))
                .flatten()
                .cloned()
                .collect();
            all.sort_by(|a, b| a.category_id.cmp(&b.category_id).then(a.sort_order.cmp(&b.sort_order)));
            Ok(all)
        }

        async fn replace_patterns(
            &self,
            category_id: &str,
            patterns: Vec<NewTaskCategoryPattern>,
        ) -> Result<(), RepoError> {
            self.patterns.lock().unwrap().insert(
                category_id.to_string(),
                patterns
                    .into_iter()
                    .enumerate()
                    .map(|(sort_order, input)| TaskCategoryPattern {
                        id: format!("{category_id}-pat-{sort_order}"),
                        category_id: category_id.to_string(),
                        regex: input.regex,
                        google_calendar_id: input.google_calendar_id,
                        sort_order: sort_order as i64,
                        created_at: "2026-08-18T00:00:00Z".to_string(),
                        updated_at: "2026-08-18T00:00:00Z".to_string(),
                    })
                    .collect(),
            );
            Ok(())
        }

        async fn delete_patterns_by_category_id(&self, _category_id: &str) -> Result<(), RepoError> {
            Ok(())
        }
    }

    /// In-memory `RoutineRepo` mirroring D1 semantics (same as the routines
    /// tests): soft-deleted rows are filtered from reads.
    struct FakeRoutineRepo {
        stored: Mutex<Vec<Routine>>,
        next_id: Mutex<u64>,
    }

    impl FakeRoutineRepo {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
            }
        }

        fn row(
            id: &str,
            user_id: &str,
            title: &str,
            sort_order: i64,
            dtstart: &str,
            rrule: &str,
            exdates: &str,
        ) -> Routine {
            Routine {
                id: id.to_string(),
                user_id: user_id.to_string(),
                title: title.to_string(),
                estimated_minutes: 15,
                dtstart: dtstart.to_string(),
                rrule: rrule.to_string(),
                exdates: exdates.to_string(),
                sort_order,
                created_at: "2026-08-18T00:00:00Z".to_string(),
                updated_at: "2026-08-18T00:00:00Z".to_string(),
                deleted_at: None,
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl RoutineRepo for FakeRoutineRepo {
        async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<Routine>, RepoError> {
            let mut rows: Vec<Routine> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.deleted_at.is_none())
                .cloned()
                .collect();
            rows.sort_by(|a, b| (a.sort_order, &a.created_at).cmp(&(b.sort_order, &b.created_at)));
            Ok(rows)
        }

        async fn get_by_id(&self, id: &str) -> Result<Option<Routine>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|row| row.id == id && row.deleted_at.is_none())
                .cloned())
        }

        async fn insert(&self, routine: NewRoutine) -> Result<Routine, RepoError> {
            let mut next = self.next_id.lock().unwrap();
            let row = Routine {
                id: format!("rt-{next}"),
                user_id: routine.user_id,
                title: routine.title,
                estimated_minutes: routine.estimated_minutes,
                dtstart: routine.dtstart,
                rrule: routine.rrule,
                exdates: routine.exdates_json,
                sort_order: routine.sort_order,
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
            updates: &UpdateRoutine,
        ) -> Result<Option<Routine>, RepoError> {
            // Mirrors ROUTINE_UPDATE_SQL (COALESCE semantics): present fields
            // are applied, the row is returned, `None` when missing/deleted.
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
            if let Some(estimated_minutes) = updates.estimated_minutes {
                row.estimated_minutes = estimated_minutes;
            }
            if let Some(dtstart) = &updates.dtstart {
                row.dtstart = dtstart.clone();
            }
            if let Some(rrule) = &updates.rrule {
                row.rrule = rrule.clone();
            }
            if let Some(exdates) = &updates.exdates {
                row.exdates = serde_json::to_string(exdates).unwrap();
            }
            if let Some(sort_order) = updates.sort_order {
                row.sort_order = sort_order;
            }
            row.updated_at = "2026-08-18T01:00:00Z".to_string();
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
            }
            Ok(())
        }

        async fn max_sort_order(&self, user_id: &str) -> Result<Option<i64>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.deleted_at.is_none())
                .map(|row| row.sort_order)
                .max())
        }
    }

    /// In-memory `OccurrenceRepo`: no soft-delete (the table has none); the
    /// insert mirrors `INSERT OR IGNORE` — a duplicate `(routine_id,
    /// local_date)` returns the existing row.
    struct FakeOccurrenceRepo {
        stored: Mutex<Vec<RoutineOccurrence>>,
        next_id: Mutex<u64>,
    }

    impl FakeOccurrenceRepo {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
            }
        }

        fn row(id: &str, routine_id: &str, user_id: &str, date: &str, status: &str) -> RoutineOccurrence {
            RoutineOccurrence {
                id: id.to_string(),
                routine_id: routine_id.to_string(),
                user_id: user_id.to_string(),
                local_date: date.to_string(),
                title: None,
                status: status.to_string(),
                calendar_id: None,
                google_event_id: None,
                created_at: "2026-08-23T00:00:00Z".to_string(),
                updated_at: "2026-08-23T01:00:00Z".to_string(),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl OccurrenceRepo for FakeOccurrenceRepo {
        async fn get_by_id(&self, id: &str) -> Result<Option<RoutineOccurrence>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|row| row.id == id)
                .cloned())
        }

        async fn get_by_routine_and_date(
            &self,
            routine_id: &str,
            local_date: &str,
        ) -> Result<Option<RoutineOccurrence>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|row| row.routine_id == routine_id && row.local_date == local_date)
                .cloned())
        }

        async fn list_by_user_and_date(
            &self,
            user_id: &str,
            local_date: &str,
        ) -> Result<Vec<RoutineOccurrence>, RepoError> {
            let mut rows: Vec<RoutineOccurrence> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.local_date == local_date)
                .cloned()
                .collect();
            rows.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
            Ok(rows)
        }

        async fn insert(&self, occurrence: NewRoutineOccurrence) -> Result<RoutineOccurrence, RepoError> {
            let mut stored = self.stored.lock().unwrap();
            // INSERT OR IGNORE semantics: the UNIQUE (routine_id, local_date)
            // slot already holds a row → return it unchanged.
            if let Some(existing) = stored
                .iter()
                .find(|row| row.routine_id == occurrence.routine_id && row.local_date == occurrence.local_date)
            {
                return Ok(existing.clone());
            }
            let mut next = self.next_id.lock().unwrap();
            let row = RoutineOccurrence {
                id: format!("occ-{next}"),
                routine_id: occurrence.routine_id,
                user_id: occurrence.user_id,
                local_date: occurrence.local_date,
                title: None,
                status: OCCURRENCE_STATUS_PENDING.to_string(),
                calendar_id: None,
                google_event_id: None,
                created_at: "2026-08-23T00:00:00Z".to_string(),
                updated_at: "2026-08-23T00:00:00Z".to_string(),
            };
            *next += 1;
            stored.push(row.clone());
            Ok(row)
        }

        async fn update_title(&self, id: &str, title: Option<&str>) -> Result<(), RepoError> {
            if let Some(row) = self.stored.lock().unwrap().iter_mut().find(|row| row.id == id) {
                row.title = title.map(|t| t.to_string());
                row.updated_at = "2026-08-23T02:00:00Z".to_string();
            }
            Ok(())
        }

        async fn set_event_ids(
            &self,
            id: &str,
            calendar_id: &str,
            google_event_id: &str,
        ) -> Result<(), RepoError> {
            if let Some(row) = self.stored.lock().unwrap().iter_mut().find(|row| row.id == id) {
                row.calendar_id = Some(calendar_id.to_string());
                row.google_event_id = Some(google_event_id.to_string());
                row.updated_at = "2026-08-23T02:00:00Z".to_string();
            }
            Ok(())
        }

        async fn set_status(&self, id: &str, status: &str) -> Result<(), RepoError> {
            if let Some(row) = self.stored.lock().unwrap().iter_mut().find(|row| row.id == id) {
                row.status = status.to_string();
                row.updated_at = "2026-08-23T02:00:00Z".to_string();
            }
            Ok(())
        }

        async fn set_local_date(&self, id: &str, local_date: &str) -> Result<(), RepoError> {
            if let Some(row) = self.stored.lock().unwrap().iter_mut().find(|row| row.id == id) {
                row.local_date = local_date.to_string();
                row.updated_at = "2026-08-23T02:00:00Z".to_string();
            }
            Ok(())
        }

        async fn list_in_progress(&self) -> Result<Vec<RoutineOccurrence>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| {
                    row.status == OCCURRENCE_STATUS_IN_PROGRESS
                        && row.calendar_id.is_some()
                        && row.google_event_id.is_some()
                })
                .cloned()
                .collect())
        }
    }

    /// In-memory `AgendaItemRepo`: hard-delete semantics; the insert mirrors
    /// `INSERT OR IGNORE` — a duplicate key returns the existing row (stored
    /// sort_order wins).
    struct FakeAgendaItemRepo {
        stored: Mutex<Vec<AgendaItem>>,
        next_id: Mutex<u64>,
    }

    impl FakeAgendaItemRepo {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl AgendaItemRepo for FakeAgendaItemRepo {
        async fn list_by_user_and_date(
            &self,
            user_id: &str,
            local_date: &str,
        ) -> Result<Vec<AgendaItem>, RepoError> {
            let mut rows: Vec<AgendaItem> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.local_date == local_date)
                .cloned()
                .collect();
            rows.sort_by(|a, b| (a.sort_order, &a.created_at).cmp(&(b.sort_order, &b.created_at)));
            Ok(rows)
        }

        async fn get_by_id(&self, id: &str) -> Result<Option<AgendaItem>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|row| row.id == id)
                .cloned())
        }

        async fn get_by_key(
            &self,
            user_id: &str,
            local_date: &str,
            kind: &str,
            ref_id: &str,
        ) -> Result<Option<AgendaItem>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|row| {
                    row.user_id == user_id
                        && row.local_date == local_date
                        && row.kind == kind
                        && row.ref_id == ref_id
                })
                .cloned())
        }

        async fn insert(&self, item: NewAgendaItem) -> Result<AgendaItem, RepoError> {
            let mut stored = self.stored.lock().unwrap();
            // INSERT OR IGNORE semantics: the UNIQUE key already holds a row →
            // return it unchanged (an add never reshuffles an existing item).
            if let Some(existing) = stored.iter().find(|row| {
                row.user_id == item.user_id
                    && row.local_date == item.local_date
                    && row.kind == item.kind
                    && row.ref_id == item.ref_id
            }) {
                return Ok(existing.clone());
            }
            let mut next = self.next_id.lock().unwrap();
            let row = AgendaItem {
                id: format!("ai-{next}"),
                user_id: item.user_id,
                local_date: item.local_date,
                kind: item.kind,
                ref_id: item.ref_id,
                sort_order: item.sort_order,
                created_at: "2026-08-23T00:00:00Z".to_string(),
                updated_at: "2026-08-23T00:00:00Z".to_string(),
            };
            *next += 1;
            stored.push(row.clone());
            Ok(row)
        }

        async fn hard_delete(&self, id: &str) -> Result<(), RepoError> {
            self.stored.lock().unwrap().retain(|row| row.id != id);
            Ok(())
        }

        async fn max_sort_order(&self, user_id: &str, local_date: &str) -> Result<Option<i64>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.local_date == local_date)
                .map(|row| row.sort_order)
                .max())
        }

        async fn set_sort_order(&self, id: &str, sort_order: i64) -> Result<(), RepoError> {
            // Mirrors AGENDA_ITEM_SET_SORT_ORDER_SQL: rank only, no updated_at.
            if let Some(row) = self.stored.lock().unwrap().iter_mut().find(|row| row.id == id) {
                row.sort_order = sort_order;
            }
            Ok(())
        }

        async fn set_local_date(
            &self,
            id: &str,
            local_date: &str,
            sort_order: i64,
        ) -> Result<(), RepoError> {
            // Mirrors AGENDA_ITEM_SET_LOCAL_DATE_SQL: date + rank, no updated_at.
            if let Some(row) = self.stored.lock().unwrap().iter_mut().find(|row| row.id == id) {
                row.local_date = local_date.to_string();
                row.sort_order = sort_order;
            }
            Ok(())
        }

        async fn shift_sort_order(
            &self,
            user_id: &str,
            local_date: &str,
            from_rank: i64,
            delta: i64,
        ) -> Result<(), RepoError> {
            // Mirrors AGENDA_ITEM_SHIFT_SORT_ORDER_SQL: peers only, no updated_at.
            for row in self.stored.lock().unwrap().iter_mut() {
                if row.user_id == user_id && row.local_date == local_date && row.sort_order >= from_rank {
                    row.sort_order += delta;
                }
            }
            Ok(())
        }
    }

    /// In-memory `TaskRepo`: living filter on reads; soft delete stamps
    /// `deleted_at`.
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

        fn push(&self, task: Task) {
            self.stored.lock().unwrap().push(task);
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
            rows.sort_by(|a, b| {
                (a.status.as_str(), a.sort_order, &a.created_at).cmp(&(b.status.as_str(), b.sort_order, &b.created_at))
            });
            Ok(rows)
        }

        async fn list_in_progress(&self) -> Result<Vec<Task>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.status == "IN_PROGRESS" && row.deleted_at.is_none())
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
                user_id: task.user_id,
                title: task.title,
                description: task.description,
                duration_minutes: task.duration_minutes,
                priority: task.priority,
                difficulty: task.difficulty,
                sort_order: task.sort_order,
                status: "OPEN".to_string(),
                created_at: "2026-08-23T00:00:00Z".to_string(),
                updated_at: "2026-08-23T00:00:00Z".to_string(),
                deleted_at: None,
            };
            *next += 1;
            self.stored.lock().unwrap().push(row.clone());
            Ok(row)
        }

        async fn shift_sort_order(
            &self,
            _user_id: &str,
            _status: &str,
            _from_inclusive: i64,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn shift_sort_order_by(
            &self,
            _user_id: &str,
            _status: &str,
            _from_inclusive: i64,
            _to_inclusive: i64,
            _delta: i64,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn update(
            &self,
            _id: &str,
            _updates: &UpdateTask,
        ) -> Result<Option<Task>, RepoError> {
            Ok(None)
        }

        async fn set_status(
            &self,
            id: &str,
            status: &str,
            now_rfc3339: &str,
        ) -> Result<Option<Task>, RepoError> {
            let mut stored = self.stored.lock().unwrap();
            let Some(row) = stored.iter_mut().find(|row| row.id == id && row.deleted_at.is_none()) else {
                return Ok(None);
            };
            row.status = status.to_string();
            row.updated_at = now_rfc3339.to_string();
            Ok(Some(row.clone()))
        }

        async fn set_sort_order(&self, id: &str, sort_order: i64) -> Result<Option<Task>, RepoError> {
            let mut stored = self.stored.lock().unwrap();
            let Some(row) = stored.iter_mut().find(|row| row.id == id && row.deleted_at.is_none()) else {
                return Ok(None);
            };
            row.sort_order = sort_order;
            Ok(Some(row.clone()))
        }

        async fn max_sort_order(
            &self,
            user_id: &str,
            status: &str,
            exclude_id: Option<&str>,
        ) -> Result<Option<i64>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| {
                    row.user_id == user_id
                        && row.status == status
                        && row.deleted_at.is_none()
                        && Some(row.id.as_str()) != exclude_id
                })
                .map(|row| row.sort_order)
                .max())
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
            }
            Ok(())
        }
    }

    /// Scripted HTTP fake with POST/PATCH routes — enough for the
    /// occurrence verbs' `events.insert`, `events.patch` and summary PATCH
    /// calls (a copy of the tasks tests' fake).
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

    /// In-memory `CalendarRepo`: a fixed set of stored calendars.
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

    /// In-memory event repo: upserts materialize `CalendarEvent` rows (so
    /// `get_by_calendar_and_google_id` sees them — the close/elongate paths
    /// resolve the cached row first) and every write is recorded.
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
            self.upserted.lock().unwrap().push(event.clone());
            let mut stored = self.stored.lock().unwrap();
            // Mirrors the ON CONFLICT(calendar_id, google_event_id) replace.
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
            _now_rfc3339: &str,
        ) -> Result<Vec<CalendarEvent>, RepoError> {
            Ok(self.stored.lock().unwrap().clone())
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

    /// Token repo for the elongate cron tests: returns a stored token per
    /// user (expiring far in the future, so `refresh_if_needed` never POSTs).
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

    struct Repos {
        lists: FakeTaskListRepo,
        categories: FakeTaskCategoryRepo,
        routines: FakeRoutineRepo,
        occurrences: FakeOccurrenceRepo,
        agenda: FakeAgendaItemRepo,
        tasks: FakeTaskRepo,
    }

    fn repos() -> Repos {
        Repos {
            lists: FakeTaskListRepo::new(),
            categories: FakeTaskCategoryRepo::new(),
            routines: FakeRoutineRepo::new(),
            occurrences: FakeOccurrenceRepo::new(),
            agenda: FakeAgendaItemRepo::new(),
            tasks: FakeTaskRepo::new(),
        }
    }

    fn task_row(id: &str, user_id: &str, title: &str, status: &str) -> Task {
        Task {
            id: id.to_string(),
            user_id: user_id.to_string(),
            title: title.to_string(),
            description: String::new(),
            duration_minutes: 15,
            priority: "medium".to_string(),
            difficulty: "easy".to_string(),
            sort_order: 0,
            status: status.to_string(),
            created_at: "2026-08-23T00:00:00Z".to_string(),
            updated_at: "2026-08-23T00:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    fn agenda_input(kind: &str, ref_id: &str, date: &str) -> NewAgendaItemInput {
        NewAgendaItemInput {
            kind: kind.to_string(),
            ref_id: ref_id.to_string(),
            sort_order: None,
            date: date.to_string(),
        }
    }

    fn get(repos: &Repos, user_id: &str, date: &str) -> Result<AgendaResponse, AgendaError> {
        pollster::block_on(get_agenda(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &repos.agenda,
            &repos.tasks,
            user_id,
            date,
            None,
        ))
    }

    fn add(
        repos: &Repos,
        user_id: &str,
        input: &NewAgendaItemInput,
    ) -> Result<AgendaItemResponse, AgendaError> {
        pollster::block_on(add_agenda_item(
            &repos.lists,
            &repos.categories,
            &repos.agenda,
            &repos.tasks,
            user_id,
            input,
            None,
        ))
    }

    fn move_item(
        repos: &Repos,
        user_id: &str,
        id: &str,
        sort_order: i64,
    ) -> Result<AgendaItemResponse, AgendaError> {
        pollster::block_on(move_agenda_item(
            &repos.lists,
            &repos.categories,
            &repos.agenda,
            &repos.tasks,
            &repos.occurrences,
            &repos.routines,
            user_id,
            id,
            sort_order,
            None,
        ))
    }

    fn reschedule(
        repos: &Repos,
        user_id: &str,
        id: &str,
        date: &str,
    ) -> Result<AgendaItemResponse, AgendaError> {
        pollster::block_on(reschedule_agenda_item(
            &repos.lists,
            &repos.categories,
            &repos.agenda,
            &repos.tasks,
            &repos.occurrences,
            &repos.routines,
            user_id,
            id,
            date,
            None,
        ))
    }

    fn complete(repos: &Repos, user_id: &str, id: &str) -> Result<OccurrenceResponse, AgendaError> {
        pollster::block_on(complete_occurrence(
            None, None, None, None,
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            user_id,
            id,
            NOW_UNIX,
        ))
    }

    fn skip(repos: &Repos, user_id: &str, id: &str) -> Result<OccurrenceResponse, AgendaError> {
        pollster::block_on(skip_occurrence(
            None, None, None, None,
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            user_id,
            id,
            NOW_UNIX,
        ))
    }

    fn patch(
        repos: &Repos,
        user_id: &str,
        id: &str,
        updates: &UpdateOccurrence,
    ) -> Result<OccurrenceResponse, AgendaError> {
        pollster::block_on(patch_occurrence(
            None, None, None, None,
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            user_id,
            id,
            updates,
            NOW_UNIX,
        ))
    }

    // ──────────────────────────────────────────
    // Google fixtures (slice 6)
    // ──────────────────────────────────────────

    /// `2026-08-23T10:00:00Z` — its civil date is `2026-08-23` in both UTC
    /// and Asia/Kolkata, so occurrence `local_date` fixtures of `2026-08-23`
    /// pass the start's today-only gate.
    const NOW_UNIX: i64 = 1_787_479_200;

    fn access() -> GoogleAccess {
        GoogleAccess {
            access_token: "at-1".to_string(),
            token_type: "Bearer".to_string(),
        }
    }

    /// A writable primary calendar for `u-1` — the default start target.
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

    /// The Google `events.insert` echo for an occurrence start — the exact
    /// shape Google returns (including the two occurrence carriers and the
    /// summary it was sent, i.e. the resolved title).
    fn created_occurrence_json(routine_id: &str, occurrence_id: &str, start: &str, end: &str) -> String {
        format!(
            r#"{{"id":"g-1","summary":"Fajr Qadha","start":{{"dateTime":"{start}"}},"end":{{"dateTime":"{end}"}},"extendedProperties":{{"shared":{{"sanctuary_routine_id":"{routine_id}","sanctuary_occurrence_id":"{occurrence_id}"}}}}}}"#
        )
    }

    /// The `events.patch` echo: same occurrence event with a new end.
    fn patched_occurrence_json(routine_id: &str, occurrence_id: &str, start: &str, end: &str) -> String {
        format!(
            r#"{{"id":"g-1","summary":"Fajr Qadha","start":{{"dateTime":"{start}"}},"end":{{"dateTime":"{end}"}},"extendedProperties":{{"shared":{{"sanctuary_routine_id":"{routine_id}","sanctuary_occurrence_id":"{occurrence_id}"}}}}}}"#
        )
    }

    /// A cached `calendar_events` row for an occurrence's chip (the close and
    /// elongate paths resolve it through the occurrence's stored ids).
    fn cached_occurrence_event(
        calendar_id: &str,
        google_id: &str,
        start: &str,
        end: &str,
    ) -> CalendarEvent {
        CalendarEvent {
            id: "evt-1".to_string(),
            calendar_id: calendar_id.to_string(),
            google_event_id: google_id.to_string(),
            google_etag: String::new(),
            google_updated_at: String::new(),
            last_synced_at: "2026-08-23T00:00:00Z".to_string(),
            title: "Fajr".to_string(),
            description: String::new(),
            start_time: start.to_string(),
            end_time: end.to_string(),
            recurrence: String::new(),
            // Occurrence events NEVER carry a task link — the two worlds stay
            // apart (the cache maps only sanctuary_task_id onto this column).
            task_id: String::new(),
            created_at: "2026-08-23T00:00:00Z".to_string(),
            updated_at: "2026-08-23T00:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    /// A stored OAuth token whose expiry is centuries out, so
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

    /// OAuth client credentials for the elongate test; the fresh token above
    /// means `refresh_if_needed` never uses them.
    fn oauth_config() -> OAuthConfig {
        OAuthConfig {
            client_id: "client-id.apps.googleusercontent.com".to_string(),
            client_secret: "client-secret".to_string(),
            redirect_url: "http://localhost:5173/auth/google/callback".to_string(),
        }
    }

    /// Wires a Fajr routine + occurrence of `status` into `repos` and returns
    /// the occurrence id.
    fn seed_occurrence(repos: &Repos, status: &str) -> String {
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        ));
        let mut occurrence =
            FakeOccurrenceRepo::row("occ-1", "rt-1", "u-1", "2026-08-23", status);
        occurrence.title = Some("Fajr Qadha".to_string());
        repos.occurrences.stored.lock().unwrap().push(occurrence);
        "occ-1".to_string()
    }

    /// `POST /api/occurrences/:id/start` with the default primary calendar and
    /// a scripted insert echo.
    fn start(repos: &Repos, user_id: &str, id: &str) -> Result<OccurrenceActionResponse, AgendaError> {
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events",
            200,
            &created_occurrence_json("rt-1", id, "2026-08-23T10:00:00Z", "2026-08-23T10:15:00Z"),
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        pollster::block_on(start_occurrence(
            &http,
            &calendars,
            &events,
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &access(),
            user_id,
            id,
            NOW_UNIX,
        ))
    }

    // ──────────────────────────────────────────
    // GET /api/agenda
    // ──────────────────────────────────────────

    #[test]
    fn get_agenda_rejects_missing_or_invalid_date() {
        let repos = repos();
        for bad in [
            "",
            "   ",
            "2026-13-01",
            "2026-00-10",
            "23-08-2026",
            "2026-08-23T00:00:00",
            "not a date",
        ] {
            let err = get(&repos, "u-1", bad).unwrap_err();
            assert!(
                matches!(err, AgendaError::Invalid(ref m) if m == "date must be YYYY-MM-DD"),
                "{err:?} for {bad:?}"
            );
        }
    }

    #[test]
    fn get_agenda_seeds_daily_routine_occurrence_and_item() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        ));

        let response = get(&repos, "u-1", "2026-08-23").unwrap();
        assert_eq!(response.items.len(), 1);
        let item = &response.items[0];
        assert_eq!(item.kind, AGENDA_KIND_OCCURRENCE);
        assert_eq!(item.local_date, "2026-08-23");
        assert_eq!(item.sort_order, 0);
        assert!(item.task.is_none(), "occurrence items embed no task");
        let occurrence = item.occurrence.as_ref().expect("occurrence embedded");
        assert_eq!(occurrence.status, OCCURRENCE_STATUS_PENDING);
        assert_eq!(occurrence.local_date, "2026-08-23");
        assert_eq!(occurrence.title, None, "seeding copies no title — inheritance stays live");
        assert_eq!(occurrence.resolved_title, "Fajr", "resolved = override ?? routine.title");
        assert_eq!(occurrence.estimated_minutes, 15, "estimate comes from the routine");
        assert_eq!(occurrence.dtstart, "2026-01-01T05:30:00");
        assert_eq!(occurrence.rrule, "FREQ=DAILY");
        assert_eq!(occurrence.exdates, Vec::<String>::new());
        assert_eq!(occurrence.calendar_id, None);
        assert_eq!(occurrence.google_event_id, None);
        assert!(
            occurrence.category.is_untracked,
            "no pattern matches Fajr → untracked summary, never a 400"
        );
        // Both rows exist in the fakes.
        assert_eq!(repos.occurrences.stored.lock().unwrap().len(), 1);
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), 1);
    }

    #[test]
    fn get_agenda_twice_is_idempotent() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        ));

        let first = get(&repos, "u-1", "2026-08-23").unwrap();
        let second = get(&repos, "u-1", "2026-08-23").unwrap();
        assert_eq!(first.items.len(), 1);
        assert_eq!(second.items.len(), 1);
        assert_eq!(
            first.items[0].id, second.items[0].id,
            "same agenda item id on the second GET"
        );
        assert_eq!(
            first.items[0].occurrence.as_ref().unwrap().id,
            second.items[0].occurrence.as_ref().unwrap().id,
            "same occurrence id on the second GET"
        );
        assert_eq!(repos.occurrences.stored.lock().unwrap().len(), 1, "no dupes");
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), 1, "no dupes");
    }

    #[test]
    fn weekly_routine_seeds_matching_dates_only() {
        let repos = repos();
        // 2026-08-17 is a Monday: BYDAY=MO,WE covers the 17th and the 19th.
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Standup",
            0,
            "2026-08-17T09:00:00",
            "FREQ=WEEKLY;BYDAY=MO,WE",
            "[]",
        ));

        let matching = get(&repos, "u-1", "2026-08-19").unwrap();
        assert_eq!(matching.items.len(), 1, "Wednesday matches");
        let non_matching = get(&repos, "u-1", "2026-08-20").unwrap();
        assert_eq!(non_matching.items.len(), 0, "Thursday does not match");
        assert_eq!(
            repos.occurrences.stored.lock().unwrap().len(),
            1,
            "no occurrence was seeded for the non-matching date"
        );
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), 1);
    }

    #[test]
    fn exdates_exclude_the_seeded_day() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            r#"["2026-08-23"]"#,
        ));

        let excluded = get(&repos, "u-1", "2026-08-23").unwrap();
        assert_eq!(excluded.items.len(), 0, "exdated day never seeds");
        assert_eq!(repos.occurrences.stored.lock().unwrap().len(), 0);
        let next_day = get(&repos, "u-1", "2026-08-24").unwrap();
        assert_eq!(next_day.items.len(), 1, "other days still seed");
    }

    #[test]
    fn soft_deleted_routine_never_seeds_and_leftover_item_is_omitted() {
        let repos = repos();
        let mut routine = FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        );
        routine.deleted_at = Some("2026-08-20T00:00:00Z".to_string());
        repos.routines.stored.lock().unwrap().push(routine);
        // A leftover materialized occurrence + membership row from when the
        // routine was living.
        repos
            .occurrences
            .stored
            .lock()
            .unwrap()
            .push(FakeOccurrenceRepo::row("occ-1", "rt-1", "u-1", "2026-08-23", "pending"));
        repos.agenda.stored.lock().unwrap().push(AgendaItem {
            id: "ai-1".to_string(),
            user_id: "u-1".to_string(),
            local_date: "2026-08-23".to_string(),
            kind: AGENDA_KIND_OCCURRENCE.to_string(),
            ref_id: "occ-1".to_string(),
            sort_order: 0,
            created_at: "2026-08-23T00:00:00Z".to_string(),
            updated_at: "2026-08-23T00:00:00Z".to_string(),
        });

        let response = get(&repos, "u-1", "2026-08-23").unwrap();
        assert_eq!(response.items.len(), 0, "orphan occurrence item is omitted");
        assert_eq!(
            repos.occurrences.stored.lock().unwrap().len(),
            1,
            "no new occurrence seeded for a soft-deleted routine"
        );
        assert_eq!(
            repos.agenda.stored.lock().unwrap().len(),
            1,
            "the orphan membership row stays in the store"
        );
    }

    #[test]
    fn soft_deleted_task_leftover_item_is_omitted() {
        let repos = repos();
        let mut task = task_row("t-1", "u-1", "Review | Work", "OPEN");
        task.deleted_at = Some("2026-08-20T00:00:00Z".to_string());
        repos.tasks.push(task);
        repos.agenda.stored.lock().unwrap().push(AgendaItem {
            id: "ai-1".to_string(),
            user_id: "u-1".to_string(),
            local_date: "2026-08-23".to_string(),
            kind: AGENDA_KIND_TASK.to_string(),
            ref_id: "t-1".to_string(),
            sort_order: 0,
            created_at: "2026-08-23T00:00:00Z".to_string(),
            updated_at: "2026-08-23T00:00:00Z".to_string(),
        });

        let response = get(&repos, "u-1", "2026-08-23").unwrap();
        assert_eq!(response.items.len(), 0, "soft-deleted task item is omitted");
        assert_eq!(
            repos.agenda.stored.lock().unwrap().len(),
            1,
            "the orphan membership row stays in the store"
        );
    }

    #[test]
    fn seeding_after_user_reorder_appends_without_reshuffling() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().extend([
            FakeRoutineRepo::row("rt-a", "u-1", "Fajr", 0, "2026-01-01T05:30:00", "FREQ=DAILY", "[]"),
            FakeRoutineRepo::row("rt-b", "u-1", "Salat", 1, "2026-01-01T12:00:00", "FREQ=DAILY", "[]"),
        ]);

        let first = get(&repos, "u-1", "2026-08-23").unwrap();
        assert_eq!(first.items.len(), 2);
        let fajr_occ = first.items[0].occurrence.as_ref().unwrap().id.clone();
        let salat_occ = first.items[1].occurrence.as_ref().unwrap().id.clone();
        let salat_item_id = first.items[1].id.clone();

        // The user reorders: Salat to the top of the pile.
        let moved = move_item(&repos, "u-1", &salat_item_id, 0).unwrap();
        assert_eq!(moved.item.sort_order, 0);
        assert_eq!(moved.item.id, salat_item_id);

        // A third routine appears; the next GET must append it AFTER the
        // reordered pair, leaving their ranks untouched.
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-c",
            "u-1",
            "Work",
            2,
            "2026-01-01T09:00:00",
            "FREQ=DAILY",
            "[]",
        ));
        let second = get(&repos, "u-1", "2026-08-23").unwrap();
        let ranks: Vec<(String, i64)> = second
            .items
            .iter()
            .map(|item| (item.occurrence.as_ref().unwrap().id.clone(), item.sort_order))
            .collect();
        assert_eq!(
            ranks,
            vec![
                (salat_occ.clone(), 0),
                (fajr_occ.clone(), 1),
                // New seed appends at max+1.
                (second.items[2].occurrence.as_ref().unwrap().id.clone(), 2),
            ],
            "existing ranks untouched, new occurrence appended"
        );
        assert_eq!(ranks[2].0, "occ-3", "third seeded occurrence in standing order");
    }

    #[test]
    fn multiple_routines_seed_in_standing_sort_order_when_list_is_empty() {
        let repos = repos();
        // Standing order is 0,1,2 regardless of insertion order.
        repos.routines.stored.lock().unwrap().extend([
            FakeRoutineRepo::row("rt-a", "u-1", "A", 2, "2026-01-01T05:30:00", "FREQ=DAILY", "[]"),
            FakeRoutineRepo::row("rt-b", "u-1", "B", 0, "2026-01-01T05:30:00", "FREQ=DAILY", "[]"),
            FakeRoutineRepo::row("rt-c", "u-1", "C", 1, "2026-01-01T05:30:00", "FREQ=DAILY", "[]"),
        ]);

        let response = get(&repos, "u-1", "2026-08-23").unwrap();
        let titles: Vec<(&str, i64)> = response
            .items
            .iter()
            .map(|item| {
                (
                    item.occurrence.as_ref().unwrap().resolved_title.as_str(),
                    item.sort_order,
                )
            })
            .collect();
        assert_eq!(titles, vec![("B", 0), ("C", 1), ("A", 2)]);
    }

    // ──────────────────────────────────────────
    // POST /api/agenda/items
    // ──────────────────────────────────────────

    #[test]
    fn add_task_happy_path_and_idempotent_duplicate() {
        let repos = repos();
        repos.tasks.push(task_row("t-1", "u-1", "Review | Work", "OPEN"));

        let response = add(&repos, "u-1", &agenda_input("task", "t-1", "2026-08-23")).unwrap();
        assert_eq!(response.item.kind, AGENDA_KIND_TASK);
        assert_eq!(response.item.ref_id, "t-1");
        assert_eq!(response.item.sort_order, 0, "empty pile appends at 0");
        let task = response.item.task.as_ref().expect("task embedded");
        assert_eq!(task.id, "t-1");
        assert_eq!(task.status, "OPEN");
        assert!(!task.focused);
        assert!(response.item.occurrence.is_none());

        // Duplicate → idempotent 200 with the SAME item id, sort_order kept.
        let again = add(&repos, "u-1", &agenda_input("task", "t-1", "2026-08-23")).unwrap();
        assert_eq!(again.item.id, response.item.id, "same membership row");
        assert_eq!(again.item.sort_order, 0, "duplicate never reshuffles");
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), 1, "no duplicate rows");

        // Second task appends at max+1.
        repos.tasks.push(task_row("t-2", "u-1", "Deep Work | Work", "PLANNED"));
        let second = add(&repos, "u-1", &agenda_input("task", "t-2", "2026-08-23")).unwrap();
        assert_eq!(second.item.sort_order, 1);
        // tasks.status is untouched by membership.
        let stored = repos.tasks.stored.lock().unwrap();
        assert_eq!(stored.iter().find(|t| t.id == "t-2").unwrap().status, "PLANNED");
    }

    #[test]
    fn add_task_rejects_terminal_foreign_and_missing_tasks() {
        let repos = repos();
        repos.tasks.push(task_row("t-done", "u-1", "Done | Work", "COMPLETED"));
        repos.tasks.push(task_row("t-disc", "u-1", "Disc | Work", "DISCARDED"));
        repos.tasks.push(task_row("t-other", "u-2", "Other | Work", "OPEN"));

        // COMPLETED / DISCARDED → 400.
        for id in ["t-done", "t-disc"] {
            let err = add(&repos, "u-1", &agenda_input("task", id, "2026-08-23")).unwrap_err();
            assert!(
                matches!(err, AgendaError::Invalid(ref m) if m == "task cannot be added to the agenda"),
                "{err:?}"
            );
        }
        // Missing task → 404.
        let err = add(&repos, "u-1", &agenda_input("task", "nope", "2026-08-23")).unwrap_err();
        assert!(matches!(err, AgendaError::NotFound), "{err:?}");
        // Another user's task → 404 (never leak existence).
        let err = add(&repos, "u-1", &agenda_input("task", "t-other", "2026-08-23")).unwrap_err();
        assert!(matches!(err, AgendaError::NotFound), "{err:?}");
        // Soft-deleted task → 404.
        let mut deleted = task_row("t-gone", "u-1", "Gone | Work", "OPEN");
        deleted.deleted_at = Some("2026-08-20T00:00:00Z".to_string());
        repos.tasks.push(deleted);
        let err = add(&repos, "u-1", &agenda_input("task", "t-gone", "2026-08-23")).unwrap_err();
        assert!(matches!(err, AgendaError::NotFound), "{err:?}");
        // Nothing was persisted for any rejection.
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), 0);
    }

    #[test]
    fn add_rejects_non_task_kinds_and_bad_dates_and_ranks() {
        let repos = repos();
        repos.tasks.push(task_row("t-1", "u-1", "Review | Work", "OPEN"));

        let err = add(&repos, "u-1", &agenda_input("occurrence", "occ-1", "2026-08-23")).unwrap_err();
        assert!(matches!(err, AgendaError::Invalid(ref m) if m == "kind must be task"), "{err:?}");
        let err = add(&repos, "u-1", &agenda_input("task", "t-1", "nope")).unwrap_err();
        assert!(matches!(err, AgendaError::Invalid(ref m) if m == "date must be YYYY-MM-DD"), "{err:?}");
        let err = add(
            &repos,
            "u-1",
            &NewAgendaItemInput {
                kind: "task".to_string(),
                ref_id: "t-1".to_string(),
                sort_order: Some(-1),
                date: "2026-08-23".to_string(),
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, AgendaError::Invalid(ref m) if m == "sort_order must be a non-negative integer"),
            "{err:?}"
        );
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), 0);
    }

    // ──────────────────────────────────────────
    // DELETE /api/agenda/items/:id
    // ──────────────────────────────────────────

    #[test]
    fn delete_task_item_unpins_but_keeps_the_task_living() {
        let repos = repos();
        repos.tasks.push(task_row("t-1", "u-1", "Review | Work", "OPEN"));
        let added = add(&repos, "u-1", &agenda_input("task", "t-1", "2026-08-23")).unwrap();

        let deleted = pollster::block_on(delete_agenda_item(&repos.agenda, "u-1", &added.item.id)).unwrap();
        assert!(deleted.success);
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), 0, "membership row hard-deleted");
        let stored = pollster::block_on(repos.tasks.get_by_id("t-1")).unwrap();
        assert_eq!(
            stored.map(|t| t.status),
            Some("OPEN".to_string()),
            "the task itself stays living on the Board"
        );
    }

    #[test]
    fn delete_occurrence_item_is_refused_and_missing_item_is_404() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        ));
        let agenda = get(&repos, "u-1", "2026-08-23").unwrap();

        let err = pollster::block_on(delete_agenda_item(&repos.agenda, "u-1", &agenda.items[0].id))
            .unwrap_err();
        assert!(
            matches!(err, AgendaError::Invalid(ref m) if m == "skip is the decline"),
            "{err:?}"
        );
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), 1, "occurrence item stays");

        let err = pollster::block_on(delete_agenda_item(&repos.agenda, "u-1", "nope")).unwrap_err();
        assert!(matches!(err, AgendaError::NotFound), "{err:?}");
        // Another user's item → 404.
        repos.agenda.stored.lock().unwrap().push(AgendaItem {
            id: "ai-x".to_string(),
            user_id: "u-2".to_string(),
            local_date: "2026-08-23".to_string(),
            kind: AGENDA_KIND_TASK.to_string(),
            ref_id: "t-x".to_string(),
            sort_order: 0,
            created_at: "2026-08-23T00:00:00Z".to_string(),
            updated_at: "2026-08-23T00:00:00Z".to_string(),
        });
        let err = pollster::block_on(delete_agenda_item(&repos.agenda, "u-1", "ai-x")).unwrap_err();
        assert!(matches!(err, AgendaError::NotFound), "{err:?}");
    }

    // ──────────────────────────────────────────
    // POST /api/agenda/items/:id/move
    // ──────────────────────────────────────────

    #[test]
    fn move_reorders_within_the_date_pile_only() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().extend([
            FakeRoutineRepo::row("rt-a", "u-1", "A", 0, "2026-01-01T05:30:00", "FREQ=DAILY", "[]"),
            FakeRoutineRepo::row("rt-b", "u-1", "B", 1, "2026-01-01T05:30:00", "FREQ=DAILY", "[]"),
            FakeRoutineRepo::row("rt-c", "u-1", "C", 2, "2026-01-01T05:30:00", "FREQ=DAILY", "[]"),
        ]);
        // Two dates, same routines — a full pile on each.
        let day1 = get(&repos, "u-1", "2026-08-23").unwrap();
        let day2 = get(&repos, "u-1", "2026-08-24").unwrap();
        assert_eq!(day2.items.len(), 3, "second date seeded independently");
        let c_id = day1.items[2].id.clone();

        // Move C to the top of day 1's pile.
        let moved = move_item(&repos, "u-1", &c_id, 0).unwrap();
        assert_eq!(moved.item.id, c_id);
        assert_eq!(moved.item.sort_order, 0);

        let day1_after: Vec<(String, i64)> = pollster::block_on(repos.agenda.list_by_user_and_date("u-1", "2026-08-23"))
            .unwrap()
            .iter()
            .map(|item| (item.id.clone(), item.sort_order))
            .collect();
        assert_eq!(day1_after, vec![(c_id.clone(), 0), (day1.items[0].id.clone(), 1), (day1.items[1].id.clone(), 2)]);

        // Day 2's pile is untouched.
        let day2_after: Vec<i64> = pollster::block_on(repos.agenda.list_by_user_and_date("u-1", "2026-08-24"))
            .unwrap()
            .iter()
            .map(|item| item.sort_order)
            .collect();
        assert_eq!(day2_after, vec![0, 1, 2], "other dates are never reshuffled");
    }

    #[test]
    fn move_same_rank_is_a_no_op_and_bad_inputs_are_rejected() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-a",
            "u-1",
            "A",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        ));
        let agenda = get(&repos, "u-1", "2026-08-23").unwrap();
        let id = agenda.items[0].id.clone();

        let noop = move_item(&repos, "u-1", &id, 0).unwrap();
        assert_eq!(noop.item.sort_order, 0);
        let stored = pollster::block_on(repos.agenda.get_by_id(&id)).unwrap().unwrap();
        assert_eq!(stored.sort_order, 0);
        assert_eq!(stored.updated_at, "2026-08-23T00:00:00Z", "no-op writes nothing");

        let err = move_item(&repos, "u-1", &id, -1).unwrap_err();
        assert!(
            matches!(err, AgendaError::Invalid(ref m) if m == "sort_order must be a non-negative integer"),
            "{err:?}"
        );
        let err = move_item(&repos, "u-1", "nope", 0).unwrap_err();
        assert!(matches!(err, AgendaError::NotFound), "{err:?}");
        // Other user's item → 404.
        repos.agenda.stored.lock().unwrap().push(AgendaItem {
            id: "ai-x".to_string(),
            user_id: "u-2".to_string(),
            local_date: "2026-08-23".to_string(),
            kind: AGENDA_KIND_TASK.to_string(),
            ref_id: "t-x".to_string(),
            sort_order: 0,
            created_at: "2026-08-23T00:00:00Z".to_string(),
            updated_at: "2026-08-23T00:00:00Z".to_string(),
        });
        let err = move_item(&repos, "u-1", "ai-x", 1).unwrap_err();
        assert!(matches!(err, AgendaError::NotFound), "{err:?}");
    }

    // ──────────────────────────────────────────
    // POST /api/agenda/items/:id/reschedule
    // ──────────────────────────────────────────

    /// A weekly-Monday routine (dtstart 2026-08-17, a Monday) + its seeded
    /// occurrence/item on 2026-08-17. Returns the agenda item id.
    fn seed_monday_occurrence(repos: &Repos) -> String {
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "2026-08-17T05:30:00",
            "FREQ=WEEKLY;BYDAY=MO",
            "[]",
        ));
        let agenda = get(repos, "u-1", "2026-08-17").unwrap();
        assert_eq!(agenda.items.len(), 1, "Monday seeded");
        agenda.items[0].id.clone()
    }

    #[test]
    fn reschedule_weekly_occurrence_exdates_source_and_moves_row() {
        let repos = repos();
        let item_id = seed_monday_occurrence(&repos);
        let occ_id = pollster::block_on(repos.agenda.get_by_id(&item_id))
            .unwrap()
            .unwrap()
            .ref_id
            .clone();

        // Monday → Tuesday.
        let response = reschedule(&repos, "u-1", &item_id, "2026-08-18").unwrap();
        assert_eq!(response.item.id, item_id, "same agenda row, relocated");
        assert_eq!(response.item.local_date, "2026-08-18");
        assert_eq!(response.item.sort_order, 0, "empty Tuesday pile appends at 0");
        let occurrence = response.item.occurrence.as_ref().expect("occurrence embedded");
        assert_eq!(occurrence.id, occ_id, "same occurrence id");
        assert_eq!(occurrence.local_date, "2026-08-18");
        assert_eq!(occurrence.status, OCCURRENCE_STATUS_PENDING);

        // The source date is exdated on the routine — the anti-re-seed lock.
        let routine = pollster::block_on(repos.routines.get_by_id("rt-1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&routine.exdates).unwrap(),
            vec!["2026-08-17".to_string()],
            "source date appended to exdates"
        );

        // Monday GET does NOT re-seed: the rule still matches 2026-08-17, but
        // the exdate keeps it out — the moved-away day stays empty.
        let monday = get(&repos, "u-1", "2026-08-17").unwrap();
        assert_eq!(monday.items.len(), 0, "exdated Monday never re-seeds");
        assert_eq!(repos.occurrences.stored.lock().unwrap().len(), 1, "no new occurrence");

        // Tuesday GET shows the SAME occurrence (not a fresh seed — the rule
        // does not even cover Tuesday, so the moved row is the only one).
        let tuesday = get(&repos, "u-1", "2026-08-18").unwrap();
        assert_eq!(tuesday.items.len(), 1);
        assert_eq!(tuesday.items[0].id, item_id, "same agenda row");
        assert_eq!(
            tuesday.items[0].occurrence.as_ref().unwrap().id,
            occ_id,
            "same occurrence id"
        );
    }

    #[test]
    fn reschedule_daily_occurrence_moves_row_and_seed_creates_no_second() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        ));
        let today = get(&repos, "u-1", "2026-08-23").unwrap();
        assert_eq!(today.items.len(), 1);
        let item_id = today.items[0].id.clone();
        let occ_id = today.items[0].occurrence.as_ref().unwrap().id.clone();

        // Today → tomorrow.
        let response = reschedule(&repos, "u-1", &item_id, "2026-08-24").unwrap();
        assert_eq!(response.item.local_date, "2026-08-24");
        assert_eq!(response.item.id, item_id, "same agenda row");

        // Today is exdated.
        let routine = pollster::block_on(repos.routines.get_by_id("rt-1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&routine.exdates).unwrap(),
            vec!["2026-08-23".to_string()]
        );

        // Tomorrow's GET: the moved row IS the seeded-looking day — the
        // ensure on 2026-08-24 finds the moved occurrence under the UNIQUE
        // key and the moved membership row under the item key, so the seed
        // creates no second occurrence and no second item.
        let tomorrow = get(&repos, "u-1", "2026-08-24").unwrap();
        assert_eq!(tomorrow.items.len(), 1, "no duplicate from the seed");
        assert_eq!(tomorrow.items[0].id, item_id, "same agenda row");
        assert_eq!(
            tomorrow.items[0].occurrence.as_ref().unwrap().id,
            occ_id,
            "the moved row, not a fresh seed"
        );
        assert_eq!(repos.occurrences.stored.lock().unwrap().len(), 1, "one occurrence total");
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), 1, "one item total");

        // Today's GET is empty (exdated).
        let today_after = get(&repos, "u-1", "2026-08-23").unwrap();
        assert_eq!(today_after.items.len(), 0);
    }

    #[test]
    fn reschedule_refuses_target_date_already_holding_the_routine() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        ));
        // Two dates already seeded by plain GETs — each has its own
        // occurrence of the same routine.
        let day1 = get(&repos, "u-1", "2026-08-23").unwrap();
        let day2 = get(&repos, "u-1", "2026-08-24").unwrap();
        assert_eq!(day2.items.len(), 1);
        let day1_id = day1.items[0].id.clone();

        let err = reschedule(&repos, "u-1", &day1_id, "2026-08-24").unwrap_err();
        assert!(
            matches!(err, AgendaError::Invalid(ref m) if m == "routine already has an occurrence on that date"),
            "{err:?}"
        );
        // Nothing moved, nothing exdated.
        let stored = pollster::block_on(repos.agenda.get_by_id(&day1_id))
            .unwrap()
            .unwrap();
        assert_eq!(stored.local_date, "2026-08-23");
        let routine = pollster::block_on(repos.routines.get_by_id("rt-1"))
            .unwrap()
            .unwrap();
        assert_eq!(routine.exdates, "[]", "no exdate on a refused move");
        let occurrence = pollster::block_on(repos.occurrences.get_by_routine_and_date("rt-1", "2026-08-23"))
            .unwrap()
            .unwrap();
        assert_eq!(occurrence.local_date, "2026-08-23", "occurrence untouched");
    }

    #[test]
    fn reschedule_statuses_in_progress_done_are_400_skipped_becomes_pending() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        ));
        // Seed once so the agenda rows exist, then rewrite the occurrences'
        // statuses to drive the matrix.
        let agenda = get(&repos, "u-1", "2026-08-23").unwrap();
        let item_id = agenda.items[0].id.clone();
        let occ_id = agenda.items[0].occurrence.as_ref().unwrap().id.clone();
        let set_status = |status: &str| {
            pollster::block_on(repos.occurrences.set_status(&occ_id, status)).unwrap();
        };

        set_status(OCCURRENCE_STATUS_IN_PROGRESS);
        let err = reschedule(&repos, "u-1", &item_id, "2026-08-24").unwrap_err();
        assert!(
            matches!(err, AgendaError::Invalid(ref m) if m == "cannot reschedule an in_progress occurrence"),
            "{err:?}"
        );

        set_status(OCCURRENCE_STATUS_DONE);
        let err = reschedule(&repos, "u-1", &item_id, "2026-08-24").unwrap_err();
        assert!(
            matches!(err, AgendaError::Invalid(ref m) if m == "cannot reschedule a done occurrence"),
            "{err:?}"
        );

        // A skipped occurrence reschedules as a DEFERRED day: pending again.
        set_status(OCCURRENCE_STATUS_SKIPPED);
        // Prove the travel rule too: a title override and (weirdly) stored
        // google ids ride along — nothing is cleared.
        pollster::block_on(repos.occurrences.update_title(&occ_id, Some("Fajr Qadha"))).unwrap();
        pollster::block_on(repos.occurrences.set_event_ids(&occ_id, "cal-1", "g-1")).unwrap();
        let response = reschedule(&repos, "u-1", &item_id, "2026-08-24").unwrap();
        assert_eq!(response.item.local_date, "2026-08-24");
        let occurrence = response.item.occurrence.as_ref().unwrap();
        assert_eq!(occurrence.status, OCCURRENCE_STATUS_PENDING, "skipped → pending");
        assert_eq!(occurrence.resolved_title, "Fajr Qadha", "override travels");
        assert_eq!(
            occurrence.calendar_id.as_deref(),
            Some("cal-1"),
            "stored google ids are never cleared"
        );
        assert_eq!(occurrence.google_event_id.as_deref(), Some("g-1"));
        // The routine got the exdate.
        let routine = pollster::block_on(repos.routines.get_by_id("rt-1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&routine.exdates).unwrap(),
            vec!["2026-08-23".to_string()]
        );
    }

    #[test]
    fn reschedule_same_date_is_200_noop_unchanged() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        ));
        let agenda = get(&repos, "u-1", "2026-08-23").unwrap();
        let item_id = agenda.items[0].id.clone();
        let occ_id = agenda.items[0].occurrence.as_ref().unwrap().id.clone();

        let response = reschedule(&repos, "u-1", &item_id, "2026-08-23").unwrap();
        assert_eq!(response.item.id, item_id, "same item");
        assert_eq!(response.item.sort_order, 0, "rank untouched");
        assert_eq!(response.item.occurrence.as_ref().unwrap().id, occ_id);

        // Nothing written: no exdate, no date/status/updated_at change.
        let routine = pollster::block_on(repos.routines.get_by_id("rt-1"))
            .unwrap()
            .unwrap();
        assert_eq!(routine.exdates, "[]", "same-date no-op never exdates");
        let occurrence = pollster::block_on(repos.occurrences.get_by_id(&occ_id))
            .unwrap()
            .unwrap();
        assert_eq!(occurrence.local_date, "2026-08-23");
        assert_eq!(occurrence.updated_at, "2026-08-23T00:00:00Z", "row untouched");
        let stored = pollster::block_on(repos.agenda.get_by_id(&item_id))
            .unwrap()
            .unwrap();
        assert_eq!(stored.updated_at, "2026-08-23T00:00:00Z", "item untouched");
    }

    #[test]
    fn reschedule_task_moves_the_slot_only() {
        let repos = repos();
        repos.tasks.push(task_row("t-1", "u-1", "Review | Work", "OPEN"));
        let added = add(&repos, "u-1", &agenda_input("task", "t-1", "2026-08-23")).unwrap();
        let item_id = added.item.id.clone();

        let response = reschedule(&repos, "u-1", &item_id, "2026-08-24").unwrap();
        assert_eq!(response.item.id, item_id, "same membership row, relocated");
        assert_eq!(response.item.local_date, "2026-08-24");
        assert_eq!(response.item.sort_order, 0, "empty target pile appends at 0");
        assert_eq!(response.item.ref_id, "t-1");

        // One slot on tomorrow, none on today.
        let tomorrow = get(&repos, "u-1", "2026-08-24").unwrap();
        assert_eq!(tomorrow.items.len(), 1);
        assert_eq!(tomorrow.items[0].kind, AGENDA_KIND_TASK);
        let today = get(&repos, "u-1", "2026-08-23").unwrap();
        assert_eq!(today.items.len(), 0, "tasks never re-seed, so today stays empty");
        // The task itself is untouched — status unchanged, no task_logs row
        // (the fake task repo has no logs at all).
        let stored = pollster::block_on(repos.tasks.get_by_id("t-1")).unwrap().unwrap();
        assert_eq!(stored.status, "OPEN", "task status unchanged");
    }

    #[test]
    fn reschedule_task_already_on_target_unpins_source_and_returns_existing() {
        let repos = repos();
        repos.tasks.push(task_row("t-1", "u-1", "Review | Work", "OPEN"));
        let on_today = add(&repos, "u-1", &agenda_input("task", "t-1", "2026-08-23")).unwrap();
        let on_tomorrow = add(&repos, "u-1", &agenda_input("task", "t-1", "2026-08-24")).unwrap();
        let today_id = on_today.item.id.clone();
        let tomorrow_id = on_tomorrow.item.id.clone();

        // Move today's slot to tomorrow, where the task already sits.
        let response = reschedule(&repos, "u-1", &today_id, "2026-08-24").unwrap();
        assert_eq!(
            response.item.id, tomorrow_id,
            "the EXISTING target item is returned — never a duplicate"
        );
        assert_eq!(response.item.local_date, "2026-08-24");

        // Source unpinned (hard-deleted), target pile has exactly one slot.
        assert!(
            pollster::block_on(repos.agenda.get_by_id(&today_id))
                .unwrap()
                .is_none(),
            "source slot hard-deleted"
        );
        let tomorrow = get(&repos, "u-1", "2026-08-24").unwrap();
        assert_eq!(tomorrow.items.len(), 1, "no duplicate slot");
        assert_eq!(tomorrow.items[0].id, tomorrow_id);
        let today = get(&repos, "u-1", "2026-08-23").unwrap();
        assert_eq!(today.items.len(), 0, "source day emptied");
    }

    #[test]
    fn reschedule_missing_other_user_and_soft_deleted_backings_are_404() {
        let repos = repos();
        repos.tasks.push(task_row("t-1", "u-1", "Review | Work", "OPEN"));
        add(&repos, "u-1", &agenda_input("task", "t-1", "2026-08-23")).unwrap();

        // Missing item → 404.
        let err = reschedule(&repos, "u-1", "nope", "2026-08-24").unwrap_err();
        assert!(matches!(err, AgendaError::NotFound), "{err:?}");

        // Another user's item → 404 (never leak existence).
        repos.agenda.stored.lock().unwrap().push(AgendaItem {
            id: "ai-x".to_string(),
            user_id: "u-2".to_string(),
            local_date: "2026-08-23".to_string(),
            kind: AGENDA_KIND_TASK.to_string(),
            ref_id: "t-x".to_string(),
            sort_order: 0,
            created_at: "2026-08-23T00:00:00Z".to_string(),
            updated_at: "2026-08-23T00:00:00Z".to_string(),
        });
        let err = reschedule(&repos, "u-1", "ai-x", "2026-08-24").unwrap_err();
        assert!(matches!(err, AgendaError::NotFound), "{err:?}");

        // A task-kind slot whose task is soft-deleted → 404.
        let mut deleted = task_row("t-gone", "u-1", "Gone | Work", "OPEN");
        deleted.deleted_at = Some("2026-08-20T00:00:00Z".to_string());
        repos.tasks.push(deleted);
        repos.agenda.stored.lock().unwrap().push(AgendaItem {
            id: "ai-gone-task".to_string(),
            user_id: "u-1".to_string(),
            local_date: "2026-08-23".to_string(),
            kind: AGENDA_KIND_TASK.to_string(),
            ref_id: "t-gone".to_string(),
            sort_order: 0,
            created_at: "2026-08-23T00:00:00Z".to_string(),
            updated_at: "2026-08-23T00:00:00Z".to_string(),
        });
        let err = reschedule(&repos, "u-1", "ai-gone-task", "2026-08-24").unwrap_err();
        assert!(matches!(err, AgendaError::NotFound), "{err:?}");

        // An occurrence-kind item whose routine is soft-deleted → 404.
        let mut routine = FakeRoutineRepo::row(
            "rt-gone",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        );
        routine.deleted_at = Some("2026-08-20T00:00:00Z".to_string());
        repos.routines.stored.lock().unwrap().push(routine);
        repos
            .occurrences
            .stored
            .lock()
            .unwrap()
            .push(FakeOccurrenceRepo::row("occ-gone", "rt-gone", "u-1", "2026-08-23", "pending"));
        repos.agenda.stored.lock().unwrap().push(AgendaItem {
            id: "ai-gone".to_string(),
            user_id: "u-1".to_string(),
            local_date: "2026-08-23".to_string(),
            kind: AGENDA_KIND_OCCURRENCE.to_string(),
            ref_id: "occ-gone".to_string(),
            sort_order: 0,
            created_at: "2026-08-23T00:00:00Z".to_string(),
            updated_at: "2026-08-23T00:00:00Z".to_string(),
        });
        let err = reschedule(&repos, "u-1", "ai-gone", "2026-08-24").unwrap_err();
        assert!(matches!(err, AgendaError::NotFound), "{err:?}");
    }

    #[test]
    fn reschedule_rejects_invalid_dates() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        ));
        let agenda = get(&repos, "u-1", "2026-08-23").unwrap();
        let item_id = agenda.items[0].id.clone();
        for bad in ["", "   ", "2026-13-01", "23-08-2026", "not a date"] {
            let err = reschedule(&repos, "u-1", &item_id, bad).unwrap_err();
            assert!(
                matches!(err, AgendaError::Invalid(ref m) if m == "date must be YYYY-MM-DD"),
                "{err:?} for {bad:?}"
            );
        }
        // Nothing was written by the rejected calls.
        let routine = pollster::block_on(repos.routines.get_by_id("rt-1"))
            .unwrap()
            .unwrap();
        assert_eq!(routine.exdates, "[]");
        let stored = pollster::block_on(repos.agenda.get_by_id(&item_id))
            .unwrap()
            .unwrap();
        assert_eq!(stored.local_date, "2026-08-23");
    }

    // ──────────────────────────────────────────
    // complete / skip — the full verb matrix
    // ──────────────────────────────────────────

    #[test]
    fn complete_matrix_all_four_states() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        ));
        for (from, expected_updated_at) in [
            ("pending", "2026-08-23T02:00:00Z"),
            ("in_progress", "2026-08-23T02:00:00Z"),
            ("done", "2026-08-23T01:00:00Z"), // no-op: row untouched
            ("skipped", "2026-08-23T02:00:00Z"),
        ] {
            let id = format!("occ-{from}");
            repos.occurrences.stored.lock().unwrap().push(FakeOccurrenceRepo::row(
                &id, "rt-1", "u-1", "2026-08-23", from,
            ));
            let response = complete(&repos, "u-1", &id).unwrap();
            assert_eq!(response.occurrence.status, OCCURRENCE_STATUS_DONE, "from {from}");
            assert_eq!(response.occurrence.resolved_title, "Fajr");
            let stored = pollster::block_on(repos.occurrences.get_by_id(&id)).unwrap().unwrap();
            assert_eq!(stored.status, OCCURRENCE_STATUS_DONE);
            assert_eq!(stored.updated_at, expected_updated_at, "no-op for from {from}");
        }
    }

    #[test]
    fn skip_matrix_all_four_states() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        ));
        for (from, expected_updated_at) in [
            ("pending", "2026-08-23T02:00:00Z"),
            ("in_progress", "2026-08-23T02:00:00Z"),
            ("done", "2026-08-23T02:00:00Z"),
            ("skipped", "2026-08-23T01:00:00Z"), // no-op: row untouched
        ] {
            let id = format!("occ-{from}");
            repos.occurrences.stored.lock().unwrap().push(FakeOccurrenceRepo::row(
                &id, "rt-1", "u-1", "2026-08-23", from,
            ));
            let response = skip(&repos, "u-1", &id).unwrap();
            assert_eq!(response.occurrence.status, OCCURRENCE_STATUS_SKIPPED, "from {from}");
            let stored = pollster::block_on(repos.occurrences.get_by_id(&id)).unwrap().unwrap();
            assert_eq!(stored.status, OCCURRENCE_STATUS_SKIPPED);
            assert_eq!(stored.updated_at, expected_updated_at, "no-op for from {from}");
        }
    }

    // ──────────────────────────────────────────
    // PATCH /api/occurrences/:id — title override
    // ──────────────────────────────────────────

    #[test]
    fn patch_title_overrides_and_empty_clears_back_to_inheritance() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        ));
        repos
            .occurrences
            .stored
            .lock()
            .unwrap()
            .push(FakeOccurrenceRepo::row("occ-1", "rt-1", "u-1", "2026-08-23", "pending"));

        // Override (trimmed on store).
        let response = patch(
            &repos,
            "u-1",
            "occ-1",
            &UpdateOccurrence {
                title: Some("  Fajr Qadha  ".to_string()),
            },
        )
        .unwrap();
        assert_eq!(response.occurrence.title.as_deref(), Some("Fajr Qadha"));
        assert_eq!(response.occurrence.resolved_title, "Fajr Qadha");
        let stored = pollster::block_on(repos.occurrences.get_by_id("occ-1")).unwrap().unwrap();
        assert_eq!(stored.title.as_deref(), Some("Fajr Qadha"));

        // `""` clears the override → inherit the routine title again.
        let cleared = patch(
            &repos,
            "u-1",
            "occ-1",
            &UpdateOccurrence {
                title: Some(String::new()),
            },
        )
        .unwrap();
        assert_eq!(cleared.occurrence.title, None, "override stored as NULL");
        assert_eq!(cleared.occurrence.resolved_title, "Fajr");
        let stored = pollster::block_on(repos.occurrences.get_by_id("occ-1")).unwrap().unwrap();
        assert_eq!(stored.title, None);

        // Whitespace-only also clears.
        let whitespace = patch(
            &repos,
            "u-1",
            "occ-1",
            &UpdateOccurrence {
                title: Some("   ".to_string()),
            },
        )
        .unwrap();
        assert_eq!(whitespace.occurrence.title, None);
        assert_eq!(whitespace.occurrence.resolved_title, "Fajr");
    }

    #[test]
    fn patch_empty_body_is_400_and_missing_occurrence_is_404() {
        let repos = repos();
        let err = patch(&repos, "u-1", "occ-1", &UpdateOccurrence { title: None }).unwrap_err();
        assert!(
            matches!(err, AgendaError::Invalid(ref m) if m == "nothing to update"),
            "{err:?}"
        );
        let err = patch(
            &repos,
            "u-1",
            "occ-missing",
            &UpdateOccurrence {
                title: Some("X".to_string()),
            },
        )
        .unwrap_err();
        assert!(matches!(err, AgendaError::NotFound), "{err:?}");
    }

    // ──────────────────────────────────────────
    // Ownership: another user's occurrence is 404 everywhere
    // ──────────────────────────────────────────

    #[test]
    fn other_users_occurrence_is_404_on_complete_skip_and_patch() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-2",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        ));
        repos
            .occurrences
            .stored
            .lock()
            .unwrap()
            .push(FakeOccurrenceRepo::row("occ-1", "rt-1", "u-2", "2026-08-23", "pending"));

        assert!(matches!(complete(&repos, "u-1", "occ-1").unwrap_err(), AgendaError::NotFound));
        assert!(matches!(skip(&repos, "u-1", "occ-1").unwrap_err(), AgendaError::NotFound));
        assert!(matches!(
            patch(
                &repos,
                "u-1",
                "occ-1",
                &UpdateOccurrence {
                    title: Some("X".to_string()),
                },
            )
            .unwrap_err(),
            AgendaError::NotFound
        ));
        // The stored row was not touched.
        let stored = pollster::block_on(repos.occurrences.get_by_id("occ-1")).unwrap().unwrap();
        assert_eq!(stored.status, "pending");
        assert_eq!(stored.title, None);
    }

    #[test]
    fn occurrence_verbs_404_when_the_routine_is_soft_deleted() {
        let repos = repos();
        let mut routine = FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        );
        routine.deleted_at = Some("2026-08-20T00:00:00Z".to_string());
        repos.routines.stored.lock().unwrap().push(routine);
        repos
            .occurrences
            .stored
            .lock()
            .unwrap()
            .push(FakeOccurrenceRepo::row("occ-1", "rt-1", "u-1", "2026-08-23", "pending"));

        assert!(matches!(complete(&repos, "u-1", "occ-1").unwrap_err(), AgendaError::NotFound));
        assert!(matches!(skip(&repos, "u-1", "occ-1").unwrap_err(), AgendaError::NotFound));
        assert!(matches!(
            patch(
                &repos,
                "u-1",
                "occ-1",
                &UpdateOccurrence {
                    title: Some("X".to_string()),
                },
            )
            .unwrap_err(),
            AgendaError::NotFound
        ));
        let stored = pollster::block_on(repos.occurrences.get_by_id("occ-1")).unwrap().unwrap();
        assert_eq!(stored.status, "pending", "verb wrote nothing");
    }

    #[test]
    fn agenda_get_embeds_full_task_view_with_focus_flag() {
        let repos = repos();
        repos.tasks.push(task_row("t-1", "u-1", "Review | Work", "IN_PROGRESS"));
        let item = add(&repos, "u-1", &agenda_input("task", "t-1", "2026-08-23")).unwrap();
        let focused = pollster::block_on(get_agenda(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &repos.agenda,
            &repos.tasks,
            "u-1",
            "2026-08-23",
            Some("t-1"),
        ))
        .unwrap();
        let task = focused.items[0].task.as_ref().expect("task embedded");
        assert!(task.focused, "focused painted from the caller's pointer");
        // The seeded Work pattern has a hole: "Review | Work" → "Review".
        assert_eq!(task.display_title, "Review");
        assert_eq!(task.duration_minutes, 15);
        assert_eq!(task.priority, "medium");
        assert_eq!(task.difficulty, "easy");
        assert_eq!(task.status, "IN_PROGRESS");
        // The same read without the pointer paints false.
        let unfocused = get(&repos, "u-1", "2026-08-23").unwrap();
        assert!(!unfocused.items[0].task.as_ref().unwrap().focused);
        // The add response itself carries the same full shape.
        assert_eq!(item.item.task.as_ref().unwrap().id, "t-1");
    }

    // ──────────────────────────────────────────
    // POST /api/occurrences/:id/start (slice 6)
    // ──────────────────────────────────────────

    #[test]
    fn start_pending_today_creates_one_shot_log_and_stores_ids() {
        let repos = repos();
        seed_occurrence(&repos, OCCURRENCE_STATUS_PENDING);
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events",
            200,
            &created_occurrence_json("rt-1", "occ-1", "2026-08-23T10:00:00Z", "2026-08-23T10:15:00Z"),
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let response = pollster::block_on(start_occurrence(
            &http,
            &calendars,
            &events,
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &access(),
            "u-1",
            "occ-1",
            NOW_UNIX,
        ))
        .unwrap();

        // Status flipped and the ids were stored on the occurrence.
        assert_eq!(response.occurrence.status, OCCURRENCE_STATUS_IN_PROGRESS);
        assert_eq!(
            response.occurrence.calendar_id.as_deref(),
            Some("cal-primary@example.com")
        );
        assert_eq!(response.occurrence.google_event_id.as_deref(), Some("g-1"));
        let stored = pollster::block_on(repos.occurrences.get_by_id("occ-1"))
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, OCCURRENCE_STATUS_IN_PROGRESS);
        assert_eq!(stored.calendar_id.as_deref(), Some("cal-primary@example.com"));
        assert_eq!(stored.google_event_id.as_deref(), Some("g-1"));

        // The insert payload: resolved title (the override), the minute-grid
        // window, the two occurrence carriers, NO task_id, NO focus, and —
        // the hard rule — no recurrence/RRULE anywhere.
        let (url, body) = http.posts.lock().unwrap().first().unwrap().clone();
        assert!(url.contains("primary%40example.com"), "{url}");
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["summary"], "Fajr Qadha", "resolved title at start");
        assert_eq!(body["start"]["dateTime"], "2026-08-23T10:00:00Z");
        assert_eq!(
            body["end"]["dateTime"], "2026-08-23T10:15:00Z",
            "T … T + START_EVENT_MINUTES"
        );
        assert!(body.get("recurrence").is_none(), "never an RRULE: {body}");
        let shared = &body["extendedProperties"]["shared"];
        assert_eq!(shared["sanctuary_routine_id"], "rt-1");
        assert_eq!(shared["sanctuary_occurrence_id"], "occ-1");
        assert!(
            shared.get("sanctuary_task_id").is_none(),
            "task carrier never set: {body}"
        );
        assert!(
            shared.get("sanctuary_focus").is_none(),
            "focus stays task-only: {body}"
        );
        assert!(
            shared.get("sanctuary_priority").is_none(),
            "no task snapshots: {body}"
        );
        assert_eq!(
            shared.as_object().unwrap().len(),
            2,
            "exactly the two carriers: {body}"
        );

        // The cached event row carries NO task link — the two worlds stay apart.
        let upserted = events.upserted.lock().unwrap();
        assert_eq!(upserted.len(), 1);
        assert_eq!(upserted[0].task_id, "", "calendar_events.task_id stays empty");
        assert_eq!(upserted[0].title, "Fajr Qadha", "the response echo maps the summary");
        assert_eq!(upserted[0].recurrence, "", "no recurrence on the cache row");
    }

    #[test]
    fn start_in_progress_is_200_noop_without_a_second_event() {
        let repos = repos();
        seed_occurrence(&repos, OCCURRENCE_STATUS_IN_PROGRESS);
        // No routes: any Google call would make the fake panic — the no-op
        // must not touch Google at all.
        let http = FakeHttp::new(vec![]);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let response = pollster::block_on(start_occurrence(
            &http,
            &calendars,
            &events,
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &access(),
            "u-1",
            "occ-1",
            NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(response.occurrence.status, OCCURRENCE_STATUS_IN_PROGRESS);
        assert!(response.event.is_none(), "no second event on the no-op");
        assert!(http.posts.lock().unwrap().is_empty(), "no Google writes");
        assert!(events.upserted.lock().unwrap().is_empty());
        assert_eq!(
            pollster::block_on(repos.occurrences.get_by_id("occ-1"))
                .unwrap()
                .unwrap()
                .status,
            OCCURRENCE_STATUS_IN_PROGRESS,
            "row untouched"
        );
    }

    #[test]
    fn start_done_or_skipped_is_invalid() {
        let repos = repos();
        for status in [OCCURRENCE_STATUS_DONE, OCCURRENCE_STATUS_SKIPPED] {
            seed_occurrence(&repos, status);
            let err = start(&repos, "u-1", "occ-1").unwrap_err();
            assert!(
                matches!(err, AgendaError::Invalid(ref m) if m.contains("cannot start")),
                "{err:?} from {status}"
            );
        }
    }

    #[test]
    fn start_wrong_date_is_invalid() {
        let repos = repos();
        // Tomorrow (2026-08-24) — start is today-only.
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        ));
        repos
            .occurrences
            .stored
            .lock()
            .unwrap()
            .push(FakeOccurrenceRepo::row("occ-1", "rt-1", "u-1", "2026-08-24", "pending"));

        let err = start(&repos, "u-1", "occ-1").unwrap_err();
        assert!(
            matches!(err, AgendaError::Invalid(ref m) if m == "occurrence can only be started on its local date"),
            "{err:?}"
        );
    }

    #[test]
    fn start_missing_other_user_or_soft_deleted_routine_is_not_found() {
        let repos = repos();
        // Missing occurrence → 404.
        assert!(matches!(start(&repos, "u-1", "nope").unwrap_err(), AgendaError::NotFound));

        // Another user's occurrence → 404 (never leak existence).
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-2",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        ));
        repos
            .occurrences
            .stored
            .lock()
            .unwrap()
            .push(FakeOccurrenceRepo::row("occ-2", "rt-1", "u-2", "2026-08-23", "pending"));
        assert!(matches!(start(&repos, "u-1", "occ-2").unwrap_err(), AgendaError::NotFound));

        // Soft-deleted routine → its occurrence is 404.
        let mut routine = FakeRoutineRepo::row(
            "rt-3",
            "u-1",
            "Fajr",
            0,
            "2026-01-01T05:30:00",
            "FREQ=DAILY",
            "[]",
        );
        routine.deleted_at = Some("2026-08-20T00:00:00Z".to_string());
        repos.routines.stored.lock().unwrap().push(routine);
        repos
            .occurrences
            .stored
            .lock()
            .unwrap()
            .push(FakeOccurrenceRepo::row("occ-3", "rt-3", "u-1", "2026-08-23", "pending"));
        assert!(matches!(start(&repos, "u-1", "occ-3").unwrap_err(), AgendaError::NotFound));
    }

    #[test]
    fn start_falls_back_to_primary_when_the_named_calendar_is_read_only() {
        let repos = repos();
        seed_occurrence(&repos, OCCURRENCE_STATUS_PENDING);
        // "Fajr Qadha" classifies untracked, so the pick never names a
        // calendar; the fixture instead proves the read-only named calendar →
        // primary fallback (the same locked rule as start_task).
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events",
            200,
            &created_occurrence_json("rt-1", "occ-1", "2026-08-23T10:00:00Z", "2026-08-23T10:15:00Z"),
        )]);
        let calendars = FakeCalendarRepo::with(vec![
            GoogleCalendar {
                access_role: "reader".to_string(),
                ..calendar("named@example.com", false)
            },
            calendar("primary@example.com", true),
        ]);
        let events = FakeEventRepo::new();

        let response = pollster::block_on(start_occurrence(
            &http,
            &calendars,
            &events,
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &access(),
            "u-1",
            "occ-1",
            NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(
            response.occurrence.calendar_id.as_deref(),
            Some("cal-primary@example.com"),
            "read-only named calendar never receives the chip"
        );
    }

    #[test]
    fn start_without_any_writable_calendar_is_invalid() {
        let repos = repos();
        seed_occurrence(&repos, OCCURRENCE_STATUS_PENDING);
        let http = FakeHttp::new(vec![]);
        let calendars = FakeCalendarRepo::with(vec![]);
        let events = FakeEventRepo::new();

        let err = pollster::block_on(start_occurrence(
            &http,
            &calendars,
            &events,
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &access(),
            "u-1",
            "occ-1",
            NOW_UNIX,
        ))
        .unwrap_err();
        assert!(
            matches!(err, AgendaError::Invalid(ref m) if m == "no writable calendar"),
            "{err:?}"
        );
    }

    // ──────────────────────────────────────────
    // complete / skip while in_progress — close the chip (slice 6)
    // ──────────────────────────────────────────

    /// Seeds an in_progress occurrence WITH stored ids plus the cached event
    /// its chip resolves to (`cal-primary@example.com` / `g-1`, 10:00 →
    /// 10:15). `NOW_UNIX` (10:00:00) snapped == start, so the exit's invert
    /// guard lands the end at `start + 60s` — the same rule as task exits.
    fn seed_running_chip(repos: &Repos) {
        let mut occurrence =
            FakeOccurrenceRepo::row("occ-1", "rt-1", "u-1", "2026-08-23", "in_progress");
        occurrence.calendar_id = Some("cal-primary@example.com".to_string());
        occurrence.google_event_id = Some("g-1".to_string());
        repos.occurrences.stored.lock().unwrap().push(occurrence);
        repos
            .routines
            .stored
            .lock()
            .unwrap()
            .push(FakeRoutineRepo::row(
                "rt-1",
                "u-1",
                "Fajr",
                0,
                "2026-01-01T05:30:00",
                "FREQ=DAILY",
                "[]",
            ));
    }

    #[test]
    fn complete_while_in_progress_patches_end_then_flips_status() {
        let repos = repos();
        seed_running_chip(&repos);
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events/g-1",
            200,
            &patched_occurrence_json("rt-1", "occ-1", "2026-08-23T10:00:00Z", "2026-08-23T10:01:00Z"),
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        // The cached row supplies `start` for the invert guard (10:00:00).
        pollster::block_on(events.upsert(
            NewCalendarEvent {
                calendar_id: "cal-primary@example.com".to_string(),
                google_event_id: "g-1".to_string(),
                google_etag: String::new(),
                google_updated_at: String::new(),
                last_synced_at: "2026-08-23T00:00:00Z".to_string(),
                title: "Fajr".to_string(),
                description: String::new(),
                start_time: "2026-08-23T10:00:00Z".to_string(),
                end_time: "2026-08-23T10:15:00Z".to_string(),
                recurrence: String::new(),
                task_id: String::new(),
            },
            "2026-08-23T00:00:00Z",
        ))
        .unwrap();

        let response = pollster::block_on(complete_occurrence(
            Some(&http),
            Some(&calendars),
            Some(&events),
            Some(&access()),
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            "u-1",
            "occ-1",
            NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(response.occurrence.status, OCCURRENCE_STATUS_DONE);
        let stored = pollster::block_on(repos.occurrences.get_by_id("occ-1"))
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, OCCURRENCE_STATUS_DONE, "status flipped after the close");

        // The PATCH closed the chip: snapped now == start → start + 60s.
        let patches = http.patches.lock().unwrap();
        assert_eq!(patches.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&patches[0].1).unwrap();
        assert_eq!(body["end"]["dateTime"], "2026-08-23T10:01:00Z");
        assert!(patches[0].0.contains("events/g-1"), "{}", patches[0].0);
    }

    #[test]
    fn skip_while_in_progress_patches_end_then_flips_status() {
        let repos = repos();
        seed_running_chip(&repos);
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events/g-1",
            200,
            &patched_occurrence_json("rt-1", "occ-1", "2026-08-23T10:00:00Z", "2026-08-23T10:01:00Z"),
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        pollster::block_on(events.upsert(
            NewCalendarEvent {
                calendar_id: "cal-primary@example.com".to_string(),
                google_event_id: "g-1".to_string(),
                google_etag: String::new(),
                google_updated_at: String::new(),
                last_synced_at: "2026-08-23T00:00:00Z".to_string(),
                title: "Fajr".to_string(),
                description: String::new(),
                start_time: "2026-08-23T10:00:00Z".to_string(),
                end_time: "2026-08-23T10:15:00Z".to_string(),
                recurrence: String::new(),
                task_id: String::new(),
            },
            "2026-08-23T00:00:00Z",
        ))
        .unwrap();

        let response = pollster::block_on(skip_occurrence(
            Some(&http),
            Some(&calendars),
            Some(&events),
            Some(&access()),
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            "u-1",
            "occ-1",
            NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(response.occurrence.status, OCCURRENCE_STATUS_SKIPPED);
        assert_eq!(
            pollster::block_on(repos.occurrences.get_by_id("occ-1"))
                .unwrap()
                .unwrap()
                .status,
            OCCURRENCE_STATUS_SKIPPED
        );
        assert_eq!(http.patches.lock().unwrap().len(), 1, "chip closed on skip");
    }

    #[test]
    fn complete_while_in_progress_without_google_is_session_only() {
        let repos = repos();
        seed_running_chip(&repos);
        // No routes: any Google call would panic — the session-only flip must
        // not touch Google (the worker decides the gate; here it is off).
        let response = complete(&repos, "u-1", "occ-1").unwrap();
        assert_eq!(response.occurrence.status, OCCURRENCE_STATUS_DONE);
        assert_eq!(
            pollster::block_on(repos.occurrences.get_by_id("occ-1"))
                .unwrap()
                .unwrap()
                .status,
            OCCURRENCE_STATUS_DONE
        );
    }

    #[test]
    fn complete_while_in_progress_404_on_google_still_flips_status() {
        let repos = repos();
        seed_running_chip(&repos);
        // The chip is gone on Google's side: the close proceeds, the flip
        // still happens — never fail a complete on a ghost event.
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events/g-1",
            404,
            r#"{"error":"not found"}"#,
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let response = pollster::block_on(complete_occurrence(
            Some(&http),
            Some(&calendars),
            Some(&events),
            Some(&access()),
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            "u-1",
            "occ-1",
            NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(response.occurrence.status, OCCURRENCE_STATUS_DONE);
    }

    // ──────────────────────────────────────────
    // PATCH title with a chip — summary follows (slice 6)
    // ──────────────────────────────────────────

    #[test]
    fn patch_title_with_chip_patches_the_google_summary() {
        let repos = repos();
        seed_running_chip(&repos);
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events/g-1",
            200,
            &patched_occurrence_json("rt-1", "occ-1", "2026-08-23T10:00:00Z", "2026-08-23T10:15:00Z"),
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let response = pollster::block_on(patch_occurrence(
            Some(&http),
            Some(&calendars),
            Some(&events),
            Some(&access()),
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            "u-1",
            "occ-1",
            &UpdateOccurrence {
                title: Some("Fajr Chips".to_string()),
            },
            NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(response.occurrence.resolved_title, "Fajr Chips");
        let patches = http.patches.lock().unwrap();
        assert_eq!(patches.len(), 1, "the summary PATCH happened");
        let body: serde_json::Value = serde_json::from_str(&patches[0].1).unwrap();
        assert_eq!(body["summary"], "Fajr Chips", "resolved title follows the rename");
    }

    #[test]
    fn patch_title_with_chip_404_proceeds_without_failing_the_override() {
        let repos = repos();
        seed_running_chip(&repos);
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events/g-1",
            404,
            r#"{"error":"not found"}"#,
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let response = pollster::block_on(patch_occurrence(
            Some(&http),
            Some(&calendars),
            Some(&events),
            Some(&access()),
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            "u-1",
            "occ-1",
            &UpdateOccurrence {
                title: Some("Fajr Chips".to_string()),
            },
            NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(
            response.occurrence.resolved_title, "Fajr Chips",
            "the override writes even when the chip is gone"
        );
    }

    #[test]
    fn patch_title_without_a_chip_never_touches_google() {
        let repos = repos();
        seed_occurrence(&repos, OCCURRENCE_STATUS_PENDING);
        // No routes: any Google call would make the fake panic.
        let http = FakeHttp::new(vec![]);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let response = pollster::block_on(patch_occurrence(
            Some(&http),
            Some(&calendars),
            Some(&events),
            Some(&access()),
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            "u-1",
            "occ-1",
            &UpdateOccurrence {
                title: Some("Fajr Chips".to_string()),
            },
            NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(response.occurrence.resolved_title, "Fajr Chips");
        assert!(http.patches.lock().unwrap().is_empty(), "no chip → no Google");
    }

    #[test]
    fn patch_unchanged_resolved_title_does_not_patch_the_summary() {
        let repos = repos();
        seed_running_chip(&repos);
        // Give the chip-bearing occurrence the override we are about to write
        // — the resolved title does not change, so no Google PATCH.
        {
            let mut stored = repos.occurrences.stored.lock().unwrap();
            let row = stored.iter_mut().find(|row| row.id == "occ-1").unwrap();
            row.title = Some("Fajr Qadha".to_string());
        }
        let http = FakeHttp::new(vec![]);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let response = pollster::block_on(patch_occurrence(
            Some(&http),
            Some(&calendars),
            Some(&events),
            Some(&access()),
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            "u-1",
            "occ-1",
            &UpdateOccurrence {
                title: Some("Fajr Qadha".to_string()),
            },
            NOW_UNIX,
        ))
        .unwrap();

        assert_eq!(response.occurrence.resolved_title, "Fajr Qadha");
        assert!(http.patches.lock().unwrap().is_empty(), "same title → no summary PATCH");
    }

    // ──────────────────────────────────────────
    // Elongate cron (slice 6): grow in_progress occurrence events
    // ──────────────────────────────────────────

    /// `2026-08-23T10:12:55Z` — +5 min slack ceils to 10:20:00Z on the grid.
    fn elongate_now() -> i64 {
        rfc3339_to_unix_secs("2026-08-23T10:12:55Z").unwrap()
    }

    /// Runs the occurrence elongate over the seeded repos. `events` may
    /// pre-seed the cached chip row; the token repo must cover `user_id`.
    fn elongate(
        http: &FakeHttp,
        calendars: &FakeCalendarRepo,
        events: &FakeEventRepo,
        occurrences: &FakeOccurrenceRepo,
    ) -> ElongateReport {
        pollster::block_on(run_elongate_occurrences(
            http,
            calendars,
            events,
            occurrences,
            &FakeTokenRepo::with(vec![fresh_token("u-1", "at-1")]),
            &oauth_config(),
            elongate_now(),
        ))
    }

    #[test]
    fn elongate_grows_an_in_progress_occurrence_event() {
        let repos = repos();
        seed_running_chip(&repos);
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events/g-1",
            200,
            &patched_occurrence_json("rt-1", "occ-1", "2026-08-23T10:00:00Z", "2026-08-23T10:20:00Z"),
        )]);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        pollster::block_on(events.upsert(
            NewCalendarEvent {
                calendar_id: "cal-primary@example.com".to_string(),
                google_event_id: "g-1".to_string(),
                google_etag: String::new(),
                google_updated_at: String::new(),
                last_synced_at: "2026-08-23T00:00:00Z".to_string(),
                title: "Fajr".to_string(),
                description: String::new(),
                start_time: "2026-08-23T10:00:00Z".to_string(),
                // Current end is before the 10:20 target → it must grow.
                end_time: "2026-08-23T10:15:00Z".to_string(),
                recurrence: String::new(),
                task_id: String::new(),
            },
            "2026-08-23T00:00:00Z",
        ))
        .unwrap();

        let report = elongate(&http, &calendars, &events, &repos.occurrences);

        assert_eq!(report.occurrences_elongated, 1);
        assert_eq!(report.elongated, 0, "task counter untouched");
        assert!(report.errors.is_empty());
        let patches = http.patches.lock().unwrap();
        assert_eq!(patches.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&patches[0].1).unwrap();
        assert_eq!(body["end"]["dateTime"], "2026-08-23T10:20:00Z", "ceil-5min(now+5min)");
        // Status is never touched by the cron.
        let stored = pollster::block_on(repos.occurrences.get_by_id("occ-1"))
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, OCCURRENCE_STATUS_IN_PROGRESS);
    }

    #[test]
    fn elongate_skips_when_the_end_already_covers_the_target() {
        let repos = repos();
        seed_running_chip(&repos);
        // No routes: a PATCH here would panic — the never-shrink rule must
        // skip when the current end (10:30) is already past the 10:20 target.
        let http = FakeHttp::new(vec![]);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();
        pollster::block_on(events.upsert(
            NewCalendarEvent {
                calendar_id: "cal-primary@example.com".to_string(),
                google_event_id: "g-1".to_string(),
                google_etag: String::new(),
                google_updated_at: String::new(),
                last_synced_at: "2026-08-23T00:00:00Z".to_string(),
                title: "Fajr".to_string(),
                description: String::new(),
                start_time: "2026-08-23T10:00:00Z".to_string(),
                end_time: "2026-08-23T10:30:00Z".to_string(),
                recurrence: String::new(),
                task_id: String::new(),
            },
            "2026-08-23T00:00:00Z",
        ))
        .unwrap();

        let report = elongate(&http, &calendars, &events, &repos.occurrences);

        assert_eq!(report.occurrences_elongated, 0);
        assert_eq!(report.skipped, 1, "never shrink");
        assert!(http.patches.lock().unwrap().is_empty());
    }

    #[test]
    fn elongate_skips_occurrences_without_a_cached_chip() {
        let repos = repos();
        seed_running_chip(&repos);
        // The occurrence has stored ids but no cached event row — the chip is
        // gone: skip, never recreate, never flip status.
        let http = FakeHttp::new(vec![]);
        let calendars = FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]);
        let events = FakeEventRepo::new();

        let report = elongate(&http, &calendars, &events, &repos.occurrences);

        assert_eq!(report.occurrences_elongated, 0);
        assert_eq!(report.skipped, 1);
        assert_eq!(
            pollster::block_on(repos.occurrences.get_by_id("occ-1"))
                .unwrap()
                .unwrap()
                .status,
            OCCURRENCE_STATUS_IN_PROGRESS,
            "the cron never flips status"
        );
    }
}