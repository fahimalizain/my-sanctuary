//! Agenda + occurrence service (ADR 0004, slice 4): the date-scoped
//! run-of-show and its occurrence verbs.
//!
//! Pure Rust and unit-testable: persistence goes through [`OccurrenceRepo`] /
//! [`AgendaItemRepo`] / [`RoutineRepo`] / [`TaskRepo`] (faked with in-memory
//! impls in tests); the Worker layers session checks on top
//! (`apps/worker/src/agenda.rs`).
//!
//! Domain rules (ADR 0004, locked; this slice implements everything except
//! the Google writes and `/start`, which slice 6 adds):
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
//! - `DELETE` on an occurrence-kind item is refused (400 "skip is the
//!   decline"); task-kind items are hard-deleted (unpin).
//! - `PATCH /api/occurrences/:id { title }` writes the override; `""`/
//!   whitespace clears it back to inheritance (NULL). No Google PATCH this
//!   slice. Missing/other-user/soft-deleted-routine → 404.
//! - `complete`/`skip` follow the locked verb matrix except Google: pending →
//!   done/skipped, in_progress → done/skipped (no chip close yet), done →
//!   done (no-op)/skipped, skipped → done/skipped (no-op). `/start` is NOT
//!   implemented this slice.
//! - Occurrence category is classified from the **resolved** title
//!   (`override ?? routine.title`) with the same matcher as tasks; a read
//!   never 400s on classification (untracked summary when nothing matches).

use std::collections::HashMap;

use thiserror::Error;

use crate::categories::{
    classify, ensure_taxonomy, first_matching_pattern, CalendarScope, CategoryWithPatterns,
    ClassifyOutcome,
};
use crate::models::{
    AgendaItem, NewAgendaItem, NewAgendaItemInput, NewRoutineOccurrence, Routine,
    RoutineOccurrence, Task, TaskCategory, TaskCategoryPattern, UpdateOccurrence,
};
use crate::repo::{
    AgendaItemRepo, OccurrenceRepo, RepoError, RoutineRepo, TaskCategoryRepo, TaskListRepo,
    TaskRepo,
};
use crate::routines::occurrence_dates;
use crate::tasks::{TaskCategorySummary, TaskView};

/// Occurrence states (lowercase on purpose — these are NOT task statuses).
pub const OCCURRENCE_STATUS_PENDING: &str = "pending";
pub const OCCURRENCE_STATUS_IN_PROGRESS: &str = "in_progress";
pub const OCCURRENCE_STATUS_DONE: &str = "done";
pub const OCCURRENCE_STATUS_SKIPPED: &str = "skipped";

/// Agenda item kinds.
pub const AGENDA_KIND_TASK: &str = "task";
pub const AGENDA_KIND_OCCURRENCE: &str = "occurrence";

/// Errors produced by the agenda/occurrence service. The Worker maps Invalid →
/// 400, NotFound → 404, Repo → 500 (no 502 this slice — no Google writes).
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum AgendaError {
    #[error("{0}")]
    Invalid(String),
    #[error("not found")]
    NotFound,
    #[error("database error: {0}")]
    Repo(#[from] RepoError),
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
/// soft-deleted-routine → 404. **No Google PATCH this slice** (slice 6 adds
/// the `summary` write when a chip exists).
pub async fn patch_occurrence(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    user_id: &str,
    id: &str,
    updates: &UpdateOccurrence,
) -> Result<OccurrenceResponse, AgendaError> {
    if updates.title.is_none() {
        return Err(AgendaError::Invalid("nothing to update".to_string()));
    }
    // The gate: missing / other-user / soft-deleted-routine → 404. The
    // loaded row itself is not needed — the reload below carries the update.
    let _occurrence = load_occurrence_for_user(occurrence_repo, routine_repo, user_id, id).await?;
    let trimmed = updates.title.as_deref().map(str::trim);
    let stored = match trimmed {
        // `""` / whitespace-only → clear the override (inherit again).
        Some("") => None,
        Some(title) => Some(title.to_string()),
        None => None, // guarded above; keeps the match exhaustive
    };
    occurrence_repo.update_title(id, stored.as_deref()).await?;
    let taxonomy = load_taxonomy_seeded(list_repo, category_repo, user_id).await?;
    let updated = occurrence_repo.get_by_id(id).await?.ok_or(AgendaError::NotFound)?;
    let routine = routine_repo
        .get_by_id(&updated.routine_id)
        .await?
        .ok_or(AgendaError::NotFound)?;
    Ok(OccurrenceResponse {
        occurrence: occurrence_view(&updated, &routine, &taxonomy),
    })
}

// ──────────────────────────────────────────
// POST /api/occurrences/:id/complete and /skip — verb matrix (no Google)
// ──────────────────────────────────────────

/// `POST /api/occurrences/:id/complete` → `{"occurrence":…}`.
///
/// Verb matrix (Google writes deferred to slice 6):
/// `pending` → `done`, `in_progress` → `done`, `done` → 200 no-op,
/// `skipped` → `done`. Missing / other-user / soft-deleted-routine → 404.
pub async fn complete_occurrence(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    user_id: &str,
    id: &str,
) -> Result<OccurrenceResponse, AgendaError> {
    let occurrence = load_occurrence_for_user(occurrence_repo, routine_repo, user_id, id).await?;
    if occurrence.status != OCCURRENCE_STATUS_DONE {
        occurrence_repo.set_status(id, OCCURRENCE_STATUS_DONE).await?;
    }
    occurrence_response(list_repo, category_repo, routine_repo, occurrence_repo, user_id, id).await
}

/// `POST /api/occurrences/:id/skip` → `{"occurrence":…}`.
///
/// Verb matrix (Google writes deferred to slice 6):
/// `pending` → `skipped`, `in_progress` → `skipped`, `done` → `skipped`,
/// `skipped` → 200 no-op. Missing / other-user / soft-deleted-routine → 404.
pub async fn skip_occurrence(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    user_id: &str,
    id: &str,
) -> Result<OccurrenceResponse, AgendaError> {
    let occurrence = load_occurrence_for_user(occurrence_repo, routine_repo, user_id, id).await?;
    if occurrence.status != OCCURRENCE_STATUS_SKIPPED {
        occurrence_repo.set_status(id, OCCURRENCE_STATUS_SKIPPED).await?;
    }
    occurrence_response(list_repo, category_repo, routine_repo, occurrence_repo, user_id, id).await
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
        NewRoutine, NewTask, NewTaskCategory, NewTaskCategoryPattern, NewTaskList, Task, TaskList,
        UpdateRoutine, UpdateTask, UpdateTaskCategory, UpdateTaskList,
    };

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
            _id: &str,
            _updates: &UpdateRoutine,
        ) -> Result<Option<Routine>, RepoError> {
            Ok(None)
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

        async fn set_status(&self, id: &str, status: &str) -> Result<(), RepoError> {
            if let Some(row) = self.stored.lock().unwrap().iter_mut().find(|row| row.id == id) {
                row.status = status.to_string();
                row.updated_at = "2026-08-23T02:00:00Z".to_string();
            }
            Ok(())
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

    fn complete(repos: &Repos, user_id: &str, id: &str) -> Result<OccurrenceResponse, AgendaError> {
        pollster::block_on(complete_occurrence(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            user_id,
            id,
        ))
    }

    fn skip(repos: &Repos, user_id: &str, id: &str) -> Result<OccurrenceResponse, AgendaError> {
        pollster::block_on(skip_occurrence(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            user_id,
            id,
        ))
    }

    fn patch(
        repos: &Repos,
        user_id: &str,
        id: &str,
        updates: &UpdateOccurrence,
    ) -> Result<OccurrenceResponse, AgendaError> {
        pollster::block_on(patch_occurrence(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            user_id,
            id,
            updates,
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
}