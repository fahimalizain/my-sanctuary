//! Agenda + occurrence service (ADR 0004): the date-scoped run-of-show and
//! its occurrence verbs, including the slice-6 Google writes: `/start`
//! creates the one-shot log, complete/skip/pause close a running chip, the title
//! PATCH updates a chip's `summary`, and the elongate cron grows living
//! in_progress occurrence events.
//!
//! Pure Rust and unit-testable: persistence goes through [`OccurrenceRepo`] /
//! [`AgendaItemRepo`] / [`RoutineRepo`] / [`TaskRepo`] (faked with in-memory
//! impls in tests), Google HTTP through [`HttpClient`], and "now" comes from
//! the caller — never `SystemTime`. The Worker layers session checks and
//! token refresh on top (`apps/worker/src/agenda.rs`).
//!
//! Domain rules (ADR 0004 amendment, locked):
//! - `GET /api/agenda?from=&to=` is a **pure range read** of the infinite day
//!   stream (slice 1 of the agenda revamp): it ensures no occurrence and
//!   writes no agenda row, even when living routines' rules cover the window.
//!   Both bounds omitted reads the **anchor week** (`anchor_date …
//!   anchor_date + 6`, 7 inclusive days) where `anchor_date` is the oldest
//!   past date holding a pending item, else today; exactly one bound → 400;
//!   an inclusive span above 31 days → 400. The response carries `from`/`to`
//!   (resolved, never the raw query), `anchor_date`, and `earliest_date`
//!   (`MIN(local_date)` over every row, the back-scroll floor).
//!   Materializing occurrences is the occurrence fill's job, never a GET's.
//! - The response omits occurrence items whose routine is missing and task
//!   items whose task is missing/soft-deleted; the orphan membership rows
//!   stay in D1. A soft-deleted routine's `done`/`skipped` history still
//!   renders (`routine_deleted: true`, borrowing the routine's display
//!   fields) while its `pending`/`in_progress` leftovers are omitted.
//! - `POST /api/agenda/items/:id/move` reorders that item's date pile only;
//!   peers at/after the insert rank shift up (no `updated_at` bump).
//! - `POST /api/agenda/items/:id/reschedule { date, sort_order? }` relocates
//!   a slot **onto today or later** (the past is a 400) with an optional
//!   landing rank (ADR 0004 amendment): an occurrence moves its **agenda item
//!   only** — `occurrence.local_date` (the rule date) and `routines.rrule`
//!   are never touched, there is **no EXDATE anywhere**, and the fill's
//!   no-membership-anywhere rule keeps the rule date from re-attaching it.
//!   `pending | skipped` are the only reschedulable statuses (`skipped` →
//!   `pending` on the new date), `in_progress`/`done` → 400, and a target
//!   date already holding this routine is allowed (two rows of the same
//!   routine on one day are legal). A task reschedule moves the
//!   **membership slot** only — task status unchanged (`COMPLETED`/
//!   `DISCARDED` → 400 "cannot reschedule a terminal task"), no `task_logs`
//!   row (the agenda is an overlay; `task_logs` has no civil-date column) —
//!   and one row per task (0018) means the source row itself moves: there is
//!   no unpin-and-merge path. Same date → 200 no-op (same-day placement is
//!   `/move`). Session-only, never a Google write.
//! - `POST /api/agenda/days/:date/move-to-today` sweeps a **past** day's
//!   still-pending items onto civil today in one call: living
//!   `OPEN | PLANNED | IN_PROGRESS` tasks and `pending` occurrences of living
//!   routines, appended to today's pile in their original order. `done`/
//!   `skipped`/`in_progress` occurrences, terminal or soft-deleted tasks,
//!   soft-deleted-routine leftovers, orphans and unknown kinds stay put. A
//!   call on today or a future date is a 400. Session-only, no Google write.
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
//!   (or with `http`/`access` `None`) the flip is session-only. The
//!   `complete` ↔ `skip` flips stay; the **dedicated un-do path is `reopen`**
//!   (below) — there is no other way back from `done`/`skipped`.
//! - `pause` is the dedicated abort: `in_progress` → `pending` after the
//!   same chip close as complete/skip, then the stored ids are **cleared**
//!   so a later start mints a NEW one-shot event. `pending` → 200 no-op
//!   (ids never cleared). `done`/`skipped` → 400 (reopen is the undo).
//! - `reopen` is the dedicated un-do verb: `done`/`skipped` return to
//!   `pending` with the stored chip ids **cleared** (`clear_event_ids` —
//!   literal NULLs). A start afterwards mints a **NEW** one-shot event — one
//!   *living* chip; the closed Google event stays as an orphaned log, never
//!   PATCHed or deleted. `pending` → 200 no-op (ids are never cleared),
//!   `in_progress` → 400 (`cannot reopen an in_progress occurrence` — no
//!   abort via reopen; a running occurrence exits via complete/skip/pause).
//!   Session-only, never a Google write.
//! - `/start` is **agenda-today-only** (this occurrence has an agenda item
//!   whose `local_date` is the user's ONE civil today — the primary Google
//!   calendar's IANA `time_zone` via chrono-tz, never a hardcoded zone; NOT
//!   `occurrence.local_date`, which is the rule date) and `pending`-only: it
//!   creates the one-shot Google log
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

use std::collections::{HashMap, HashSet};

use thiserror::Error;

use crate::calendar::{
    create_event, patch_event, patch_event_summary, CalendarError, CreateEventOutput,
};
use crate::categories::{
    classify, ensure_taxonomy, first_matching_index, first_matching_pattern, CalendarScope, CategoryWithPatterns,
    ClassifyOutcome,
};
use crate::config::OAuthConfig;
use crate::models::{
    AgendaItem, CalendarEvent, GoogleCalendar, NewEventInput, Routine, RoutineOccurrence, Task,
    TaskCategory, TaskCategoryPattern, UpdateOccurrence,
};
use crate::oauth::HttpClient;
use crate::repo::{
    AgendaItemRepo, CalendarEventOperationRepo, CalendarEventRepo, CalendarRepo, OccurrenceRepo,
    RepoError, RoutineRepo, TaskCategoryRepo, TaskListRepo, TaskRepo, TokenRepo,
};
use crate::tasks::{ElongateReport, START_EVENT_MINUTES, TaskCategorySummary, TaskView};
use crate::time::{
    ceil_5min_unix_in_zone, civil_date_in_zone, nearest_minute_unix, rfc3339_to_unix_secs,
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
            // A category color that cannot snap/resolve to a cached label is
            // a caller-side 400, folded onto the agenda Invalid arm.
            CalendarError::Invalid(message) => AgendaError::Invalid(message),
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

/// Response envelope for `GET /api/agenda` — a window of the infinite day
/// stream plus the server-computed civil today (ADR 0004 amendment): `today`
/// is the civil date of now in the user's primary calendar `time_zone`
/// (chrono-tz), the Home anchor the browser must not compute itself.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AgendaResponse {
    /// Every item in the window, ordered by `(local_date, sort_order,
    /// created_at)`.
    pub items: Vec<AgendaItemView>,
    /// `YYYY-MM-DD` — the resolved inclusive window start (never the raw
    /// query).
    pub from: String,
    /// `YYYY-MM-DD` — the resolved inclusive window end.
    pub to: String,
    /// `YYYY-MM-DD` — civil today in `time_zone`.
    pub today: String,
    /// IANA name of the zone that produced `today` (the primary calendar's
    /// `time_zone`, or `UTC` when the user has no primary calendar).
    pub time_zone: String,
    /// `YYYY-MM-DD` — the oldest past date that still holds a **pending**
    /// item, else `today`. The both-omitted window starts here.
    pub anchor_date: String,
    /// `YYYY-MM-DD` — `MIN(local_date)` over all of the user's agenda rows
    /// (orphans and future rows included), else `today`. Independent of the
    /// requested window: the browser's back-scroll floor.
    pub earliest_date: String,
}

/// Response envelope for `POST /api/agenda/items/:id/move` and
/// `POST /api/agenda/items/:id/reschedule`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AgendaItemResponse {
    pub item: AgendaItemView,
}

/// Response envelope for `POST /api/agenda/days/:date/move-to-today`: only
/// the rows the sweep actually moved, in the order they were appended to
/// today's pile.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MoveDayToTodayResponse {
    pub items: Vec<AgendaItemView>,
}

/// Response envelope for `PATCH /api/occurrences/:id`,
/// `POST /api/occurrences/:id/complete`, `POST /api/occurrences/:id/skip`,
/// `POST /api/occurrences/:id/pause`, and `POST /api/occurrences/:id/reopen`.
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
/// routine's display fields (`estimated_minutes`, the `rrule` recurrence
/// blob), the **resolved** title and its computed category.
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
    /// The routine's two-line recurrence blob (`DTSTART:` + `RRULE:`), for
    /// display.
    pub rrule: String,
    /// `true` only on the range read's **history** rows: the routine was
    /// soft-deleted and this occurrence is `done`/`skipped`, so the item is
    /// still shown (with the routine's title/estimate/blob) but must render
    /// as retired. Living routines always paint `false`, and the occurrence
    /// verbs never change it.
    pub routine_deleted: bool,
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
// GET /api/agenda — a pure range read of the day stream
// ──────────────────────────────────────────

/// The user's **one civil today** (ADR 0004 amendment): the civil date of
/// `now_unix` in the **primary Google calendar's IANA `time_zone`**
/// (chrono-tz, DST-aware), plus that zone name. No primary calendar (or no
/// calendars at all) → UTC. "Travel = home base": the primary calendar is the
/// user's home zone — the hotel's timezone never moves today, and no zone is
/// ever hardcoded.
pub(crate) async fn user_today(
    calendars: &dyn CalendarRepo,
    user_id: &str,
    now_unix: i64,
) -> Result<(String, String), AgendaError> {
    let time_zone = calendars
        .list_by_user_id(user_id)
        .await?
        .into_iter()
        .find(|cal| cal.is_primary)
        .map(|cal| cal.time_zone)
        .unwrap_or_else(|| "UTC".to_string());
    let date = civil_date_in_zone(now_unix, &time_zone);
    Ok((date, time_zone))
}

/// `GET /api/agenda?from=YYYY-MM-DD&to=YYYY-MM-DD` →
/// `{"items":[…],"from":…,"to":…,"today":…,"time_zone":…,"anchor_date":…,
/// "earliest_date":…}`.
///
/// A **pure read** (ADR 0004 amendment, infinite stream): no occurrence is
/// ensured and no agenda row is written — not even when a living routine's
/// rule covers the window. Materializing occurrences is the occurrence fill's
/// job, never a GET's.
///
/// Window resolution (trim first; empty/whitespace counts as omitted):
/// - both omitted → the **anchor week**: `from = anchor_date`, `to =
///   from + 6` civil days (7 inclusive days);
/// - both present → the inclusive range; `to < from` is a 400, and an
///   inclusive span above 31 days is a 400;
/// - exactly one present → 400 "both or neither";
/// - a non-empty value that is not `YYYY-MM-DD` → 400.
///
/// `anchor_date` is the oldest **past** date that still holds a pending item
/// (a living `OPEN | PLANNED | IN_PROGRESS` task, or a `pending |
/// in_progress` occurrence of a living routine), else today. It is computed
/// by one repo query, never by loading history. `earliest_date` is
/// `MIN(local_date)` over every agenda row of the user (orphans and future
/// rows included), else today — the browser's back-scroll floor, independent
/// of this page.
///
/// Hydration is batched: one `list_by_user_and_date_range` page read, one
/// `list_by_ids` for the page's occurrences, `list_by_user_id` for living
/// tasks/routines, and `list_by_ids_including_deleted` only for routine ids
/// missing from the living set. Soft-deleted tasks and orphan/unknown-kind
/// rows are omitted while their membership rows stay in D1. A deleted
/// routine's `done | skipped` occurrence still renders (with
/// `routine_deleted: true`, borrowing the routine's title/estimate/blob);
/// its `pending | in_progress` leftovers are omitted.
///
/// `focused_task_id` is the caller's pointer into this user's focus lock —
/// it only paints the embedded tasks' `focused` flag (reads never write the
/// users row), the same contract as `tasks::list_tasks`.
pub async fn get_agenda(
    calendars: &dyn CalendarRepo,
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    agenda_repo: &dyn AgendaItemRepo,
    task_repo: &dyn TaskRepo,
    user_id: &str,
    from: Option<&str>,
    to: Option<&str>,
    now_unix: i64,
    focused_task_id: Option<&str>,
) -> Result<AgendaResponse, AgendaError> {
    let (today, time_zone) = user_today(calendars, user_id, now_unix).await?;
    // The anchor is needed even when the caller names a window — the response
    // always carries it — and it decides the default window's start.
    let anchor_date = agenda_repo
        .oldest_pending_local_date_before(user_id, &today)
        .await?
        .unwrap_or_else(|| today.clone());
    let (from, to) = resolve_window(from, to, &anchor_date)?;
    let taxonomy = load_taxonomy_seeded(list_repo, category_repo, user_id).await?;

    // One page read for the whole window; the repo owns the stream order.
    let items = agenda_repo
        .list_by_user_and_date_range(user_id, &from, &to)
        .await?;
    // The back-scroll floor is page-independent: ALL of the user's rows.
    let earliest_date = agenda_repo
        .min_local_date(user_id)
        .await?
        .unwrap_or_else(|| today.clone());

    let mut views = Vec::with_capacity(items.len());
    if !items.is_empty() {
        let user_tasks = task_repo.list_by_user_id(user_id).await?;
        let tasks: HashMap<&str, &Task> =
            user_tasks.iter().map(|task| (task.id.as_str(), task)).collect();

        let occurrence_ids: Vec<String> = items
            .iter()
            .filter(|item| item.kind == AGENDA_KIND_OCCURRENCE)
            .map(|item| item.ref_id.clone())
            .collect();
        let fetched_occurrences: Vec<RoutineOccurrence> = if occurrence_ids.is_empty() {
            Vec::new()
        } else {
            occurrence_repo.list_by_ids(&occurrence_ids).await?
        };
        let occurrences: HashMap<&str, &RoutineOccurrence> = fetched_occurrences
            .iter()
            .map(|occ| (occ.id.as_str(), occ))
            .collect();

        // Living routines first; only the ids missing from that set are
        // fetched again INCLUDING soft-deleted rows — a read never loads
        // every deleted routine up front.
        let living_routines = routine_repo.list_by_user_id(user_id).await?;
        let living_ids: HashSet<&str> = living_routines
            .iter()
            .map(|routine| routine.id.as_str())
            .collect();
        let mut missing: HashSet<&str> = HashSet::new();
        for item in &items {
            if item.kind != AGENDA_KIND_OCCURRENCE {
                continue;
            }
            let Some(occ) = occurrences.get(item.ref_id.as_str()) else {
                continue;
            };
            if occ.user_id != item.user_id {
                continue;
            }
            if !living_ids.contains(occ.routine_id.as_str()) {
                missing.insert(occ.routine_id.as_str());
            }
        }
        let mut missing_ids: Vec<String> = missing.into_iter().map(str::to_string).collect();
        missing_ids.sort();
        let historical_routines: Vec<Routine> = if missing_ids.is_empty() {
            Vec::new()
        } else {
            routine_repo
                .list_by_ids_including_deleted(&missing_ids)
                .await?
        };
        let routines_by_id: HashMap<&str, &Routine> = living_routines
            .iter()
            .chain(historical_routines.iter())
            .map(|routine| (routine.id.as_str(), routine))
            .collect();

        for item in &items {
            let (task, occurrence) = match item.kind.as_str() {
                AGENDA_KIND_TASK => {
                    // `list_by_user_id` filters soft-deleted tasks — a
                    // deleted/missing task omits the whole item (orphan
                    // membership rows stay in D1).
                    let Some(task) = tasks.get(item.ref_id.as_str()) else {
                        continue;
                    };
                    (Some(task_view(task, &taxonomy, focused_task_id)), None)
                }
                AGENDA_KIND_OCCURRENCE => {
                    let Some(occ) = occurrences.get(item.ref_id.as_str()) else {
                        continue;
                    };
                    // Never leak another user's occurrence through a
                    // hand-edited row.
                    if occ.user_id != item.user_id {
                        continue;
                    }
                    // Missing entirely (row gone) → omit; a soft-deleted
                    // routine is still resolvable through the history batch.
                    let Some(routine) = routines_by_id.get(occ.routine_id.as_str()) else {
                        continue;
                    };
                    let routine_deleted = routine.deleted_at.is_some();
                    // A deleted routine's live leftovers are noise; its
                    // done/skipped history still renders for the record.
                    if routine_deleted
                        && !matches!(
                            occ.status.as_str(),
                            OCCURRENCE_STATUS_DONE | OCCURRENCE_STATUS_SKIPPED
                        )
                    {
                        continue;
                    }
                    let mut view = occurrence_view(occ, routine, &taxonomy);
                    view.routine_deleted = routine_deleted;
                    (None, Some(view))
                }
                _ => continue,
            };
            views.push(AgendaItemView {
                id: item.id.clone(),
                user_id: item.user_id.clone(),
                local_date: item.local_date.clone(),
                kind: item.kind.clone(),
                ref_id: item.ref_id.clone(),
                sort_order: item.sort_order,
                task,
                occurrence,
            });
        }
    }
    Ok(AgendaResponse {
        items: views,
        from,
        to,
        today,
        time_zone,
        anchor_date,
        earliest_date,
    })
}

/// Resolves the requested inclusive window under the stream contract: trim
/// first, empty/whitespace = omitted, both-or-neither, `YYYY-MM-DD` only,
/// `to >= from`, and at most 31 inclusive days. Both omitted → the anchor
/// week (`anchor_date … anchor_date + 6`).
fn resolve_window(
    from: Option<&str>,
    to: Option<&str>,
    anchor_date: &str,
) -> Result<(String, String), AgendaError> {
    let from = from.map(str::trim).filter(|value| !value.is_empty());
    let to = to.map(str::trim).filter(|value| !value.is_empty());
    match (from, to) {
        (None, None) => {
            let from = parse_date(anchor_date)?;
            let to = add_civil_days(&from, 6)?;
            Ok((from, to))
        }
        (Some(from), Some(to)) => {
            let from = parse_date(from)?;
            let to = parse_date(to)?;
            let from_date = civil_date(&from)?;
            let to_date = civil_date(&to)?;
            if to_date < from_date {
                return Err(AgendaError::Invalid(
                    "to must not be before from".to_string(),
                ));
            }
            // Inclusive span is the day difference + 1: 30 → 31 days is valid.
            if to_date.signed_duration_since(from_date).num_days() > 30 {
                return Err(AgendaError::Invalid(
                    "date range must not exceed 31 days".to_string(),
                ));
            }
            Ok((from, to))
        }
        _ => Err(AgendaError::Invalid(
            "from and to must both be set or both be omitted".to_string(),
        )),
    }
}

/// Parses an already-validated `YYYY-MM-DD` into a civil date (window
/// arithmetic only — [`parse_date`] owns the 400 text).
fn civil_date(date: &str) -> Result<chrono::NaiveDate, AgendaError> {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|_| AgendaError::Invalid("date must be YYYY-MM-DD".to_string()))
}

/// Adds `days` civil days to a validated `YYYY-MM-DD` date. Overflow only
/// happens past chrono's date range; it is a 400, never a panic. Shared with
/// the agenda fill's horizon arithmetic.
pub(crate) fn add_civil_days(date: &str, days: i64) -> Result<String, AgendaError> {
    civil_date(date)?
        .checked_add_signed(chrono::Duration::days(days))
        .map(|date| date.format("%Y-%m-%d").to_string())
        .ok_or_else(|| AgendaError::Invalid("date out of range".to_string()))
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
// POST /api/agenda/items/:id/reschedule — relocate a slot onto today or later
// ──────────────────────────────────────────

/// `POST /api/agenda/items/:id/reschedule { date, sort_order? }` →
/// `{"item":…}` (the same embed as move).
///
/// Relocates the item's slot to `date` — **today or later** (the past is a
/// 400: the day is over) — with an optional landing `sort_order` (ADR 0004
/// amendment). Session-only — no Google write of any kind, no RRULE anywhere,
/// **no EXDATE anywhere**.
///
/// **Occurrence** (kind `occurrence`):
/// 1. Load the item + its occurrence + the living routine (404s as always).
/// 2. `pending | skipped` are the only reschedulable statuses — `skipped`
///    becomes `pending` on the new date (deferred, not declined);
///    `in_progress`/`done` → 400.
/// 3. The **agenda item moves only**: `occurrence.local_date` is never
///    rewritten (it stays the series-instance key — the rule date), and the
///    routine's `rrule` is never touched. A target date that already holds
///    another occurrence of this routine is FINE — daily snooze
///    today→tomorrow puts two rows of the same routine on tomorrow.
/// 4. Title override and any stored google ids travel with the row (never
///    cleared).
///
/// **Task** (kind `task`): the **membership slot** moves, not the task —
/// `tasks.status` is unchanged and **no `task_logs` row is written** (the
/// agenda is an overlay, and `task_logs` has no civil-date column).
/// `COMPLETED`/`DISCARDED` → 400. One row per task (0018) means the source
/// row itself moves: there is no target row to merge into.
///
/// **Placement** on a different day: `sort_order` omitted → appended at
/// `max+1` (0 when the target pile is empty); named → the target day's peers
/// at/after the rank shift up one first (same gap semantics as `/move`), then
/// the row lands on the rank. The source day's pile is never reshuffled.
///
/// Same date → 200 no-op returning the item unchanged, even when a
/// `sort_order` is named — same-day placement is `/move`. Missing item /
/// other-user / missing or soft-deleted task / soft-deleted routine → 404.
/// Missing/invalid `date` → 400.
pub async fn reschedule_agenda_item(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    agenda_repo: &dyn AgendaItemRepo,
    task_repo: &dyn TaskRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    routine_repo: &dyn RoutineRepo,
    calendars: &dyn CalendarRepo,
    now_unix: i64,
    user_id: &str,
    id: &str,
    date: &str,
    sort_order: Option<i64>,
    focused_task_id: Option<&str>,
) -> Result<AgendaItemResponse, AgendaError> {
    let date = parse_date(date)?;
    let Some(item) = agenda_repo.get_by_id(id).await? else {
        return Err(AgendaError::NotFound);
    };
    if item.user_id != user_id {
        return Err(AgendaError::NotFound);
    }
    // A reschedule only ever moves forward. `YYYY-MM-DD` sorts
    // chronologically, so the string compare IS the civil-date compare; today
    // itself is allowed.
    let (today, _time_zone) = user_today(calendars, user_id, now_unix).await?;
    if date < today {
        return Err(AgendaError::Invalid(
            "cannot reschedule into the past".to_string(),
        ));
    }
    // Same date → 200 no-op: nothing moves, nothing is shifted (a named
    // `sort_order` is ignored — same-day placement is `/move`).
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
    // Only validated when the row is actually going to be placed.
    if let Some(rank) = sort_order {
        if rank < 0 {
            return Err(AgendaError::Invalid(
                "sort_order must be a non-negative integer".to_string(),
            ));
        }
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
                sort_order,
            )
            .await?;
        }
        AGENDA_KIND_TASK => {
            reschedule_task_slot(agenda_repo, task_repo, user_id, &item, &date, sort_order)
                .await?;
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

/// Places a membership row on another date's pile: a named `sort_order`
/// shifts that day's peers at/after the rank up one first (the moving row is
/// not on that date yet, so it is never shifted), an omitted one appends at
/// `max+1` (0 when the pile is empty). Same gap semantics as `/move`.
async fn place_on_date(
    agenda_repo: &dyn AgendaItemRepo,
    user_id: &str,
    id: &str,
    date: &str,
    sort_order: Option<i64>,
) -> Result<(), AgendaError> {
    let rank = match sort_order {
        Some(rank) => {
            agenda_repo.shift_sort_order(user_id, date, rank, 1).await?;
            rank
        }
        None => agenda_repo
            .max_sort_order(user_id, date)
            .await?
            .map(|max| max + 1)
            .unwrap_or(0),
    };
    agenda_repo.set_local_date(id, date, rank).await?;
    Ok(())
}

/// The occurrence half of a reschedule:
/// 1. verify the occurrence + its routine are alive and owned (404 otherwise),
/// 2. gate the status (`pending | skipped` only; `skipped` → `pending`),
/// 3. place the **agenda item** on the target pile ([`place_on_date`]).
///
/// `occurrence.local_date` is deliberately NEVER rewritten — it is the
/// series-instance key (the rule date), and the "Monday must not come back"
/// guarantee now comes from the fill's no-membership-anywhere rule, not from
/// an exdate. The routine's `rrule` is never touched either.
async fn reschedule_occurrence(
    agenda_repo: &dyn AgendaItemRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    routine_repo: &dyn RoutineRepo,
    user_id: &str,
    item: &AgendaItem,
    date: &str,
    sort_order: Option<i64>,
) -> Result<(), AgendaError> {
    let Some(occurrence) = occurrence_repo.get_by_id(&item.ref_id).await? else {
        return Err(AgendaError::NotFound);
    };
    if occurrence.user_id != user_id {
        return Err(AgendaError::NotFound);
    }
    // A soft-deleted routine makes its materialized occurrences 404 (same
    // gate as every occurrence verb).
    if routine_repo.get_by_id(&occurrence.routine_id).await?.is_none() {
        return Err(AgendaError::NotFound);
    }

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

    // NO target-date UNIQUE check: `occurrence.local_date` is the rule date,
    // not the moved-to date, so it cannot tell us whether the target pile
    // already holds this routine. A daily snooze today→tomorrow legitimately
    // puts TWO rows of the same routine on tomorrow (today's moved instance +
    // tomorrow's own seeded instance) — that is allowed.
    if occurrence.status == OCCURRENCE_STATUS_SKIPPED {
        occurrence_repo
            .set_status(&occurrence.id, OCCURRENCE_STATUS_PENDING)
            .await?;
    }
    // The agenda row lands on the target pile (the source pile keeps its
    // ranks). `occurrence.local_date` and `routines.rrule` are untouched.
    place_on_date(agenda_repo, user_id, &item.id, date, sort_order).await
}

/// The task half of a reschedule: the **membership slot** moves, never the
/// task — no `tasks.status` change, no `task_logs` row (the agenda is an
/// overlay, and `task_logs` has no civil-date column). A terminal task is a
/// 400. One row per task (0018): the source row itself moves, so there is no
/// second row to look up, merge into, or hard-delete.
async fn reschedule_task_slot(
    agenda_repo: &dyn AgendaItemRepo,
    task_repo: &dyn TaskRepo,
    user_id: &str,
    item: &AgendaItem,
    date: &str,
    sort_order: Option<i64>,
) -> Result<(), AgendaError> {
    let Some(task) = task_repo.get_by_id(&item.ref_id).await? else {
        return Err(AgendaError::NotFound); // missing / soft-deleted
    };
    if task.user_id != user_id {
        return Err(AgendaError::NotFound);
    }
    if matches!(task.status.as_str(), "COMPLETED" | "DISCARDED") {
        return Err(AgendaError::Invalid(
            "cannot reschedule a terminal task".to_string(),
        ));
    }
    place_on_date(agenda_repo, user_id, &item.id, date, sort_order).await
}

// ──────────────────────────────────────────
// POST /api/agenda/days/:date/move-to-today — sweep a past day's leftovers
// ──────────────────────────────────────────

/// `POST /api/agenda/days/:date/move-to-today` → `{"items":[…]}`.
///
/// The one-call catch-up for a skipped past day: every **still-pending** item
/// of `date` moves onto **civil today**, appended to today's pile in the
/// order it held on the past day (after whatever is already there). No Google
/// write, no RRULE anywhere.
///
/// Movable rows:
/// - kind `task` whose living task is `OPEN | PLANNED | IN_PROGRESS` (an
///   `in_progress` **task** moves);
/// - kind `occurrence` whose status is `pending` and whose routine is living
///   (the same gate as a reschedule — `occurrence.local_date` is never
///   rewritten: it stays the rule date).
///
/// Everything else stays put: `done`/`skipped`/`in_progress` occurrences,
/// `COMPLETED`/`DISCARDED` and soft-deleted tasks, soft-deleted-routine
/// leftovers, orphans, and unknown kinds. A row that cannot move never fails
/// the call.
///
/// `date` must be in the **past** (`>= today` → 400). Invalid `date` → 400.
/// Session-only.
pub async fn move_day_to_today(
    calendars: &dyn CalendarRepo,
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    agenda_repo: &dyn AgendaItemRepo,
    task_repo: &dyn TaskRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    routine_repo: &dyn RoutineRepo,
    user_id: &str,
    date: &str,
    now_unix: i64,
    focused_task_id: Option<&str>,
) -> Result<MoveDayToTodayResponse, AgendaError> {
    let date = parse_date(date)?;
    let (today, _time_zone) = user_today(calendars, user_id, now_unix).await?;
    // `YYYY-MM-DD` sorts chronologically: today itself has nothing to catch
    // up on, and the future is not a "past day".
    if date >= today {
        return Err(AgendaError::Invalid(
            "cannot move a day that is not in the past".to_string(),
        ));
    }

    // One pile read, in pile order — the moved rows keep their relative order.
    let items = agenda_repo.list_by_user_and_date(user_id, &date).await?;
    let mut movable = Vec::new();
    if !items.is_empty() {
        // Living tasks only (`list_by_user_id` filters soft-deleted rows).
        let tasks = task_repo.list_by_user_id(user_id).await?;
        for item in &items {
            match item.kind.as_str() {
                AGENDA_KIND_TASK => {
                    let Some(task) = tasks.iter().find(|task| task.id == item.ref_id) else {
                        continue; // orphan / soft-deleted task — stays put
                    };
                    if matches!(task.status.as_str(), "OPEN" | "PLANNED" | "IN_PROGRESS") {
                        movable.push(item);
                    }
                }
                AGENDA_KIND_OCCURRENCE => {
                    let Some(occurrence) = occurrence_repo.get_by_id(&item.ref_id).await? else {
                        continue; // orphan — stays put
                    };
                    // Never leak another user's occurrence through a
                    // hand-edited row.
                    if occurrence.user_id != user_id {
                        continue;
                    }
                    // Only a waiting chip moves: `skipped` is a decline and
                    // `in_progress`/`done` are already spoken for.
                    if occurrence.status != OCCURRENCE_STATUS_PENDING {
                        continue;
                    }
                    // A soft-deleted routine's leftover stays where it is.
                    if routine_repo.get_by_id(&occurrence.routine_id).await?.is_none() {
                        continue;
                    }
                    movable.push(item);
                }
                _ => continue, // unknown kinds stay put
            }
        }
    }
    if movable.is_empty() {
        return Ok(MoveDayToTodayResponse { items: Vec::new() });
    }

    // Each row takes the then-current `max+1` on today's pile, so the moved
    // rows follow whatever is already on today in their original order.
    let mut rank = agenda_repo
        .max_sort_order(user_id, &today)
        .await?
        .map(|max| max + 1)
        .unwrap_or(0);
    let taxonomy = load_taxonomy_seeded(list_repo, category_repo, user_id).await?;
    let mut views = Vec::with_capacity(movable.len());
    for item in movable {
        agenda_repo.set_local_date(&item.id, &today, rank).await?;
        let mut moved = item.clone();
        moved.local_date = today.clone();
        moved.sort_order = rank;
        rank += 1;
        // The backing row was verified living a moment ago; a concurrent
        // delete simply drops it from the answer (the move itself stands).
        if let Some(view) = embed_item(
            task_repo,
            occurrence_repo,
            routine_repo,
            &moved,
            &taxonomy,
            focused_task_id,
        )
        .await?
        {
            views.push(view);
        }
    }
    Ok(MoveDayToTodayResponse { items: views })
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
    operations: Option<&dyn CalendarEventOperationRepo>,
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
        if let (Some(http), Some(calendars), Some(events), Some(operations), Some(access)) =
            (http, calendars, events, operations, access)
        {
            let output = patch_event_summary(
                http,
                calendars,
                events,
                operations,
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
// POST /api/occurrences/:id/start, /pause, /complete and /skip — verb matrix + Google
// ──────────────────────────────────────────

/// `POST /api/occurrences/:id/start` → 200 `{"occurrence":…,"event":…}`.
///
/// Starts a **pending, agenda-today** occurrence: creates the one-shot Google
/// log (summary = the **resolved** title, carriers
/// `sanctuary_routine_id`/`sanctuary_occurrence_id`, never a task_id, never
/// an RRULE, `T … T + START_EVENT_MINUTES` on the minute grid), stores
/// `calendar_id` + `google_event_id` on the occurrence, and flips it to
/// `in_progress`. One **living** chip per occurrence: reopen clears the
/// stored ids, so a later start mints a new event (the closed one stays as an
/// orphaned log).
///
/// Verb matrix (locked): `pending` → start; `in_progress` → **200 no-op**
/// (no second event); `done`/`skipped` → 400. Start is valid only when this
/// occurrence has an **agenda item whose `local_date` is civil today** — the
/// user's ONE today: the civil date of `now_unix` in the **primary Google
/// calendar's IANA `time_zone`** (chrono-tz, ADR 0004 amendment — travel =
/// home base, never a hardcoded zone); otherwise 400. The gate is agenda
/// membership, NOT `occurrence.local_date`: a reschedule moves the item (and
/// the plan) to another day while the occurrence keeps its rule date, so
/// "today" is where the item sits. Missing / other-user / soft-deleted-routine
/// → 404. No writable calendar → 400. Focus stays task-only:
/// `sanctuary_focus` is never set here.
pub async fn start_occurrence(
    http: &dyn HttpClient,
    calendars: &dyn CalendarRepo,
    events: &dyn CalendarEventRepo,
    operations: &dyn CalendarEventOperationRepo,
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    agenda_repo: &dyn AgendaItemRepo,
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
    // Agenda-today only (ADR 0004 amendment): the user's ONE civil today —
    // the primary Google calendar's IANA time_zone via chrono-tz (travel =
    // home base), never a hardcoded zone — and the occurrence must be
    // SCHEDULED there — i.e. its agenda item sits on that date.
    // `occurrence.local_date` is the rule date and says nothing about where
    // the item was rescheduled to.
    let (today, _time_zone) = user_today(calendars, user_id, now_unix).await?;
    if agenda_repo
        .get_by_key(user_id, &today, AGENDA_KIND_OCCURRENCE, &occurrence.id)
        .await?
        .is_none()
    {
        return Err(AgendaError::Invalid(
            "occurrence can only be started on a day it is scheduled".to_string(),
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
        operations,
        access,
        user_id,
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
            color_hex: target.color_hex,
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
/// available), `done` → 200 no-op, `skipped` → `done`. The dedicated un-do
/// verb is `reopen_occurrence` (`done`/`skipped` → `pending`, ids cleared).
/// Missing / other-user / soft-deleted-routine → 404.
pub async fn complete_occurrence(
    http: Option<&dyn HttpClient>,
    calendars: Option<&dyn CalendarRepo>,
    events: Option<&dyn CalendarEventRepo>,
    operations: Option<&dyn CalendarEventOperationRepo>,
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
        http, calendars, events, operations, access, list_repo, category_repo, routine_repo,
        occurrence_repo, user_id, id, now_unix, OCCURRENCE_STATUS_DONE,
    )
    .await
}

/// `POST /api/occurrences/:id/skip` → 200 `{"occurrence":…}`.
///
/// Verb matrix (locked): `pending` → `skipped`, `in_progress` → `skipped`
/// (the open chip's end is PATCHed closed first when ids are stored and
/// Google is available), `done` → `skipped`, `skipped` → 200 no-op. The
/// dedicated unskip path is `reopen_occurrence` (`skipped`/`done` →
/// `pending`, ids cleared). Missing / other-user / soft-deleted-routine →
/// 404.
pub async fn skip_occurrence(
    http: Option<&dyn HttpClient>,
    calendars: Option<&dyn CalendarRepo>,
    events: Option<&dyn CalendarEventRepo>,
    operations: Option<&dyn CalendarEventOperationRepo>,
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
        http, calendars, events, operations, access, list_repo, category_repo, routine_repo,
        occurrence_repo, user_id, id, now_unix, OCCURRENCE_STATUS_SKIPPED,
    )
    .await
}

/// `POST /api/occurrences/:id/pause` → 200 `{"occurrence":…}`.
///
/// The dedicated abort (ADR 0004 amendment): `in_progress` → `pending` after
/// the open chip's end is PATCHed closed (same invert guard as complete/skip
/// when ids are stored and Google is available), then the stored chip ids
/// are **cleared** so a later start mints a NEW one-shot event. `pending` →
/// 200 no-op (ids never cleared). `done`/`skipped` → 400 (reopen is the
/// undo). Google optional — `http`/`access` `None` still flips + clears.
/// Missing / other-user / soft-deleted-routine → 404.
pub async fn pause_occurrence(
    http: Option<&dyn HttpClient>,
    calendars: Option<&dyn CalendarRepo>,
    events: Option<&dyn CalendarEventRepo>,
    operations: Option<&dyn CalendarEventOperationRepo>,
    access: Option<&GoogleAccess>,
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    user_id: &str,
    id: &str,
    now_unix: i64,
) -> Result<OccurrenceResponse, AgendaError> {
    let occurrence = load_occurrence_for_user(occurrence_repo, routine_repo, user_id, id).await?;
    match occurrence.status.as_str() {
        OCCURRENCE_STATUS_PENDING => {
            occurrence_response(list_repo, category_repo, routine_repo, occurrence_repo, user_id, id)
                .await
        }
        OCCURRENCE_STATUS_IN_PROGRESS => {
            if occurrence.calendar_id.is_some() && occurrence.google_event_id.is_some() {
                if let (Some(http), Some(calendars), Some(events), Some(operations), Some(access)) =
                    (http, calendars, events, operations, access)
                {
                    close_occurrence_event(
                        http,
                        calendars,
                        events,
                        operations,
                        access,
                        occurrence.calendar_id.as_deref().expect("checked above"),
                        occurrence.google_event_id.as_deref().expect("checked above"),
                        now_unix,
                    )
                    .await?;
                }
            }
            occurrence_repo.clear_event_ids(id).await?;
            occurrence_repo
                .set_status(id, OCCURRENCE_STATUS_PENDING)
                .await?;
            occurrence_response(list_repo, category_repo, routine_repo, occurrence_repo, user_id, id)
                .await
        }
        other => Err(AgendaError::Invalid(format!(
            "cannot pause a {other} occurrence"
        ))),
    }
}

/// `POST /api/occurrences/:id/reopen` → 200 `{"occurrence":…}`.
///
/// The dedicated un-do verb (ADR 0004 amendment): `done`/`skipped` return to
/// `pending` with the stored chip ids **cleared** — a later start mints a NEW
/// one-shot event; the closed Google event stays as an orphaned log, never
/// PATCHed or deleted. `pending` → **200 no-op** (ids are never cleared —
/// they are only written by a fresh start). `in_progress` → 400 (no abort
/// via reopen: a running occurrence exits via complete/skip/pause).
/// Session-only — never a Google write of any kind. Missing / other-user /
/// soft-deleted-routine → 404.
pub async fn reopen_occurrence(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    user_id: &str,
    id: &str,
) -> Result<OccurrenceResponse, AgendaError> {
    let occurrence = load_occurrence_for_user(occurrence_repo, routine_repo, user_id, id).await?;
    match occurrence.status.as_str() {
        // 200 no-op: nothing to un-do, and ids are NEVER cleared here — a
        // pending occurrence that somehow carries a chip keeps it.
        OCCURRENCE_STATUS_PENDING => {
            occurrence_response(list_repo, category_repo, routine_repo, occurrence_repo, user_id, id)
                .await
        }
        // No abort via reopen: a running occurrence exits via
        // complete/skip/pause.
        OCCURRENCE_STATUS_IN_PROGRESS => Err(AgendaError::Invalid(
            "cannot reopen an in_progress occurrence".to_string(),
        )),
        // The un-do path: chip ids first (the old closed event becomes an
        // orphaned log), then the flip back to pending.
        OCCURRENCE_STATUS_DONE | OCCURRENCE_STATUS_SKIPPED => {
            occurrence_repo.clear_event_ids(id).await?;
            occurrence_repo
                .set_status(id, OCCURRENCE_STATUS_PENDING)
                .await?;
            occurrence_response(list_repo, category_repo, routine_repo, occurrence_repo, user_id, id)
                .await
        }
        other => Err(AgendaError::Invalid(format!(
            "cannot reopen a {other} occurrence"
        ))),
    }
}

/// The shared complete/skip machinery: close the running chip when leaving
/// `in_progress` with stored ids (Google optional — `http`/`access` `None`
/// means session-only, the flip still happens), then flip the status.
/// Already in the target status → 200 no-op (no Google write).
async fn exit_occurrence(
    http: Option<&dyn HttpClient>,
    calendars: Option<&dyn CalendarRepo>,
    events: Option<&dyn CalendarEventRepo>,
    operations: Option<&dyn CalendarEventOperationRepo>,
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
        if let (Some(http), Some(calendars), Some(events), Some(operations), Some(access)) =
            (http, calendars, events, operations, access)
        {
            close_occurrence_event(
                http,
                calendars,
                events,
                operations,
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
    operations: &dyn CalendarEventOperationRepo,
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
        operations,
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
    /// Matched category's hex color; `None` when the resolved title is
    /// untracked or the category has no (non-blank) color — the event insert
    /// omits the label then.
    color_hex: Option<String>,
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
        // The matched category's hex color, or `None` for untracked /
        // categories without one (or a blank one) — the event insert omits
        // the label then.
        color_hex: category
            .map(|category| category.color.trim().to_string())
            .filter(|color| !color.is_empty()),
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
    operations: &dyn CalendarEventOperationRepo,
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
            operations,
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
        rrule: routine.rrule.clone(),
        routine_deleted: false,
        calendar_id: occurrence.calendar_id.clone(),
        google_event_id: occurrence.google_event_id.clone(),
        created_at: occurrence.created_at.clone(),
        updated_at: occurrence.updated_at.clone(),
        category,
    }
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
        matchers.push(CategoryWithPatterns::new(
            category.id.clone(),
            category.parent_id.clone(),
            patterns,
        ));
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
    let Some(index) = first_matching_index(title, CalendarScope::Ignore, matcher) else {
        return (String::new(), String::new(), title.to_string());
    };
    match matcher.split_hole_at(index, title) {
        Ok(split) => (split.prefix, split.suffix, split.hole),
        Err(_) => (String::new(), String::new(), title.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::agenda_fill::{
        ensure_task_items, fill_occurrence_horizon, run_agenda_fill_cron, AgendaFillReport,
    };
    use crate::models::{
        GoogleCalendar, GoogleOAuthToken, NewAgendaItem, NewCalendar, NewCalendarEvent, NewRoutine,
        NewRoutineInput, NewRoutineOccurrence, NewTask, NewTaskCategory, NewTaskCategoryPattern,
        NewTaskList, NewToken, Task, TaskList, UpdateRoutine, UpdateTask, UpdateTaskCategory,
        UpdateTaskList,
    };
    use crate::oauth::{HttpClient, HttpError};
    use crate::routines::{
        create_routine, delete_routine, occurrence_dates, update_routine, DeleteRoutineResponse,
        RoutineResponse, RoutinesError,
    };

    // ──────────────────────────────────────────
    // Fakes
    // ──────────────────────────────────────────

    /// Cheap call counters on the occurrence/agenda fakes — the range-read
    /// test snapshots these to prove a read makes zero writes and uses only
    /// the batched set reads (the range page, the anchor, the floor, and one
    /// `list_by_ids`).
    #[derive(Debug, Clone, Default)]
    struct CallCounters {
        insert: u64,
        insert_many: u64,
        delete_by_ids: u64,
        get_by_id: u64,
        get_by_ref: u64,
        list_by_refs: u64,
        list_by_ids: u64,
        list_by_user_and_date: u64,
        list_by_user_and_date_range: u64,
        min_local_date: u64,
        oldest_pending_local_date_before: u64,
    }

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
    /// tests): soft-deleted rows are filtered from reads. `stored` is an
    /// `Arc` because `FakeAgendaItemRepo` joins it for the anchor predicate.
    struct FakeRoutineRepo {
        stored: Arc<Mutex<Vec<Routine>>>,
        next_id: Mutex<u64>,
    }

    impl FakeRoutineRepo {
        fn new() -> Self {
            Self {
                stored: Arc::new(Mutex::new(Vec::new())),
                next_id: Mutex::new(1),
            }
        }

        fn row(
            id: &str,
            user_id: &str,
            title: &str,
            sort_order: i64,
            rrule: &str,
        ) -> Routine {
            Routine {
                id: id.to_string(),
                user_id: user_id.to_string(),
                title: title.to_string(),
                estimated_minutes: 15,
                rrule: rrule.to_string(),
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
                rrule: routine.rrule,
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
            if let Some(rrule) = &updates.rrule {
                row.rrule = rrule.clone();
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

        async fn list_user_ids_with_living_routines(&self) -> Result<Vec<String>, RepoError> {
            // Mirrors ROUTINE_LIST_USER_IDS_WITH_LIVING_SQL: distinct living
            // owners, unordered.
            let rows = self.stored.lock().unwrap();
            let mut ids: Vec<String> = rows
                .iter()
                .filter(|row| row.deleted_at.is_none())
                .map(|row| row.user_id.clone())
                .collect();
            ids.sort();
            ids.dedup();
            Ok(ids)
        }

        async fn list_by_ids_including_deleted(&self, ids: &[String]) -> Result<Vec<Routine>, RepoError> {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let id_set: HashSet<&str> = ids.iter().map(|id| id.as_str()).collect();
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| id_set.contains(row.id.as_str()))
                .cloned()
                .collect())
        }
    }

    /// In-memory `OccurrenceRepo`: no soft-delete (the table has none); the
    /// insert mirrors `INSERT OR IGNORE` — a duplicate `(routine_id,
    /// local_date)` returns the existing row. `stored` is an `Arc` because
    /// `FakeAgendaItemRepo` joins it for the anchor predicate.
    struct FakeOccurrenceRepo {
        stored: Arc<Mutex<Vec<RoutineOccurrence>>>,
        next_id: Mutex<u64>,
        calls: Mutex<CallCounters>,
    }

    impl FakeOccurrenceRepo {
        fn new() -> Self {
            Self {
                stored: Arc::new(Mutex::new(Vec::new())),
                next_id: Mutex::new(1),
                calls: Mutex::new(CallCounters::default()),
            }
        }

        fn call_counts(&self) -> CallCounters {
            self.calls.lock().unwrap().clone()
        }

        fn reset_calls(&self) {
            *self.calls.lock().unwrap() = CallCounters::default();
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
            self.calls.lock().unwrap().get_by_id += 1;
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
            self.calls.lock().unwrap().list_by_user_and_date += 1;
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

        async fn list_by_user_and_date_range(
            &self,
            user_id: &str,
            from: &str,
            to: &str,
        ) -> Result<Vec<RoutineOccurrence>, RepoError> {
            self.calls.lock().unwrap().list_by_user_and_date_range += 1;
            let mut rows: Vec<RoutineOccurrence> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                // `YYYY-MM-DD` sorts chronologically, so a string window is
                // the same inclusive range as the SQL comparison.
                .filter(|row| {
                    row.user_id == user_id
                        && row.local_date.as_str() >= from
                        && row.local_date.as_str() <= to
                })
                .cloned()
                .collect();
            rows.sort_by(|a, b| {
                (&a.local_date, &a.created_at, &a.id).cmp(&(&b.local_date, &b.created_at, &b.id))
            });
            Ok(rows)
        }

        async fn list_by_ids(&self, ids: &[String]) -> Result<Vec<RoutineOccurrence>, RepoError> {
            self.calls.lock().unwrap().list_by_ids += 1;
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let id_set: HashSet<&str> = ids.iter().map(|id| id.as_str()).collect();
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| id_set.contains(row.id.as_str()))
                .cloned()
                .collect())
        }

        async fn insert(&self, occurrence: NewRoutineOccurrence) -> Result<RoutineOccurrence, RepoError> {
            self.calls.lock().unwrap().insert += 1;
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

        async fn insert_many(
            &self,
            rows: Vec<NewRoutineOccurrence>,
        ) -> Result<(), RepoError> {
            self.calls.lock().unwrap().insert_many += 1;
            for row in rows {
                self.insert(row).await?;
            }
            Ok(())
        }

        async fn delete_by_ids(&self, ids: &[String]) -> Result<(), RepoError> {
            // Mirrors build_occurrence_delete_by_ids_sql: HARD, id-scoped,
            // empty input touches nothing. Agenda rows are the caller's.
            self.calls.lock().unwrap().delete_by_ids += 1;
            if ids.is_empty() {
                return Ok(());
            }
            let id_set: HashSet<&str> = ids.iter().map(|id| id.as_str()).collect();
            self.stored
                .lock()
                .unwrap()
                .retain(|row| !id_set.contains(row.id.as_str()));
            Ok(())
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

        async fn clear_event_ids(&self, id: &str) -> Result<(), RepoError> {
            if let Some(row) = self.stored.lock().unwrap().iter_mut().find(|row| row.id == id) {
                row.calendar_id = None;
                row.google_event_id = None;
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
    /// sort_order wins). It also holds `Arc`s of the task/occurrence/routine
    /// stores so the anchor query can join them like D1 does.
    struct FakeAgendaItemRepo {
        stored: Mutex<Vec<AgendaItem>>,
        tasks: Arc<Mutex<Vec<Task>>>,
        occurrences: Arc<Mutex<Vec<RoutineOccurrence>>>,
        routines: Arc<Mutex<Vec<Routine>>>,
        next_id: Mutex<u64>,
        calls: Mutex<CallCounters>,
    }

    impl FakeAgendaItemRepo {
        fn new(
            tasks: Arc<Mutex<Vec<Task>>>,
            occurrences: Arc<Mutex<Vec<RoutineOccurrence>>>,
            routines: Arc<Mutex<Vec<Routine>>>,
        ) -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                tasks,
                occurrences,
                routines,
                next_id: Mutex::new(1),
                calls: Mutex::new(CallCounters::default()),
            }
        }

        fn call_counts(&self) -> CallCounters {
            self.calls.lock().unwrap().clone()
        }

        fn reset_calls(&self) {
            *self.calls.lock().unwrap() = CallCounters::default();
        }
    }

    #[async_trait::async_trait(?Send)]
    impl AgendaItemRepo for FakeAgendaItemRepo {
        async fn list_by_user_and_date(
            &self,
            user_id: &str,
            local_date: &str,
        ) -> Result<Vec<AgendaItem>, RepoError> {
            self.calls.lock().unwrap().list_by_user_and_date += 1;
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

        async fn list_by_user_and_date_range(
            &self,
            user_id: &str,
            from: &str,
            to: &str,
        ) -> Result<Vec<AgendaItem>, RepoError> {
            self.calls.lock().unwrap().list_by_user_and_date_range += 1;
            let mut rows: Vec<AgendaItem> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                // `YYYY-MM-DD` sorts chronologically, so a string window is
                // the same inclusive range as the SQL comparison.
                .filter(|row| {
                    row.user_id == user_id
                        && row.local_date.as_str() >= from
                        && row.local_date.as_str() <= to
                })
                .cloned()
                .collect();
            rows.sort_by(|a, b| {
                (&a.local_date, a.sort_order, &a.created_at)
                    .cmp(&(&b.local_date, b.sort_order, &b.created_at))
            });
            Ok(rows)
        }

        async fn min_local_date(&self, user_id: &str) -> Result<Option<String>, RepoError> {
            self.calls.lock().unwrap().min_local_date += 1;
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id)
                .map(|row| row.local_date.as_str())
                .min()
                .map(str::to_string))
        }

        async fn oldest_pending_local_date_before(
            &self,
            user_id: &str,
            before: &str,
        ) -> Result<Option<String>, RepoError> {
            self.calls.lock().unwrap().oldest_pending_local_date_before += 1;
            // The fake mirror of AGENDA_ITEM_OLDEST_PENDING_LOCAL_DATE_BEFORE_SQL:
            // a task row must exist, live, and be OPEN|PLANNED|IN_PROGRESS; a
            // occurrence must be pending|in_progress under a living routine.
            let tasks = self.tasks.lock().unwrap();
            let occurrences = self.occurrences.lock().unwrap();
            let routines = self.routines.lock().unwrap();
            let pending = |item: &AgendaItem| match item.kind.as_str() {
                AGENDA_KIND_TASK => tasks.iter().any(|task| {
                    task.id == item.ref_id
                        && task.user_id == item.user_id
                        && task.deleted_at.is_none()
                        && matches!(task.status.as_str(), "OPEN" | "PLANNED" | "IN_PROGRESS")
                }),
                AGENDA_KIND_OCCURRENCE => occurrences
                    .iter()
                    .find(|occ| occ.id == item.ref_id)
                    .map(|occ| {
                        occ.user_id == item.user_id
                            && matches!(
                                occ.status.as_str(),
                                OCCURRENCE_STATUS_PENDING | OCCURRENCE_STATUS_IN_PROGRESS
                            )
                            && routines
                                .iter()
                                .any(|routine| routine.id == occ.routine_id && routine.deleted_at.is_none())
                    })
                    .unwrap_or(false),
                _ => false,
            };
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|item| {
                    item.user_id == user_id
                        && item.local_date.as_str() < before
                        && pending(item)
                })
                .map(|item| item.local_date.as_str())
                .min()
                .map(str::to_string))
        }

        async fn get_by_id(&self, id: &str) -> Result<Option<AgendaItem>, RepoError> {
            self.calls.lock().unwrap().get_by_id += 1;
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

        async fn get_by_ref(
            &self,
            user_id: &str,
            kind: &str,
            ref_id: &str,
        ) -> Result<Option<AgendaItem>, RepoError> {
            self.calls.lock().unwrap().get_by_ref += 1;
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|row| {
                    row.user_id == user_id && row.kind == kind && row.ref_id == ref_id
                })
                .cloned())
        }

        async fn list_by_refs(
            &self,
            user_id: &str,
            kind: &str,
            ref_ids: &[String],
        ) -> Result<Vec<AgendaItem>, RepoError> {
            self.calls.lock().unwrap().list_by_refs += 1;
            if ref_ids.is_empty() {
                return Ok(Vec::new());
            }
            let ref_set: HashSet<&str> = ref_ids.iter().map(|id| id.as_str()).collect();
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| {
                    row.user_id == user_id && row.kind == kind && ref_set.contains(row.ref_id.as_str())
                })
                .cloned()
                .collect())
        }

        async fn insert(&self, item: NewAgendaItem) -> Result<AgendaItem, RepoError> {
            self.calls.lock().unwrap().insert += 1;
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

        async fn insert_many(&self, rows: Vec<NewAgendaItem>) -> Result<(), RepoError> {
            self.calls.lock().unwrap().insert_many += 1;
            for row in rows {
                self.insert(row).await?;
            }
            Ok(())
        }

        async fn hard_delete(&self, id: &str) -> Result<(), RepoError> {
            self.stored.lock().unwrap().retain(|row| row.id != id);
            Ok(())
        }

        async fn delete_by_ref(
            &self,
            user_id: &str,
            kind: &str,
            ref_id: &str,
        ) -> Result<(), RepoError> {
            // Mirrors AGENDA_ITEM_DELETE_BY_REF_SQL: EVERY matching row (never
            // `LIMIT 1`); no error when nothing matches.
            self.stored.lock().unwrap().retain(|row| {
                !(row.user_id == user_id && row.kind == kind && row.ref_id == ref_id)
            });
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
    /// `deleted_at`. `stored` is an `Arc` because `FakeAgendaItemRepo` joins
    /// it for the anchor predicate.
    struct FakeTaskRepo {
        stored: Arc<Mutex<Vec<Task>>>,
        next_id: Mutex<u64>,
    }

    impl FakeTaskRepo {
        fn new() -> Self {
            Self {
                stored: Arc::new(Mutex::new(Vec::new())),
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

        async fn list_user_ids_with_open_tasks(&self) -> Result<Vec<String>, RepoError> {
            // Mirrors TASK_LIST_USER_IDS_WITH_OPEN_SQL: distinct living owners
            // of a still-actionable task.
            let rows = self.stored.lock().unwrap();
            let mut ids: Vec<String> = rows
                .iter()
                .filter(|row| {
                    row.deleted_at.is_none()
                        && matches!(row.status.as_str(), "OPEN" | "PLANNED" | "IN_PROGRESS")
                })
                .map(|row| row.user_id.clone())
                .collect();
            ids.sort();
            ids.dedup();
            Ok(ids)
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
            url: &str,
            _token: &str,
        ) -> Result<(u16, Vec<u8>), HttpError> {
            for (substr, status, body) in &self.routes {
                if url.contains(substr.as_str()) {
                    return Ok((*status, body.clone().into_bytes()));
                }
            }
            Ok((200, br#"{"id":"unknown","etag":"e1"}"#.to_vec()))
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

    /// In-memory `CalendarRepo`: a fixed set of stored calendars, plus a set
    /// of user ids whose `list_by_user_id` fails (the agenda fill cron's
    /// per-user failure path — one broken user must not stop the rest).
    struct FakeCalendarRepo {
        stored: Mutex<Vec<GoogleCalendar>>,
        failing: Mutex<HashSet<String>>,
    }

    impl FakeCalendarRepo {
        fn with(calendars: Vec<GoogleCalendar>) -> Self {
            Self {
                stored: Mutex::new(calendars),
                failing: Mutex::new(HashSet::new()),
            }
        }

        /// Makes `list_by_user_id` fail for `user_id` — off by default, so it
        /// never disturbs the other tests.
        fn fail_for(&self, user_id: &str) {
            self.failing.lock().unwrap().insert(user_id.to_string());
        }
    }

    #[async_trait::async_trait(?Send)]
    impl CalendarRepo for FakeCalendarRepo {
        async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<GoogleCalendar>, RepoError> {
            if self.failing.lock().unwrap().contains(user_id) {
                return Err(RepoError::Backend("calendar lookup failed".to_string()));
            }
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

        async fn get_by_id_unfiltered(
            &self,
            _id: &str,
        ) -> Result<Option<GoogleCalendar>, RepoError> {
            Ok(None)
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

        async fn record_sync_attempt(
            &self,
            _id: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn record_sync_success(
            &self,
            _id: &str,
            _sync_token: &str,
            _query_fingerprint: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn record_sync_success_if_owner(
            &self,
            _id: &str,
            _sync_token: &str,
            _query_fingerprint: &str,
            _lease_owner: &str,
            _now_rfc3339: &str,
        ) -> Result<bool, RepoError> {
            Ok(true)
        }

        async fn record_sync_failure(
            &self,
            _id: &str,
            _error_code: &str,
            _sync_status: &str,
            _next_retry_rfc3339: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn record_sync_contention(
            &self,
            _id: &str,
            _error_code: &str,
            _sync_status: &str,
            _next_retry_rfc3339: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn begin_replica_reseed(
            &self,
            _id: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn try_acquire_lease(
            &self,
            _id: &str,
            _owner: &str,
            _now_rfc3339: &str,
            _expires_rfc3339: &str,
        ) -> Result<bool, RepoError> {
            Ok(true)
        }

        async fn release_lease(
            &self,
            _id: &str,
            _owner: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn renew_lease(
            &self,
            _id: &str,
            _owner: &str,
            _expires_rfc3339: &str,
            _now_rfc3339: &str,
        ) -> Result<bool, RepoError> {
            Ok(true)
        }

        async fn bump_dirty_requested(
            &self,
            _id: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn mark_dirty_applied(
            &self,
            _id: &str,
            _generation: i64,
            _now_rfc3339: &str,
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

        async fn set_event_labels(
            &self,
            id: &str,
            event_labels_json: &str,
            now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            // Persist into the in-memory rows (agenda never reads it back, but
            // the fake must mirror the D1 write).
            let mut stored = self.stored.lock().unwrap();
            if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
                cal.event_labels = event_labels_json.to_string();
                cal.event_labels_updated_at = Some(now_rfc3339.to_string());
            }
            Ok(())
        }

        async fn set_watch_coverage(
            &self,
            _id: &str,
            _coverage: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn set_event_coverage(
            &self,
            _id: &str,
            _coverage: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn delete(&self, _id: &str, _now_rfc3339: &str) -> Result<(), RepoError> {
            Ok(())
        }

        async fn get_calendar_list_sync_token(
            &self,
            _user_id: &str,
        ) -> Result<Option<String>, RepoError> {
            Ok(None)
        }

        async fn set_calendar_list_sync_token(
            &self,
            _user_id: &str,
            _token: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn list_user_ids_with_calendars(&self) -> Result<Vec<String>, RepoError> {
            Ok(Vec::new())
        }

        async fn clear_authorization_required_for_user(
            &self,
            _user_id: &str,
            _next_retry_rfc3339: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
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
                ical_uid: event.ical_uid.clone(),
                sequence: event.sequence,
                status: event.status.clone(),
                recurring_event_id: event.recurring_event_id.clone(),
                original_start: event.original_start.clone(),
                start_time_zone: event.start_time_zone.clone(),
                end_time_zone: event.end_time_zone.clone(),
                is_all_day: event.is_all_day,
                raw_json: event.raw_json.clone(),
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

        async fn upsert_batch_if_owner(
            &self,
            _events: Vec<NewCalendarEvent>,
            _lease_owner: &str,
            _now_rfc3339: &str,
        ) -> Result<bool, RepoError> {
            Ok(true)
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

        async fn reassign_calendar(
            &self,
            id: &str,
            new_calendar_id: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            let mut stored = self.stored.lock().unwrap();
            if let Some(event) = stored
                .iter_mut()
                .find(|event| event.deleted_at.is_none() && event.id == id)
            {
                event.calendar_id = new_calendar_id.to_string();
            }
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

        async fn delete_by_google_event_id_if_owner(
            &self,
            _calendar_id: &str,
            _google_event_id: &str,
            _lease_owner: &str,
            _now_rfc3339: &str,
        ) -> Result<bool, RepoError> {
            Ok(true)
        }

        async fn delete_stale(
            &self,
            _calendar_id: &str,
            _older_than_rfc3339: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn record_replica_seen(
            &self,
            _calendar_id: &str,
            _run_id: &str,
            _google_event_ids: Vec<String>,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn clear_replica_seen_for_calendar(
            &self,
            _calendar_id: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn sweep_absent_if_owner(
            &self,
            _calendar_id: &str,
            _run_id: &str,
            _lease_owner: &str,
            _now_rfc3339: &str,
        ) -> Result<bool, RepoError> {
            Ok(true)
        }

        async fn upsert_quarantine(
            &self,
            _calendar_id: &str,
            _google_event_id: &str,
            _phase: &str,
            _error_class: &str,
            _replay_payload: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }

        async fn clear_quarantine_for_calendar(
            &self,
            _calendar_id: &str,
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
        calendars: FakeCalendarRepo,
    }

    fn repos() -> Repos {
        // The agenda fake joins the other three stores (the anchor predicate),
        // so they must share the SAME `Arc`s, not copies.
        let routines = FakeRoutineRepo::new();
        let occurrences = FakeOccurrenceRepo::new();
        let tasks = FakeTaskRepo::new();
        let agenda = FakeAgendaItemRepo::new(
            tasks.stored.clone(),
            occurrences.stored.clone(),
            routines.stored.clone(),
        );
        Repos {
            lists: FakeTaskListRepo::new(),
            categories: FakeTaskCategoryRepo::new(),
            routines,
            occurrences,
            agenda,
            tasks,
            // The fake primary calendar is UTC (ADR 0004 amendment: the
            // primary calendar's time_zone decides today) — tests that need
            // another zone swap `repos.calendars` for their own fixture.
            calendars: FakeCalendarRepo::with(vec![calendar("primary@example.com", true)]),
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

    /// Pushes a membership row straight into the fake store — the tests that
    /// place a slot without going through a service call use this.
    fn agenda_row(
        repos: &Repos,
        id: &str,
        user_id: &str,
        date: &str,
        kind: &str,
        ref_id: &str,
        sort_order: i64,
    ) {
        repos.agenda.stored.lock().unwrap().push(AgendaItem {
            id: id.to_string(),
            user_id: user_id.to_string(),
            local_date: date.to_string(),
            kind: kind.to_string(),
            ref_id: ref_id.to_string(),
            sort_order,
            created_at: "2026-08-23T00:00:00Z".to_string(),
            updated_at: "2026-08-23T00:00:00Z".to_string(),
        });
    }

    /// `GET /api/agenda` with an explicit inclusive window.
    fn get_range(
        repos: &Repos,
        user_id: &str,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<AgendaResponse, AgendaError> {
        pollster::block_on(get_agenda(
            &repos.calendars,
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &repos.agenda,
            &repos.tasks,
            user_id,
            from,
            to,
            NOW_UNIX,
            None,
        ))
    }

    /// A single-day read: the same date on both ends of the window.
    fn get(repos: &Repos, user_id: &str, date: &str) -> Result<AgendaResponse, AgendaError> {
        get_range(repos, user_id, Some(date), Some(date))
    }

    /// `GET /api/agenda` with both bounds **omitted** — the server resolves
    /// the anchor week (the fake primary calendar is UTC, so `NOW_UNIX` is
    /// `2026-08-23`).
    fn get_default(repos: &Repos, user_id: &str) -> Result<AgendaResponse, AgendaError> {
        get_range(repos, user_id, None, None)
    }

    /// Test-only stand-in for the retired GET seed: ensures this date's
    /// occurrences for every living routine the rule covers and appends the
    /// missing membership rows. Tests for the move/reschedule/delete verbs use
    /// it to set up rows — a GET never writes any more.
    fn materialize_date(repos: &Repos, date: &str) {
        let user_id = "u-1";
        let routines = pollster::block_on(repos.routines.list_by_user_id(user_id)).unwrap();
        let covering: Vec<Routine> = routines
            .into_iter()
            .filter(|routine| {
                occurrence_dates(&routine.rrule, date, date)
                    .map(|dates| dates.contains(&date.to_string()))
                    .unwrap_or(false)
            })
            .collect();
        if covering.is_empty() {
            return;
        }

        let existing = pollster::block_on(repos.occurrences.list_by_user_and_date(user_id, date))
            .unwrap();
        let mut occurrence_ids = Vec::with_capacity(covering.len());
        for routine in &covering {
            let occurrence = match existing.iter().find(|occ| occ.routine_id == routine.id) {
                Some(occurrence) => occurrence.clone(),
                None => pollster::block_on(repos.occurrences.insert(NewRoutineOccurrence {
                    routine_id: routine.id.clone(),
                    user_id: user_id.to_string(),
                    local_date: date.to_string(),
                }))
                .unwrap(),
            };
            occurrence_ids.push(occurrence.id);
        }

        let memberships = pollster::block_on(repos.agenda.list_by_refs(
            user_id,
            AGENDA_KIND_OCCURRENCE,
            &occurrence_ids,
        ))
        .unwrap();
        let membership_refs: HashSet<&str> =
            memberships.iter().map(|item| item.ref_id.as_str()).collect();
        let mut rank = pollster::block_on(repos.agenda.max_sort_order(user_id, date))
            .unwrap()
            .map(|max| max + 1)
            .unwrap_or(0);
        for ref_id in occurrence_ids {
            if membership_refs.contains(ref_id.as_str()) {
                continue;
            }
            pollster::block_on(repos.agenda.insert(NewAgendaItem {
                user_id: user_id.to_string(),
                local_date: date.to_string(),
                kind: AGENDA_KIND_OCCURRENCE.to_string(),
                ref_id,
                sort_order: rank,
            }))
            .unwrap();
            rank += 1;
        }
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
        reschedule_at(repos, user_id, id, date, None)
    }

    /// The same reschedule with a named landing rank.
    fn reschedule_at(
        repos: &Repos,
        user_id: &str,
        id: &str,
        date: &str,
        sort_order: Option<i64>,
    ) -> Result<AgendaItemResponse, AgendaError> {
        pollster::block_on(reschedule_agenda_item(
            &repos.lists,
            &repos.categories,
            &repos.agenda,
            &repos.tasks,
            &repos.occurrences,
            &repos.routines,
            &repos.calendars,
            NOW_UNIX,
            user_id,
            id,
            date,
            sort_order,
            None,
        ))
    }

    /// `POST /api/agenda/days/:date/move-to-today`.
    fn move_day(
        repos: &Repos,
        user_id: &str,
        date: &str,
    ) -> Result<MoveDayToTodayResponse, AgendaError> {
        pollster::block_on(move_day_to_today(
            &repos.calendars,
            &repos.lists,
            &repos.categories,
            &repos.agenda,
            &repos.tasks,
            &repos.occurrences,
            &repos.routines,
            user_id,
            date,
            NOW_UNIX,
            None,
        ))
    }

    fn complete(repos: &Repos, user_id: &str, id: &str) -> Result<OccurrenceResponse, AgendaError> {
        pollster::block_on(complete_occurrence(
            None, None, None, None, None,
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
            None, None, None, None, None,
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            user_id,
            id,
            NOW_UNIX,
        ))
    }

    fn reopen(repos: &Repos, user_id: &str, id: &str) -> Result<OccurrenceResponse, AgendaError> {
        pollster::block_on(reopen_occurrence(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            user_id,
            id,
        ))
    }

    fn pause(repos: &Repos, user_id: &str, id: &str) -> Result<OccurrenceResponse, AgendaError> {
        pollster::block_on(pause_occurrence(
            None, None, None, None, None,
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
            None, None, None, None, None,
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

    /// `2026-08-23T10:00:00Z` — its civil date is `2026-08-23` in UTC (the
    /// fake primary calendar's zone) and in Asia/Kolkata, so occurrence
    /// `local_date` fixtures of `2026-08-23` pass the start's today-only
    /// gate against the default fixtures.
    const NOW_UNIX: i64 = 1_787_479_200;

    fn access() -> GoogleAccess {
        GoogleAccess {
            access_token: "at-1".to_string(),
            token_type: "Bearer".to_string(),
        }
    }


    fn ops() -> crate::calendar::FakeOperationRepo {
        crate::calendar::FakeOperationRepo::new()
    }


    /// All 24 event-label hexes as a cached `event_labels` JSON array with
    /// stable fake ids (`label-0` … `label-23`) — the fixture's default
    /// cache, so colored starts resolve their label id locally (create_event
    /// never fetches).
    fn event_labels_json() -> String {
        let labels: Vec<serde_json::Value> = crate::GOOGLE_EVENT_LABEL_COLORS
            .iter()
            .enumerate()
            .map(|(index, hex)| {
                serde_json::json!({
                    "id": format!("label-{index}"),
                    "backgroundColor": hex,
                })
            })
            .collect();
        serde_json::to_string(&labels).expect("event labels serialize")
    }

    /// A writable primary calendar for `u-1` — the default start target. Its
    /// `time_zone` is UTC, so `NOW_UNIX` reads as `2026-08-23` (ADR 0004
    /// amendment: the primary calendar's zone decides civil today).
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
            // The 24 seeded labels with stable fake ids — agenda never syncs,
            // so this is the cache `create_event` resolves colors against.
            event_labels: event_labels_json(),
            event_labels_updated_at: Some("2026-08-17T00:00:00Z".to_string()),
            sync_query_fingerprint: String::new(),
            sync_status: String::new(),
            initial_sync_complete: false,
            last_attempt_at: None,
            last_success_at: None,
            last_error_code: String::new(),
            failure_streak: 0,
            next_retry_at: None,
            dirty_requested_generation: 0,
            dirty_applied_generation: 0,
            full_sync_requested: false,
            lease_owner: String::new(),
            lease_expires_at: None,
            cache_revision: 0,
            projection: "timed_masters_and_exceptions".to_string(),
            watch_coverage: String::new(),
            event_coverage: String::new(),
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
            google_etag: "e1".to_string(),
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
            ical_uid: String::new(),
            sequence: 0,
            status: String::new(),
            recurring_event_id: String::new(),
            original_start: String::new(),
            start_time_zone: String::new(),
            end_time_zone: String::new(),
            is_all_day: false,
            raw_json: String::new(),
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
    /// the occurrence id. The occurrence's agenda item sits on 2026-08-23 —
    /// the civil date of `NOW_UNIX` — so the start gate (agenda membership on
    /// civil today) passes.
    fn seed_occurrence(repos: &Repos, status: &str) -> String {
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        let mut occurrence =
            FakeOccurrenceRepo::row("occ-1", "rt-1", "u-1", "2026-08-23", status);
        occurrence.title = Some("Fajr Qadha".to_string());
        repos.occurrences.stored.lock().unwrap().push(occurrence);
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
            &events, &ops(),
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &repos.agenda,
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
    fn get_agenda_rejects_invalid_dates() {
        let repos = repos();
        for bad in [
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
    fn get_agenda_without_bounds_reads_the_anchor_week() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));

        // Both bounds omitted → the anchor week (2026-08-23 in the UTC
        // primary calendar at NOW_UNIX). Nothing was materialized: a GET is a
        // pure read, so the window is empty and the anchor falls back to today.
        let response = get_default(&repos, "u-1").unwrap();
        assert!(
            response.items.is_empty(),
            "a read never materializes occurrences"
        );
        assert_eq!(response.today, "2026-08-23");
        assert_eq!(response.time_zone, "UTC");
        assert_eq!(response.anchor_date, "2026-08-23");
        assert_eq!(response.from, "2026-08-23");
        assert_eq!(response.to, "2026-08-29", "7 inclusive days");
        assert_eq!(response.earliest_date, "2026-08-23", "no rows → today");
        assert_eq!(repos.occurrences.stored.lock().unwrap().len(), 0);
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), 0);

        // Blank/whitespace bounds count as omitted.
        let blank_with_from = get_range(&repos, "u-1", Some("   "), Some("")).unwrap();
        assert_eq!(blank_with_from.from, "2026-08-23");
        let blank_with_to = get_range(&repos, "u-1", Some(""), Some("  ")).unwrap();
        assert_eq!(blank_with_to.to, "2026-08-29");
    }

    #[test]
    fn get_agenda_today_follows_the_primary_calendar_tz_only() {
        let mut repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        // Late UTC evening: New York is still on the 23rd (18:30 EDT), Kolkata
        // is already on the 24th (04:00+05:30). "Travel = home base" — the
        // PRIMARY calendar's zone wins, and a named calendar's zone never
        // moves today.
        let evening = rfc3339_to_unix_secs("2026-08-23T22:30:00Z").unwrap();
        repos.calendars = FakeCalendarRepo::with(vec![
            GoogleCalendar {
                time_zone: "Asia/Kolkata".to_string(),
                ..calendar("named@example.com", false)
            },
            GoogleCalendar {
                time_zone: "America/New_York".to_string(),
                ..calendar("primary@example.com", true)
            },
        ]);

        let response = pollster::block_on(get_agenda(
            &repos.calendars,
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &repos.agenda,
            &repos.tasks,
            "u-1",
            None,
            None,
            evening,
            None,
        ))
        .unwrap();
        assert_eq!(response.today, "2026-08-23", "New York, not Kolkata");
        assert_eq!(response.time_zone, "America/New_York");

        // No primary calendar at all → UTC fallback.
        repos.calendars = FakeCalendarRepo::with(vec![calendar("named@example.com", false)]);
        let fallback = pollster::block_on(get_agenda(
            &repos.calendars,
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &repos.agenda,
            &repos.tasks,
            "u-1",
            None,
            None,
            evening,
            None,
        ))
        .unwrap();
        assert_eq!(fallback.today, "2026-08-23", "UTC fallback");
        assert_eq!(fallback.time_zone, "UTC");
    }

    #[test]
    fn get_agenda_range_read_is_write_free_even_when_a_daily_routine_covers_it() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));

        let response = get_range(&repos, "u-1", Some("2026-08-23"), Some("2026-08-29")).unwrap();
        assert!(response.items.is_empty(), "no rows were ever written");
        assert_eq!(response.from, "2026-08-23");
        assert_eq!(response.to, "2026-08-29");
        assert_eq!(
            repos.occurrences.stored.lock().unwrap().len(),
            0,
            "a GET never ensures occurrences, even for a covering daily rule"
        );
        assert_eq!(
            repos.agenda.stored.lock().unwrap().len(),
            0,
            "a GET never appends membership rows"
        );
        assert_eq!(response.earliest_date, "2026-08-23", "no rows → today");
    }

    #[test]
    fn get_agenda_range_returns_items_ordered_by_date_then_rank() {
        let repos = repos();
        repos.tasks.push(task_row("t-1", "u-1", "One", "OPEN"));
        repos.tasks.push(task_row("t-2", "u-1", "Two", "OPEN"));
        repos.tasks.push(task_row("t-3", "u-1", "Three", "OPEN"));
        let push_item = |id: &str, date: &str, ref_id: &str, sort_order: i64| {
            repos.agenda.stored.lock().unwrap().push(AgendaItem {
                id: id.to_string(),
                user_id: "u-1".to_string(),
                local_date: date.to_string(),
                kind: AGENDA_KIND_TASK.to_string(),
                ref_id: ref_id.to_string(),
                sort_order,
                created_at: "2026-08-23T00:00:00Z".to_string(),
                updated_at: "2026-08-23T00:00:00Z".to_string(),
            });
        };
        // Inserted out of order on purpose.
        push_item("ai-1", "2026-08-24", "t-2", 0);
        push_item("ai-2", "2026-08-23", "t-1", 1);
        push_item("ai-3", "2026-08-23", "t-3", 0);

        let response = get_range(&repos, "u-1", Some("2026-08-23"), Some("2026-08-24")).unwrap();
        let order: Vec<(&str, i64)> = response
            .items
            .iter()
            .map(|item| (item.ref_id.as_str(), item.sort_order))
            .collect();
        assert_eq!(
            order,
            vec![("t-3", 0), ("t-1", 1), ("t-2", 0)],
            "(local_date, sort_order) order across the window"
        );
        assert_eq!(response.from, "2026-08-23");
        assert_eq!(response.to, "2026-08-24");
        assert_eq!(
            response.earliest_date, "2026-08-23",
            "the min row date, not the window start"
        );
    }

    #[test]
    fn get_agenda_anchor_is_the_oldest_past_pending_date() {
        let repos = repos();
        // A living routine and a deleted one — the anchor must only count
        // pending work under a living routine.
        repos.routines.stored.lock().unwrap().extend([
            FakeRoutineRepo::row("rt-live", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"),
            FakeRoutineRepo::row("rt-gone", "u-1", "Salat", 1, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"),
        ]);
        repos.routines.stored.lock().unwrap()[1].deleted_at =
            Some("2026-08-20T00:00:00Z".to_string());
        repos.tasks.push(task_row("t-pending", "u-1", "Pending | Work", "OPEN"));
        repos.tasks.push(task_row("t-done", "u-1", "Done | Work", "COMPLETED"));
        repos.tasks.push(task_row("t-disc", "u-1", "Disc | Work", "DISCARDED"));
        repos.tasks.push(task_row("t-today", "u-1", "Today | Work", "PLANNED"));
        for (id, routine_id, date, status) in [
            ("occ-done", "rt-live", "2026-08-19", OCCURRENCE_STATUS_DONE),
            ("occ-skipped", "rt-live", "2026-08-21", OCCURRENCE_STATUS_SKIPPED),
            ("occ-today", "rt-live", "2026-08-23", OCCURRENCE_STATUS_PENDING),
            ("occ-gone", "rt-gone", "2026-08-18", OCCURRENCE_STATUS_PENDING),
            ("occ-gone-running", "rt-gone", "2026-08-22", OCCURRENCE_STATUS_IN_PROGRESS),
        ] {
            repos
                .occurrences
                .stored
                .lock()
                .unwrap()
                .push(FakeOccurrenceRepo::row(id, routine_id, "u-1", date, status));
        }
        let push_item = |id: &str, date: &str, kind: &str, ref_id: &str| {
            repos.agenda.stored.lock().unwrap().push(AgendaItem {
                id: id.to_string(),
                user_id: "u-1".to_string(),
                local_date: date.to_string(),
                kind: kind.to_string(),
                ref_id: ref_id.to_string(),
                sort_order: 0,
                created_at: "2026-08-23T00:00:00Z".to_string(),
                updated_at: "2026-08-23T00:00:00Z".to_string(),
            });
        };
        // Never pending: an orphan task-kind row.
        push_item("ai-orphan", "2026-08-18", AGENDA_KIND_TASK, "t-missing");
        push_item("ai-done", "2026-08-19", AGENDA_KIND_OCCURRENCE, "occ-done");
        // The oldest past pending row.
        push_item("ai-pending", "2026-08-20", AGENDA_KIND_TASK, "t-pending");
        push_item("ai-skipped", "2026-08-21", AGENDA_KIND_OCCURRENCE, "occ-skipped");
        push_item("ai-completed", "2026-08-22", AGENDA_KIND_TASK, "t-done");
        push_item("ai-discarded", "2026-08-22", AGENDA_KIND_TASK, "t-disc");
        // A deleted routine's pending/in_progress leftovers never count.
        push_item("ai-gone", "2026-08-18", AGENDA_KIND_OCCURRENCE, "occ-gone");
        push_item("ai-gone-running", "2026-08-22", AGENDA_KIND_OCCURRENCE, "occ-gone-running");
        // Today never pulls the anchor into the past.
        push_item("ai-today", "2026-08-23", AGENDA_KIND_OCCURRENCE, "occ-today");
        push_item("ai-today-task", "2026-08-23", AGENDA_KIND_TASK, "t-today");

        // Both bounds omitted → the window starts at the anchor.
        let response = get_default(&repos, "u-1").unwrap();
        assert_eq!(response.anchor_date, "2026-08-20");
        assert_eq!(response.from, "2026-08-20");
        assert_eq!(response.to, "2026-08-26", "anchor + 6 civil days");
        assert_eq!(response.earliest_date, "2026-08-18");
    }

    #[test]
    fn get_agenda_anchor_falls_back_to_today_without_past_pending() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        repos
            .occurrences
            .stored
            .lock()
            .unwrap()
            .push(FakeOccurrenceRepo::row("occ-today", "rt-1", "u-1", "2026-08-23", OCCURRENCE_STATUS_PENDING));
        repos.agenda.stored.lock().unwrap().push(AgendaItem {
            id: "ai-today".to_string(),
            user_id: "u-1".to_string(),
            local_date: "2026-08-23".to_string(),
            kind: AGENDA_KIND_OCCURRENCE.to_string(),
            ref_id: "occ-today".to_string(),
            sort_order: 0,
            created_at: "2026-08-23T00:00:00Z".to_string(),
            updated_at: "2026-08-23T00:00:00Z".to_string(),
        });

        // A pending item ON today is not "past" — the anchor stays today.
        let response = get_default(&repos, "u-1").unwrap();
        assert_eq!(response.anchor_date, "2026-08-23");
        assert_eq!(response.from, "2026-08-23");
        assert_eq!(response.items.len(), 1, "today's pending item is in the window");
    }

    #[test]
    fn get_agenda_earliest_date_is_the_min_row_date_even_outside_the_window() {
        // No rows at all → today.
        let empty = get_default(&repos(), "u-1").unwrap();
        assert_eq!(empty.earliest_date, "2026-08-23");

        let repos = repos();
        // An orphan row (no living task) on an old date and a future row: the
        // floor binds to MIN(local_date) over every row, not to the page.
        repos.agenda.stored.lock().unwrap().extend([
            AgendaItem {
                id: "ai-old".to_string(),
                user_id: "u-1".to_string(),
                local_date: "2026-07-01".to_string(),
                kind: AGENDA_KIND_TASK.to_string(),
                ref_id: "t-missing".to_string(),
                sort_order: 0,
                created_at: "2026-07-01T00:00:00Z".to_string(),
                updated_at: "2026-07-01T00:00:00Z".to_string(),
            },
            AgendaItem {
                id: "ai-future".to_string(),
                user_id: "u-1".to_string(),
                local_date: "2026-09-30".to_string(),
                kind: AGENDA_KIND_TASK.to_string(),
                ref_id: "t-missing-too".to_string(),
                sort_order: 0,
                created_at: "2026-09-30T00:00:00Z".to_string(),
                updated_at: "2026-09-30T00:00:00Z".to_string(),
            },
        ]);

        let response = get_range(&repos, "u-1", Some("2026-08-23"), Some("2026-08-24")).unwrap();
        assert!(response.items.is_empty(), "both rows are outside the window");
        assert_eq!(
            response.earliest_date, "2026-07-01",
            "the floor ignores the requested window"
        );
    }

    #[test]
    fn get_agenda_bounds_reject_overlong_reversed_single_sided_and_garbage() {
        let repos = repos();

        // 32 inclusive days (2026-08-01 … 2026-09-01) is one over the cap.
        let err = get_range(&repos, "u-1", Some("2026-08-01"), Some("2026-09-01")).unwrap_err();
        assert!(
            matches!(err, AgendaError::Invalid(ref m) if m == "date range must not exceed 31 days"),
            "{err:?}"
        );
        // 31 inclusive days is valid.
        let ok = get_range(&repos, "u-1", Some("2026-08-01"), Some("2026-08-31")).unwrap();
        assert_eq!(ok.from, "2026-08-01");
        assert_eq!(ok.to, "2026-08-31");

        let err = get_range(&repos, "u-1", Some("2026-08-24"), Some("2026-08-23")).unwrap_err();
        assert!(
            matches!(err, AgendaError::Invalid(ref m) if m == "to must not be before from"),
            "{err:?}"
        );

        for (from, to) in [
            (Some("2026-08-23"), None),
            (None, Some("2026-08-23")),
        ] {
            let err = get_range(&repos, "u-1", from, to).unwrap_err();
            assert!(
                matches!(err, AgendaError::Invalid(ref m) if m == "from and to must both be set or both be omitted"),
                "{err:?} for {from:?}/{to:?}"
            );
        }

        for (from, to) in [
            (Some("not a date"), Some("2026-08-23")),
            (Some("2026-08-23"), Some("2026-13-01")),
        ] {
            let err = get_range(&repos, "u-1", from, to).unwrap_err();
            assert!(
                matches!(err, AgendaError::Invalid(ref m) if m == "date must be YYYY-MM-DD"),
                "{err:?} for {from:?}/{to:?}"
            );
        }
    }

    #[test]
    fn deleted_routine_history_renders_done_and_skipped_only() {
        let repos = repos();
        // A living routine with a pending occurrence today …
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-live", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        // … and a soft-deleted one whose record must still lend its display
        // fields to its done/skipped past.
        let mut gone = FakeRoutineRepo::row("rt-gone", "u-1", "Fajr Qadha", 1, "DTSTART:20260101T054500\nRRULE:FREQ=DAILY");
        gone.estimated_minutes = 30;
        gone.deleted_at = Some("2026-08-20T00:00:00Z".to_string());
        repos.routines.stored.lock().unwrap().push(gone);
        for (id, routine_id, date, status) in [
            ("occ-live", "rt-live", "2026-08-23", OCCURRENCE_STATUS_PENDING),
            ("occ-done", "rt-gone", "2026-08-23", OCCURRENCE_STATUS_DONE),
            ("occ-skipped", "rt-gone", "2026-08-24", OCCURRENCE_STATUS_SKIPPED),
            ("occ-pending", "rt-gone", "2026-08-25", OCCURRENCE_STATUS_PENDING),
            ("occ-running", "rt-gone", "2026-08-26", OCCURRENCE_STATUS_IN_PROGRESS),
        ] {
            repos
                .occurrences
                .stored
                .lock()
                .unwrap()
                .push(FakeOccurrenceRepo::row(id, routine_id, "u-1", date, status));
        }
        let mut deleted_task = task_row("t-gone", "u-1", "Gone | Work", "OPEN");
        deleted_task.deleted_at = Some("2026-08-20T00:00:00Z".to_string());
        repos.tasks.push(deleted_task);
        let push_item = |id: &str, date: &str, kind: &str, ref_id: &str| {
            repos.agenda.stored.lock().unwrap().push(AgendaItem {
                id: id.to_string(),
                user_id: "u-1".to_string(),
                local_date: date.to_string(),
                kind: kind.to_string(),
                ref_id: ref_id.to_string(),
                sort_order: 0,
                created_at: "2026-08-23T00:00:00Z".to_string(),
                updated_at: "2026-08-23T00:00:00Z".to_string(),
            });
        };
        push_item("ai-live", "2026-08-23", AGENDA_KIND_OCCURRENCE, "occ-live");
        push_item("ai-done", "2026-08-23", AGENDA_KIND_OCCURRENCE, "occ-done");
        push_item("ai-skipped", "2026-08-24", AGENDA_KIND_OCCURRENCE, "occ-skipped");
        push_item("ai-pending", "2026-08-25", AGENDA_KIND_OCCURRENCE, "occ-pending");
        push_item("ai-running", "2026-08-26", AGENDA_KIND_OCCURRENCE, "occ-running");
        push_item("ai-task", "2026-08-27", AGENDA_KIND_TASK, "t-gone");

        let response = get_range(&repos, "u-1", Some("2026-08-23"), Some("2026-08-29")).unwrap();
        let ids: Vec<&str> = response.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["ai-live", "ai-done", "ai-skipped"],
            "pending/in_progress leftovers of a deleted routine and the soft-deleted task are omitted"
        );

        let live = response.items[0].occurrence.as_ref().unwrap();
        assert!(!live.routine_deleted);
        assert_eq!(live.resolved_title, "Fajr");

        for item in &response.items[1..] {
            let occurrence = item.occurrence.as_ref().expect("occurrence embedded");
            assert!(occurrence.routine_deleted, "the routine is soft-deleted");
            assert_eq!(
                occurrence.resolved_title, "Fajr Qadha",
                "the deleted routine still provides the title"
            );
            assert_eq!(occurrence.estimated_minutes, 30);
            assert_eq!(occurrence.rrule, "DTSTART:20260101T054500\nRRULE:FREQ=DAILY");
        }
        assert_eq!(
            repos.agenda.stored.lock().unwrap().len(),
            6,
            "every membership row stays in the store; a read deletes nothing"
        );
    }

    #[test]
    fn range_read_is_write_free_and_uses_batched_reads() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().extend([
            FakeRoutineRepo::row("rt-a", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"),
            FakeRoutineRepo::row("rt-b", "u-1", "Salat", 1, "DTSTART:20260101T120000\nRRULE:FREQ=DAILY"),
        ]);
        // The retired GET seed, now done up front by the test.
        materialize_date(&repos, "2026-08-23");
        assert_eq!(repos.occurrences.stored.lock().unwrap().len(), 2);
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), 2);

        // Reset the counters; the read must not write or re-read per row.
        repos.occurrences.reset_calls();
        repos.agenda.reset_calls();
        let response = get(&repos, "u-1", "2026-08-23").unwrap();
        assert_eq!(response.items.len(), 2);

        let occurrences = repos.occurrences.call_counts();
        assert_eq!(occurrences.insert, 0, "no per-row occurrence insert on a read");
        assert_eq!(occurrences.insert_many, 0, "no occurrence insert_many on a read");
        assert_eq!(occurrences.get_by_id, 0, "no per-row occurrence get_by_id on a read");
        assert_eq!(
            occurrences.list_by_user_and_date, 0,
            "the read never re-lists a single date"
        );
        assert_eq!(occurrences.list_by_ids, 1, "hydrate resolves all refs in one batch");

        let agenda = repos.agenda.call_counts();
        assert_eq!(agenda.insert, 0, "no per-row agenda insert on a read");
        assert_eq!(agenda.insert_many, 0, "no agenda insert_many on a read");
        assert_eq!(agenda.get_by_ref, 0, "no per-row get_by_ref on a read");
        assert_eq!(agenda.list_by_refs, 0, "no membership check on a read");
        assert_eq!(agenda.list_by_user_and_date, 0, "no single-date pile read");
        assert_eq!(agenda.list_by_user_and_date_range, 1, "one window read");
        assert_eq!(agenda.min_local_date, 1, "one earliest-date read");
        assert_eq!(
            agenda.oldest_pending_local_date_before, 1,
            "the anchor is one query"
        );
    }

    #[test]
    fn soft_deleted_routine_pending_leftover_is_omitted_and_the_read_does_not_write() {
        let repos = repos();
        let mut routine = FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY");
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
            "the read writes no new occurrence"
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

    // ──────────────────────────────────────────
    // POST /api/agenda/items/:id/move
    // ──────────────────────────────────────────

    #[test]
    fn move_reorders_within_the_date_pile_only() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().extend([
            FakeRoutineRepo::row("rt-a", "u-1", "A", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"),
            FakeRoutineRepo::row("rt-b", "u-1", "B", 1, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"),
            FakeRoutineRepo::row("rt-c", "u-1", "C", 2, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"),
        ]);
        // Two dates, same routines — a full pile on each (materialized by the
        // test: a GET never seeds any more).
        materialize_date(&repos, "2026-08-23");
        materialize_date(&repos, "2026-08-24");
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
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-a", "u-1", "A", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        materialize_date(&repos, "2026-08-23");
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

    /// A weekly-Monday routine (dtstart 2026-08-17, a Monday) + its
    /// materialized occurrence/item on 2026-08-17. Returns the agenda item id.
    fn seed_monday_occurrence(repos: &Repos) -> String {
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260817T053000\nRRULE:FREQ=WEEKLY;BYDAY=MO"));
        materialize_date(repos, "2026-08-17");
        let agenda = get(repos, "u-1", "2026-08-17").unwrap();
        assert_eq!(agenda.items.len(), 1, "Monday materialized");
        agenda.items[0].id.clone()
    }

    #[test]
    fn reschedule_weekly_occurrence_moves_item_only_and_monday_never_comes_back() {
        let repos = repos();
        let item_id = seed_monday_occurrence(&repos);
        let occ_id = pollster::block_on(repos.agenda.get_by_id(&item_id))
            .unwrap()
            .unwrap()
            .ref_id
            .clone();

        // Monday → a future Tuesday (today is 2026-08-23; the past is a 400).
        let response = reschedule(&repos, "u-1", &item_id, "2026-08-25").unwrap();
        assert_eq!(response.item.id, item_id, "same agenda row, relocated");
        assert_eq!(response.item.local_date, "2026-08-25");
        assert_eq!(response.item.sort_order, 0, "empty Tuesday pile appends at 0");
        let occurrence = response.item.occurrence.as_ref().expect("occurrence embedded");
        assert_eq!(occurrence.id, occ_id, "same occurrence id");
        assert_eq!(
            occurrence.local_date, "2026-08-17",
            "occurrence.local_date is the SERIES key (the rule date) — never rewritten"
        );
        assert_eq!(occurrence.status, OCCURRENCE_STATUS_PENDING);

        // The routine is untouched — no exdate anywhere, the blob is intact.
        let routine = pollster::block_on(repos.routines.get_by_id("rt-1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            routine.rrule, "DTSTART:20260817T053000\nRRULE:FREQ=WEEKLY;BYDAY=MO",
            "rrule never changes on a reschedule"
        );

        // Monday GET does NOT re-attach: the occurrence exists for the rule
        // date, but its agenda item now sits on Tuesday, and a read never
        // appends membership.
        let monday = get(&repos, "u-1", "2026-08-17").unwrap();
        assert_eq!(monday.items.len(), 0, "Monday stays empty");
        assert_eq!(repos.occurrences.stored.lock().unwrap().len(), 1, "no new occurrence");
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), 1, "no re-attached item");

        // Tuesday GET shows the SAME occurrence (the rule does not even cover
        // this Tuesday, so the moved row is the only one).
        let tuesday = get(&repos, "u-1", "2026-08-25").unwrap();
        assert_eq!(tuesday.items.len(), 1);
        assert_eq!(tuesday.items[0].id, item_id, "same agenda row");
        assert_eq!(
            tuesday.items[0].occurrence.as_ref().unwrap().id,
            occ_id,
            "same occurrence id"
        );

        // NEXT Monday (2026-08-24) gets a fresh instance of its own (the test
        // materializes it — a GET is a pure read) — the moved instance never
        // blocks the series.
        materialize_date(&repos, "2026-08-24");
        let next_monday = get(&repos, "u-1", "2026-08-24").unwrap();
        assert_eq!(next_monday.items.len(), 1, "next week's Monday has its own row");
        assert_ne!(
            next_monday.items[0].occurrence.as_ref().unwrap().id,
            occ_id,
            "a different occurrence instance"
        );
    }

    #[test]
    fn reschedule_daily_occurrence_moves_item_only_and_tomorrow_gets_two_rows() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        materialize_date(&repos, "2026-08-23");
        let today = get(&repos, "u-1", "2026-08-23").unwrap();
        assert_eq!(today.items.len(), 1);
        let item_id = today.items[0].id.clone();
        let occ_id = today.items[0].occurrence.as_ref().unwrap().id.clone();

        // Today → tomorrow: the agenda item moves; the occurrence keeps its
        // rule date.
        let response = reschedule(&repos, "u-1", &item_id, "2026-08-24").unwrap();
        assert_eq!(response.item.local_date, "2026-08-24");
        assert_eq!(response.item.id, item_id, "same agenda row");
        assert_eq!(
            response.item.occurrence.as_ref().unwrap().local_date,
            "2026-08-23",
            "occurrence.local_date untouched"
        );
        let routine = pollster::block_on(repos.routines.get_by_id("rt-1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            routine.rrule, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY",
            "no exdate is written"
        );

        // Tomorrow's own instance (the rule covers it and the UNIQUE key is
        // (routine_id, local_date) — a different day is a different slot) is
        // materialized by the test; the moved row stays too: TWO rows of the
        // same routine on tomorrow is ALLOWED.
        materialize_date(&repos, "2026-08-24");
        let tomorrow = get(&repos, "u-1", "2026-08-24").unwrap();
        assert_eq!(tomorrow.items.len(), 2, "moved instance + tomorrow's own");
        let moved = tomorrow
            .items
            .iter()
            .find(|item| item.id == item_id)
            .expect("the moved row is still there");
        assert_eq!(moved.occurrence.as_ref().unwrap().id, occ_id);
        let fresh = tomorrow
            .items
            .iter()
            .find(|item| item.id != item_id)
            .expect("a fresh instance for tomorrow");
        assert_eq!(fresh.occurrence.as_ref().unwrap().local_date, "2026-08-24");
        assert_eq!(repos.occurrences.stored.lock().unwrap().len(), 2, "two instances");
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), 2, "two items");

        // Today's GET: the occurrence exists (no duplicate seeded), but its
        // item lives on tomorrow — today stays empty and a read writes nothing.
        let today_after = get(&repos, "u-1", "2026-08-23").unwrap();
        assert_eq!(today_after.items.len(), 0, "today stays empty");
        assert_eq!(repos.occurrences.stored.lock().unwrap().len(), 2, "no new occurrence");
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), 2, "no re-attached item");
    }

    #[test]
    fn reschedule_allows_target_date_already_holding_the_routine() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        // Two dates already holding their own occurrence of the same routine.
        materialize_date(&repos, "2026-08-23");
        materialize_date(&repos, "2026-08-24");
        let day1 = get(&repos, "u-1", "2026-08-23").unwrap();
        let day2 = get(&repos, "u-1", "2026-08-24").unwrap();
        assert_eq!(day2.items.len(), 1);
        let day1_id = day1.items[0].id.clone();
        let day1_occ = day1.items[0].occurrence.as_ref().unwrap().id.clone();
        let day2_occ = day2.items[0].occurrence.as_ref().unwrap().id.clone();

        // Moving onto a day that already holds another occurrence of this
        // routine is ALLOWED — `occurrence.local_date` is the rule date, not
        // the plan, so it cannot be used to refuse the target.
        let response = reschedule(&repos, "u-1", &day1_id, "2026-08-24").unwrap();
        assert_eq!(response.item.id, day1_id);
        assert_eq!(response.item.local_date, "2026-08-24");
        assert_eq!(response.item.sort_order, 1, "appended after the existing row");
        assert_eq!(response.item.occurrence.as_ref().unwrap().id, day1_occ);

        // Two items on the 24th: day1's moved occurrence + day2's own.
        let day2_after = get(&repos, "u-1", "2026-08-24").unwrap();
        assert_eq!(day2_after.items.len(), 2);
        let occs: Vec<String> = day2_after
            .items
            .iter()
            .map(|item| item.occurrence.as_ref().unwrap().id.clone())
            .collect();
        assert_eq!(occs, vec![day2_occ, day1_occ.clone()], "both instances present");
        // The moved occurrence's rule date stays untouched.
        let occurrence = pollster::block_on(repos.occurrences.get_by_id(&day1_occ))
            .unwrap()
            .unwrap();
        assert_eq!(occurrence.local_date, "2026-08-23", "occurrence untouched");
    }

    #[test]
    fn reschedule_statuses_in_progress_done_are_400_skipped_becomes_pending() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        // Materialize once so the agenda rows exist, then rewrite the
        // occurrences' statuses to drive the matrix.
        materialize_date(&repos, "2026-08-23");
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
        assert_eq!(
            occurrence.local_date, "2026-08-23",
            "occurrence.local_date is never rewritten"
        );
        // The routine is untouched — no exdate is ever written.
        let routine = pollster::block_on(repos.routines.get_by_id("rt-1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            routine.rrule, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY",
            "rrule unchanged"
        );
    }

    #[test]
    fn reschedule_same_date_is_200_noop_unchanged() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        materialize_date(&repos, "2026-08-23");
        let agenda = get(&repos, "u-1", "2026-08-23").unwrap();
        let item_id = agenda.items[0].id.clone();
        let occ_id = agenda.items[0].occurrence.as_ref().unwrap().id.clone();

        let response = reschedule(&repos, "u-1", &item_id, "2026-08-23").unwrap();
        assert_eq!(response.item.id, item_id, "same item");
        assert_eq!(response.item.sort_order, 0, "rank untouched");
        assert_eq!(response.item.occurrence.as_ref().unwrap().id, occ_id);

        // Nothing written: no rrule change, no date/status/updated_at change.
        let routine = pollster::block_on(repos.routines.get_by_id("rt-1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            routine.rrule, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY",
            "same-date no-op never touches the routine"
        );
        let occurrence = pollster::block_on(repos.occurrences.get_by_id(&occ_id))
            .unwrap()
            .unwrap();
        assert_eq!(occurrence.local_date, "2026-08-23");
        assert_eq!(occurrence.updated_at, "2026-08-23T00:00:00Z", "row untouched");
        let stored = pollster::block_on(repos.agenda.get_by_id(&item_id))
            .unwrap()
            .unwrap();
        assert_eq!(stored.updated_at, "2026-08-23T00:00:00Z", "item untouched");

        // A named rank on a same-date call is ignored too: same-day placement
        // is `/move` (it shifts peers); a reschedule no-ops.
        let ranked = reschedule_at(&repos, "u-1", &item_id, "2026-08-23", Some(5)).unwrap();
        assert_eq!(ranked.item.id, item_id);
        assert_eq!(ranked.item.sort_order, 0, "rank untouched");
        let stored = pollster::block_on(repos.agenda.get_by_id(&item_id))
            .unwrap()
            .unwrap();
        assert_eq!(stored.sort_order, 0, "no peer is ever shifted");
        assert_eq!(stored.updated_at, "2026-08-23T00:00:00Z", "no-op writes nothing");
    }

    #[test]
    fn reschedule_task_moves_the_slot_only() {
        let repos = repos();
        repos.tasks.push(task_row("t-1", "u-1", "Review | Work", "OPEN"));
        agenda_row(&repos, "ai-1", "u-1", "2026-08-23", AGENDA_KIND_TASK, "t-1", 0);
        let item_id = "ai-1".to_string();

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
    fn reschedule_missing_other_user_and_soft_deleted_backings_are_404() {
        let repos = repos();
        repos.tasks.push(task_row("t-1", "u-1", "Review | Work", "OPEN"));
        agenda_row(&repos, "ai-1", "u-1", "2026-08-23", AGENDA_KIND_TASK, "t-1", 0);

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
        let mut routine = FakeRoutineRepo::row("rt-gone", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY");
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
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        materialize_date(&repos, "2026-08-23");
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
        assert_eq!(routine.rrule, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY");
        let stored = pollster::block_on(repos.agenda.get_by_id(&item_id))
            .unwrap()
            .unwrap();
        assert_eq!(stored.local_date, "2026-08-23");
    }

    #[test]
    fn reschedule_into_the_past_is_rejected_and_nothing_moves() {
        let repos = repos();
        // One task slot and one pending occurrence, both on today.
        repos.tasks.push(task_row("t-1", "u-1", "Review | Work", "OPEN"));
        agenda_row(&repos, "ai-task", "u-1", "2026-08-23", AGENDA_KIND_TASK, "t-1", 0);
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        repos.occurrences.stored.lock().unwrap().push(FakeOccurrenceRepo::row(
            "occ-1", "rt-1", "u-1", "2026-08-23", OCCURRENCE_STATUS_PENDING,
        ));
        agenda_row(&repos, "ai-occ", "u-1", "2026-08-23", AGENDA_KIND_OCCURRENCE, "occ-1", 1);

        for (id, rank) in [("ai-task", 0), ("ai-occ", 1)] {
            let err = reschedule(&repos, "u-1", id, "2026-08-22").unwrap_err();
            assert!(
                matches!(err, AgendaError::Invalid(ref m) if m == "cannot reschedule into the past"),
                "{err:?} for {id}"
            );
            let stored = pollster::block_on(repos.agenda.get_by_id(id)).unwrap().unwrap();
            assert_eq!(stored.local_date, "2026-08-23", "a rejected past move writes nothing");
            assert_eq!(stored.sort_order, rank, "rank untouched");
            assert_eq!(stored.updated_at, "2026-08-23T00:00:00Z", "row untouched");
        }
        let occurrence = pollster::block_on(repos.occurrences.get_by_id("occ-1")).unwrap().unwrap();
        assert_eq!(occurrence.status, OCCURRENCE_STATUS_PENDING);
        assert_eq!(get(&repos, "u-1", "2026-08-23").unwrap().items.len(), 2, "both slots stay");
        assert!(get(&repos, "u-1", "2026-08-22").unwrap().items.is_empty());
    }

    #[test]
    fn reschedule_sort_order_lands_at_the_rank_and_shifts_the_peer() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        materialize_date(&repos, "2026-08-23");
        materialize_date(&repos, "2026-08-24");
        let today = get(&repos, "u-1", "2026-08-23").unwrap();
        let tomorrow = get(&repos, "u-1", "2026-08-24").unwrap();
        let mover = today.items[0].id.clone();
        let peer = tomorrow.items[0].id.clone();

        // Rank 0 → the mover lands on top and tomorrow's peer shifts to 1.
        let response = reschedule_at(&repos, "u-1", &mover, "2026-08-24", Some(0)).unwrap();
        assert_eq!(response.item.id, mover, "same agenda row");
        assert_eq!(response.item.local_date, "2026-08-24");
        assert_eq!(response.item.sort_order, 0, "the named rank is the landing rank");
        let pile = |date: &str| -> Vec<(String, i64)> {
            pollster::block_on(repos.agenda.list_by_user_and_date("u-1", date))
                .unwrap()
                .into_iter()
                .map(|item| (item.id, item.sort_order))
                .collect()
        };
        assert_eq!(
            pile("2026-08-24"),
            vec![(mover.clone(), 0), (peer.clone(), 1)],
            "the peer at/after the rank shifts up one"
        );
        assert!(pile("2026-08-23").is_empty(), "the source pile is never reshuffled");

        // Absent sort_order still appends at max+1.
        repos.tasks.push(task_row("t-1", "u-1", "Review | Work", "OPEN"));
        agenda_row(&repos, "ai-task", "u-1", "2026-08-23", AGENDA_KIND_TASK, "t-1", 0);
        let appended = reschedule(&repos, "u-1", "ai-task", "2026-08-24").unwrap();
        assert_eq!(appended.item.sort_order, 2, "append = max+1");
        assert_eq!(
            pile("2026-08-24"),
            vec![(mover, 0), (peer, 1), ("ai-task".to_string(), 2)],
            "the appended row lands after the existing pile"
        );
    }

    #[test]
    fn reschedule_terminal_tasks_is_rejected() {
        let repos = repos();
        for (id, status) in [("t-done", "COMPLETED"), ("t-disc", "DISCARDED")] {
            repos.tasks.push(task_row(id, "u-1", "Review | Work", status));
            let item_id = format!("ai-{id}");
            agenda_row(&repos, &item_id, "u-1", "2026-08-23", AGENDA_KIND_TASK, id, 0);
            let err = reschedule(&repos, "u-1", &item_id, "2026-08-24").unwrap_err();
            assert!(
                matches!(err, AgendaError::Invalid(ref m) if m == "cannot reschedule a terminal task"),
                "{err:?} for {status}"
            );
            let stored = pollster::block_on(repos.agenda.get_by_id(&item_id)).unwrap().unwrap();
            assert_eq!(stored.local_date, "2026-08-23", "the slot stays put");
            let task = pollster::block_on(repos.tasks.get_by_id(id)).unwrap().unwrap();
            assert_eq!(task.status, status, "tasks.status is never touched");
        }
    }

    #[test]
    fn reschedule_rejects_a_negative_sort_order_on_a_real_move() {
        let repos = repos();
        repos.tasks.push(task_row("t-1", "u-1", "Review | Work", "OPEN"));
        agenda_row(&repos, "ai-1", "u-1", "2026-08-23", AGENDA_KIND_TASK, "t-1", 0);

        let err = reschedule_at(&repos, "u-1", "ai-1", "2026-08-24", Some(-1)).unwrap_err();
        assert!(
            matches!(err, AgendaError::Invalid(ref m) if m == "sort_order must be a non-negative integer"),
            "{err:?}"
        );
        let stored = pollster::block_on(repos.agenda.get_by_id("ai-1")).unwrap().unwrap();
        assert_eq!(stored.local_date, "2026-08-23", "a rejected rank writes nothing");
        assert_eq!(stored.sort_order, 0);
    }

    // ──────────────────────────────────────────
    // POST /api/agenda/days/:date/move-to-today
    // ──────────────────────────────────────────

    #[test]
    fn move_day_to_today_moves_the_still_pending_items_in_pile_order() {
        let repos = repos();
        // Today already holds an item: the swept pile is appended after it.
        repos.tasks.push(task_row("t-today", "u-1", "Today | Work", "OPEN"));
        agenda_row(&repos, "ai-today", "u-1", "2026-08-23", AGENDA_KIND_TASK, "t-today", 0);

        // Yesterday mixes every shape the sweep has to sort out.
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        let mut gone = FakeRoutineRepo::row("rt-gone", "u-1", "Gone", 1, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY");
        gone.deleted_at = Some("2026-08-20T00:00:00Z".to_string());
        repos.routines.stored.lock().unwrap().push(gone);

        repos.tasks.push(task_row("t-open", "u-1", "Open | Work", "OPEN"));
        repos.tasks.push(task_row("t-running", "u-1", "Running | Work", "IN_PROGRESS"));
        repos.tasks.push(task_row("t-planned", "u-1", "Planned | Work", "PLANNED"));
        repos.tasks.push(task_row("t-completed", "u-1", "Done | Work", "COMPLETED"));
        let mut t_gone = task_row("t-gone", "u-1", "Gone | Work", "OPEN");
        t_gone.deleted_at = Some("2026-08-20T00:00:00Z".to_string());
        repos.tasks.push(t_gone);

        repos.occurrences.stored.lock().unwrap().extend([
            FakeOccurrenceRepo::row("occ-pending", "rt-1", "u-1", "2026-08-22", OCCURRENCE_STATUS_PENDING),
            FakeOccurrenceRepo::row("occ-skipped", "rt-1", "u-1", "2026-08-22", OCCURRENCE_STATUS_SKIPPED),
            FakeOccurrenceRepo::row("occ-done", "rt-1", "u-1", "2026-08-22", OCCURRENCE_STATUS_DONE),
            FakeOccurrenceRepo::row("occ-running", "rt-1", "u-1", "2026-08-22", OCCURRENCE_STATUS_IN_PROGRESS),
            FakeOccurrenceRepo::row("occ-orphan", "rt-gone", "u-1", "2026-08-22", OCCURRENCE_STATUS_PENDING),
        ]);

        agenda_row(&repos, "ai-open", "u-1", "2026-08-22", AGENDA_KIND_TASK, "t-open", 0);
        agenda_row(&repos, "ai-pending", "u-1", "2026-08-22", AGENDA_KIND_OCCURRENCE, "occ-pending", 1);
        agenda_row(&repos, "ai-running", "u-1", "2026-08-22", AGENDA_KIND_TASK, "t-running", 2);
        agenda_row(&repos, "ai-done-task", "u-1", "2026-08-22", AGENDA_KIND_TASK, "t-completed", 3);
        agenda_row(&repos, "ai-skipped", "u-1", "2026-08-22", AGENDA_KIND_OCCURRENCE, "occ-skipped", 4);
        agenda_row(&repos, "ai-done-occ", "u-1", "2026-08-22", AGENDA_KIND_OCCURRENCE, "occ-done", 5);
        agenda_row(&repos, "ai-running-occ", "u-1", "2026-08-22", AGENDA_KIND_OCCURRENCE, "occ-running", 6);
        agenda_row(&repos, "ai-gone-task", "u-1", "2026-08-22", AGENDA_KIND_TASK, "t-gone", 7);
        agenda_row(&repos, "ai-orphan", "u-1", "2026-08-22", AGENDA_KIND_OCCURRENCE, "occ-orphan", 8);
        agenda_row(&repos, "ai-planned", "u-1", "2026-08-22", AGENDA_KIND_TASK, "t-planned", 9);
        agenda_row(&repos, "ai-weird", "u-1", "2026-08-22", "weird", "ref-x", 10);

        let response = move_day(&repos, "u-1", "2026-08-22").unwrap();
        let moved: Vec<(&str, &str)> = response
            .items
            .iter()
            .map(|item| (item.id.as_str(), item.local_date.as_str()))
            .collect();
        assert_eq!(
            moved,
            vec![
                ("ai-open", "2026-08-23"),
                ("ai-pending", "2026-08-23"),
                ("ai-running", "2026-08-23"),
                ("ai-planned", "2026-08-23"),
            ],
            "only the still-pending rows move, in their original order"
        );
        assert_eq!(
            response.items.iter().map(|item| item.sort_order).collect::<Vec<_>>(),
            vec![1, 2, 3, 4],
            "each row appends after today's existing item"
        );
        // An in-progress TASK moves; the occurrence keeps its rule date.
        assert_eq!(response.items[0].task.as_ref().unwrap().status, "OPEN");
        assert_eq!(response.items[2].task.as_ref().unwrap().status, "IN_PROGRESS");
        assert_eq!(response.items[3].task.as_ref().unwrap().status, "PLANNED");
        let moved_occurrence = response.items[1].occurrence.as_ref().unwrap();
        assert_eq!(moved_occurrence.id, "occ-pending");
        assert_eq!(
            moved_occurrence.local_date, "2026-08-22",
            "occurrence.local_date stays the rule date"
        );
        assert_eq!(moved_occurrence.status, OCCURRENCE_STATUS_PENDING, "pending stays pending");

        // Today: the existing item first, then the four swept rows in order.
        let today = get(&repos, "u-1", "2026-08-23").unwrap();
        let today_ids: Vec<&str> = today.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(
            today_ids,
            vec!["ai-today", "ai-open", "ai-pending", "ai-running", "ai-planned"]
        );

        // The past day keeps exactly the non-movable rows at their old ranks.
        let left: Vec<(String, i64)> =
            pollster::block_on(repos.agenda.list_by_user_and_date("u-1", "2026-08-22"))
                .unwrap()
                .into_iter()
                .map(|item| (item.id, item.sort_order))
                .collect();
        assert_eq!(
            left,
            vec![
                ("ai-done-task".to_string(), 3),
                ("ai-skipped".to_string(), 4),
                ("ai-done-occ".to_string(), 5),
                ("ai-running-occ".to_string(), 6),
                ("ai-gone-task".to_string(), 7),
                ("ai-orphan".to_string(), 8),
                ("ai-weird".to_string(), 10),
            ],
            "terminal, skipped, in_progress, soft-deleted and orphan rows stay put"
        );
        // Nothing was created or destroyed: 1 row on today + 11 on yesterday.
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), 12);
        assert_eq!(repos.occurrences.stored.lock().unwrap().len(), 5);
    }

    #[test]
    fn move_day_to_today_rejects_today_the_future_and_a_garbage_date() {
        let repos = repos();
        repos.tasks.push(task_row("t-1", "u-1", "Review | Work", "OPEN"));
        agenda_row(&repos, "ai-1", "u-1", "2026-08-23", AGENDA_KIND_TASK, "t-1", 0);

        let err = move_day(&repos, "u-1", "nope").unwrap_err();
        assert!(
            matches!(err, AgendaError::Invalid(ref m) if m == "date must be YYYY-MM-DD"),
            "{err:?}"
        );
        for date in ["2026-08-23", "2026-08-24"] {
            let err = move_day(&repos, "u-1", date).unwrap_err();
            assert!(
                matches!(err, AgendaError::Invalid(ref m) if m == "cannot move a day that is not in the past"),
                "{err:?} for {date}"
            );
        }
        let stored = pollster::block_on(repos.agenda.get_by_id("ai-1")).unwrap().unwrap();
        assert_eq!(stored.local_date, "2026-08-23", "nothing moved");
        assert_eq!(stored.sort_order, 0);
    }

    #[test]
    fn move_day_to_today_with_nothing_pending_is_an_empty_200() {
        let repos = repos();
        // The past day holds only terminal leftovers.
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        repos.occurrences.stored.lock().unwrap().push(FakeOccurrenceRepo::row(
            "occ-done", "rt-1", "u-1", "2026-08-22", OCCURRENCE_STATUS_DONE,
        ));
        agenda_row(&repos, "ai-done", "u-1", "2026-08-22", AGENDA_KIND_OCCURRENCE, "occ-done", 0);
        repos.tasks.push(task_row("t-done", "u-1", "Done | Work", "COMPLETED"));
        agenda_row(&repos, "ai-task", "u-1", "2026-08-22", AGENDA_KIND_TASK, "t-done", 1);

        let response = move_day(&repos, "u-1", "2026-08-22").unwrap();
        assert!(response.items.is_empty(), "nothing pending → nothing moved");
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), 2, "the store is unchanged");
        let left: Vec<(String, String)> =
            pollster::block_on(repos.agenda.list_by_user_and_date("u-1", "2026-08-22"))
                .unwrap()
                .into_iter()
                .map(|item| (item.id, item.local_date))
                .collect();
        assert_eq!(
            left,
            vec![
                ("ai-done".to_string(), "2026-08-22".to_string()),
                ("ai-task".to_string(), "2026-08-22".to_string()),
            ]
        );
    }

    // ──────────────────────────────────────────
    // complete / skip — the full verb matrix
    // ──────────────────────────────────────────

    #[test]
    fn complete_matrix_all_four_states() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
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
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
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

    #[test]
    fn reopen_matrix_all_four_states() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));

        // `pending` → 200 no-op: row untouched, ids kept even if set.
        let mut pending = FakeOccurrenceRepo::row("occ-pending", "rt-1", "u-1", "2026-08-23", "pending");
        pending.calendar_id = Some("cal-1".to_string());
        pending.google_event_id = Some("g-1".to_string());
        repos.occurrences.stored.lock().unwrap().push(pending);
        let response = reopen(&repos, "u-1", "occ-pending").unwrap();
        assert_eq!(response.occurrence.status, OCCURRENCE_STATUS_PENDING);
        let stored = pollster::block_on(repos.occurrences.get_by_id("occ-pending")).unwrap().unwrap();
        assert_eq!(stored.updated_at, "2026-08-23T01:00:00Z", "pending reopen is a no-op");
        assert_eq!(stored.calendar_id.as_deref(), Some("cal-1"), "ids untouched on a no-op");
        assert_eq!(stored.google_event_id.as_deref(), Some("g-1"), "ids untouched on a no-op");

        // `in_progress` → 400 with the exact locked message (no abort).
        repos.occurrences.stored.lock().unwrap().push(FakeOccurrenceRepo::row(
            "occ-in_progress", "rt-1", "u-1", "2026-08-23", "in_progress",
        ));
        let err = reopen(&repos, "u-1", "occ-in_progress").unwrap_err();
        assert!(
            matches!(err, AgendaError::Invalid(ref m) if m == "cannot reopen an in_progress occurrence"),
            "{err:?}"
        );
        let stored = pollster::block_on(repos.occurrences.get_by_id("occ-in_progress")).unwrap().unwrap();
        assert_eq!(stored.status, "in_progress", "reject wrote nothing");

        // `done` → `pending`.
        repos.occurrences.stored.lock().unwrap().push(FakeOccurrenceRepo::row(
            "occ-done", "rt-1", "u-1", "2026-08-23", "done",
        ));
        let response = reopen(&repos, "u-1", "occ-done").unwrap();
        assert_eq!(response.occurrence.status, OCCURRENCE_STATUS_PENDING);
        let stored = pollster::block_on(repos.occurrences.get_by_id("occ-done")).unwrap().unwrap();
        assert_eq!(stored.status, OCCURRENCE_STATUS_PENDING);
        assert_eq!(stored.calendar_id, None);
        assert_eq!(stored.google_event_id, None);
        assert_eq!(stored.updated_at, "2026-08-23T02:00:00Z");

        // `skipped` → `pending`.
        repos.occurrences.stored.lock().unwrap().push(FakeOccurrenceRepo::row(
            "occ-skipped", "rt-1", "u-1", "2026-08-23", "skipped",
        ));
        let response = reopen(&repos, "u-1", "occ-skipped").unwrap();
        assert_eq!(response.occurrence.status, OCCURRENCE_STATUS_PENDING);
        let stored = pollster::block_on(repos.occurrences.get_by_id("occ-skipped")).unwrap().unwrap();
        assert_eq!(stored.status, OCCURRENCE_STATUS_PENDING);
        assert_eq!(stored.calendar_id, None);
        assert_eq!(stored.google_event_id, None);
        assert_eq!(stored.updated_at, "2026-08-23T02:00:00Z");
    }

    #[test]
    fn reopen_done_with_stored_ids_clears_the_chip() {
        // The chip-clear contract: a done occurrence that carried a one-shot
        // log comes back to pending with BOTH ids None, so a later start
        // mints a NEW Google event (the closed one stays as an orphaned log).
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        let mut done = FakeOccurrenceRepo::row("occ-1", "rt-1", "u-1", "2026-08-23", "done");
        done.calendar_id = Some("cal-1".to_string());
        done.google_event_id = Some("g-1".to_string());
        repos.occurrences.stored.lock().unwrap().push(done);

        let response = reopen(&repos, "u-1", "occ-1").unwrap();
        assert_eq!(response.occurrence.status, OCCURRENCE_STATUS_PENDING);
        let stored = pollster::block_on(repos.occurrences.get_by_id("occ-1")).unwrap().unwrap();
        assert_eq!(stored.status, OCCURRENCE_STATUS_PENDING);
        assert_eq!(stored.calendar_id, None, "chip cleared on reopen");
        assert_eq!(stored.google_event_id, None, "chip cleared on reopen");
        assert_eq!(stored.updated_at, "2026-08-23T02:00:00Z");
    }

    #[test]
    fn pause_matrix_all_four_states() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));

        // `pending` → 200 no-op: row untouched, ids kept even if set.
        let mut pending = FakeOccurrenceRepo::row("occ-pending", "rt-1", "u-1", "2026-08-23", "pending");
        pending.calendar_id = Some("cal-1".to_string());
        pending.google_event_id = Some("g-1".to_string());
        repos.occurrences.stored.lock().unwrap().push(pending);
        let response = pause(&repos, "u-1", "occ-pending").unwrap();
        assert_eq!(response.occurrence.status, OCCURRENCE_STATUS_PENDING);
        let stored = pollster::block_on(repos.occurrences.get_by_id("occ-pending")).unwrap().unwrap();
        assert_eq!(stored.updated_at, "2026-08-23T01:00:00Z", "pending pause is a no-op");
        assert_eq!(stored.calendar_id.as_deref(), Some("cal-1"), "ids untouched on a no-op");
        assert_eq!(stored.google_event_id.as_deref(), Some("g-1"), "ids untouched on a no-op");

        // `in_progress` → `pending` + ids cleared (session-only here).
        let mut running = FakeOccurrenceRepo::row("occ-in_progress", "rt-1", "u-1", "2026-08-23", "in_progress");
        running.calendar_id = Some("cal-1".to_string());
        running.google_event_id = Some("g-1".to_string());
        repos.occurrences.stored.lock().unwrap().push(running);
        let response = pause(&repos, "u-1", "occ-in_progress").unwrap();
        assert_eq!(response.occurrence.status, OCCURRENCE_STATUS_PENDING);
        let stored = pollster::block_on(repos.occurrences.get_by_id("occ-in_progress")).unwrap().unwrap();
        assert_eq!(stored.status, OCCURRENCE_STATUS_PENDING);
        assert_eq!(stored.calendar_id, None, "chip cleared on pause");
        assert_eq!(stored.google_event_id, None, "chip cleared on pause");
        assert_eq!(stored.updated_at, "2026-08-23T02:00:00Z");

        // `done` / `skipped` → 400 (reopen is the undo).
        for from in ["done", "skipped"] {
            let id = format!("occ-{from}");
            repos.occurrences.stored.lock().unwrap().push(FakeOccurrenceRepo::row(
                &id, "rt-1", "u-1", "2026-08-23", from,
            ));
            let err = pause(&repos, "u-1", &id).unwrap_err();
            assert!(
                matches!(err, AgendaError::Invalid(ref m) if m == &format!("cannot pause a {from} occurrence")),
                "{from}: {err:?}"
            );
            let stored = pollster::block_on(repos.occurrences.get_by_id(&id)).unwrap().unwrap();
            assert_eq!(stored.status, from, "reject wrote nothing");
        }
    }

    #[test]
    fn pause_missing_or_other_user_is_not_found() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        repos.occurrences.stored.lock().unwrap().push(FakeOccurrenceRepo::row(
            "occ-1", "rt-1", "u-1", "2026-08-23", "in_progress",
        ));
        assert!(matches!(pause(&repos, "u-1", "missing").unwrap_err(), AgendaError::NotFound));
        assert!(matches!(pause(&repos, "u-2", "occ-1").unwrap_err(), AgendaError::NotFound));
    }

    // ──────────────────────────────────────────
    // PATCH /api/occurrences/:id — title override
    // ──────────────────────────────────────────

    #[test]
    fn patch_title_overrides_and_empty_clears_back_to_inheritance() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
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
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-2", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        repos
            .occurrences
            .stored
            .lock()
            .unwrap()
            .push(FakeOccurrenceRepo::row("occ-1", "rt-1", "u-2", "2026-08-23", "pending"));

        assert!(matches!(complete(&repos, "u-1", "occ-1").unwrap_err(), AgendaError::NotFound));
        assert!(matches!(skip(&repos, "u-1", "occ-1").unwrap_err(), AgendaError::NotFound));
        assert!(matches!(reopen(&repos, "u-1", "occ-1").unwrap_err(), AgendaError::NotFound));
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
        let mut routine = FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY");
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
        assert!(matches!(reopen(&repos, "u-1", "occ-1").unwrap_err(), AgendaError::NotFound));
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
        agenda_row(&repos, "ai-1", "u-1", "2026-08-23", AGENDA_KIND_TASK, "t-1", 0);
        let focused = pollster::block_on(get_agenda(
            &repos.calendars,
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &repos.agenda,
            &repos.tasks,
            "u-1",
            Some("2026-08-23"),
            Some("2026-08-23"),
            NOW_UNIX,
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
        assert_eq!(focused.items[0].ref_id, "t-1", "the embed is the living task");
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
            &events, &ops(),
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &repos.agenda,
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
            shared.get("sanctuary_event_id").and_then(|v| v.as_str()).is_some(),
            "client-supplied event id stamped on every insert: {body}"
        );
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
            3,
            "event id + the two occurrence carriers: {body}"
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
            &events, &ops(),
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &repos.agenda,
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
    fn start_gate_follows_the_primary_calendar_tz_not_kolkata() {
        let repos = repos();
        seed_occurrence(&repos, OCCURRENCE_STATUS_PENDING);
        // Late UTC evening: Kolkata is already on the 24th (04:00+05:30),
        // New York is still on the 23rd (18:30 EDT). The gate is the PRIMARY
        // calendar's zone — "travel = home base" — so the 23rd item starts
        // from New York but not from Kolkata, and no zone is hardcoded.
        let evening = rfc3339_to_unix_secs("2026-08-23T22:30:00Z").unwrap();
        let http = FakeHttp::new(vec![(
            "/calendars/primary%40example.com/events",
            200,
            &created_occurrence_json("rt-1", "occ-1", "2026-08-23T22:30:00Z", "2026-08-23T22:45:00Z"),
        )]);
        let events = FakeEventRepo::new();

        // Kolkata primary: today is the 24th — the 23rd item cannot start.
        let kolkata = FakeCalendarRepo::with(vec![GoogleCalendar {
            time_zone: "Asia/Kolkata".to_string(),
            ..calendar("primary@example.com", true)
        }]);
        let err = pollster::block_on(start_occurrence(
            &http,
            &kolkata,
            &events, &ops(),
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &repos.agenda,
            &access(),
            "u-1",
            "occ-1",
            evening,
        ))
        .unwrap_err();
        assert!(
            matches!(err, AgendaError::Invalid(ref m) if m == "occurrence can only be started on a day it is scheduled"),
            "{err:?}"
        );

        // Same instant, New York primary: today is still the 23rd — starts.
        let ny = FakeCalendarRepo::with(vec![GoogleCalendar {
            time_zone: "America/New_York".to_string(),
            ..calendar("primary@example.com", true)
        }]);
        let response = pollster::block_on(start_occurrence(
            &http,
            &ny,
            &events, &ops(),
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &repos.agenda,
            &access(),
            "u-1",
            "occ-1",
            evening,
        ))
        .unwrap();
        assert_eq!(response.occurrence.status, OCCURRENCE_STATUS_IN_PROGRESS);
        let stored = pollster::block_on(repos.occurrences.get_by_id("occ-1"))
            .unwrap()
            .unwrap();
        assert_eq!(stored.calendar_id.as_deref(), Some("cal-primary@example.com"));
    }

    #[test]
    fn start_wrong_date_is_invalid() {
        let repos = repos();
        // The occurrence's rule date IS today (2026-08-23), but its agenda
        // item sits on TOMORROW — the gate is membership, not local_date, and
        // a day without the scheduled row cannot start.
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        repos
            .occurrences
            .stored
            .lock()
            .unwrap()
            .push(FakeOccurrenceRepo::row("occ-1", "rt-1", "u-1", "2026-08-23", "pending"));
        repos.agenda.stored.lock().unwrap().push(AgendaItem {
            id: "ai-1".to_string(),
            user_id: "u-1".to_string(),
            local_date: "2026-08-24".to_string(),
            kind: AGENDA_KIND_OCCURRENCE.to_string(),
            ref_id: "occ-1".to_string(),
            sort_order: 0,
            created_at: "2026-08-23T00:00:00Z".to_string(),
            updated_at: "2026-08-23T00:00:00Z".to_string(),
        });

        let err = start(&repos, "u-1", "occ-1").unwrap_err();
        assert!(
            matches!(err, AgendaError::Invalid(ref m) if m == "occurrence can only be started on a day it is scheduled"),
            "{err:?}"
        );
    }

    #[test]
    fn start_after_reschedule_to_today_works() {
        let repos = repos();
        // Weekly-Monday series: the Monday (2026-08-17) occurrence is
        // rescheduled onto 2026-08-23 — the civil date of NOW_UNIX — while
        // `occurrence.local_date` still says 2026-08-17.
        let item_id = seed_monday_occurrence(&repos);
        reschedule(&repos, "u-1", &item_id, "2026-08-23").unwrap();

        let response = start(&repos, "u-1", "occ-1").unwrap();
        assert_eq!(response.occurrence.status, OCCURRENCE_STATUS_IN_PROGRESS);
        assert_eq!(
            response.occurrence.local_date, "2026-08-17",
            "the rule date is untouched — the item's date is what mattered"
        );
        let stored = pollster::block_on(repos.occurrences.get_by_id("occ-1"))
            .unwrap()
            .unwrap();
        assert_eq!(stored.calendar_id.as_deref(), Some("cal-primary@example.com"));
    }

    #[test]
    fn start_requires_an_agenda_row_for_that_occurrence_on_today() {
        let repos = repos();
        // A pending occurrence with NO agenda item anywhere: even on its own
        // rule date it cannot start — it was never scheduled.
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        repos
            .occurrences
            .stored
            .lock()
            .unwrap()
            .push(FakeOccurrenceRepo::row("occ-1", "rt-1", "u-1", "2026-08-23", "pending"));

        let err = start(&repos, "u-1", "occ-1").unwrap_err();
        assert!(
            matches!(err, AgendaError::Invalid(ref m) if m == "occurrence can only be started on a day it is scheduled"),
            "{err:?}"
        );
    }

    #[test]
    fn start_missing_other_user_or_soft_deleted_routine_is_not_found() {
        let repos = repos();
        // Missing occurrence → 404.
        assert!(matches!(start(&repos, "u-1", "nope").unwrap_err(), AgendaError::NotFound));

        // Another user's occurrence → 404 (never leak existence).
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row("rt-1", "u-2", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
        repos
            .occurrences
            .stored
            .lock()
            .unwrap()
            .push(FakeOccurrenceRepo::row("occ-2", "rt-1", "u-2", "2026-08-23", "pending"));
        assert!(matches!(start(&repos, "u-1", "occ-2").unwrap_err(), AgendaError::NotFound));

        // Soft-deleted routine → its occurrence is 404.
        let mut routine = FakeRoutineRepo::row("rt-3", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY");
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
            &events, &ops(),
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &repos.agenda,
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
            &events, &ops(),
            &repos.lists,
            &repos.categories,
            &repos.routines,
            &repos.occurrences,
            &repos.agenda,
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
            .push(FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"));
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
                google_etag: "e1".to_string(),
                google_updated_at: String::new(),
                last_synced_at: "2026-08-23T00:00:00Z".to_string(),
                title: "Fajr".to_string(),
                description: String::new(),
                start_time: "2026-08-23T10:00:00Z".to_string(),
                end_time: "2026-08-23T10:15:00Z".to_string(),
                recurrence: String::new(),
                task_id: String::new(),
                ical_uid: String::new(),
                sequence: 0,
                status: String::new(),
                recurring_event_id: String::new(),
                original_start: String::new(),
                start_time_zone: String::new(),
                end_time_zone: String::new(),
                is_all_day: false,
                raw_json: String::new(),
            },
            "2026-08-23T00:00:00Z",
        ))
        .unwrap();

        let response = pollster::block_on(complete_occurrence(
            Some(&http),
            Some(&calendars),
            Some(&events),
            Some(&ops()),
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
                google_etag: "e1".to_string(),
                google_updated_at: String::new(),
                last_synced_at: "2026-08-23T00:00:00Z".to_string(),
                title: "Fajr".to_string(),
                description: String::new(),
                start_time: "2026-08-23T10:00:00Z".to_string(),
                end_time: "2026-08-23T10:15:00Z".to_string(),
                recurrence: String::new(),
                task_id: String::new(),
                ical_uid: String::new(),
                sequence: 0,
                status: String::new(),
                recurring_event_id: String::new(),
                original_start: String::new(),
                start_time_zone: String::new(),
                end_time_zone: String::new(),
                is_all_day: false,
                raw_json: String::new(),
            },
            "2026-08-23T00:00:00Z",
        ))
        .unwrap();

        let response = pollster::block_on(skip_occurrence(
            Some(&http),
            Some(&calendars),
            Some(&events),
            Some(&ops()),
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
    fn pause_while_in_progress_patches_end_clears_ids_lands_pending() {
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
                google_etag: "e1".to_string(),
                google_updated_at: String::new(),
                last_synced_at: "2026-08-23T00:00:00Z".to_string(),
                title: "Fajr".to_string(),
                description: String::new(),
                start_time: "2026-08-23T10:00:00Z".to_string(),
                end_time: "2026-08-23T10:15:00Z".to_string(),
                recurrence: String::new(),
                task_id: String::new(),
                ical_uid: String::new(),
                sequence: 0,
                status: String::new(),
                recurring_event_id: String::new(),
                original_start: String::new(),
                start_time_zone: String::new(),
                end_time_zone: String::new(),
                is_all_day: false,
                raw_json: String::new(),
            },
            "2026-08-23T00:00:00Z",
        ))
        .unwrap();

        let response = pollster::block_on(pause_occurrence(
            Some(&http),
            Some(&calendars),
            Some(&events),
            Some(&ops()),
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

        assert_eq!(response.occurrence.status, OCCURRENCE_STATUS_PENDING);
        let stored = pollster::block_on(repos.occurrences.get_by_id("occ-1"))
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, OCCURRENCE_STATUS_PENDING);
        assert_eq!(stored.calendar_id, None, "chip cleared so the next start mints a new event");
        assert_eq!(stored.google_event_id, None);

        let patches = http.patches.lock().unwrap();
        assert_eq!(patches.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&patches[0].1).unwrap();
        assert_eq!(body["end"]["dateTime"], "2026-08-23T10:01:00Z");
        assert!(patches[0].0.contains("events/g-1"), "{}", patches[0].0);
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
            Some(&ops()),
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
            Some(&ops()),
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
            Some(&ops()),
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
            Some(&ops()),
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
            Some(&ops()),
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
            &ops(),
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
                google_etag: "e1".to_string(),
                google_updated_at: String::new(),
                last_synced_at: "2026-08-23T00:00:00Z".to_string(),
                title: "Fajr".to_string(),
                description: String::new(),
                start_time: "2026-08-23T10:00:00Z".to_string(),
                // Current end is before the 10:20 target → it must grow.
                end_time: "2026-08-23T10:15:00Z".to_string(),
                recurrence: String::new(),
                task_id: String::new(),
                ical_uid: String::new(),
                sequence: 0,
                status: String::new(),
                recurring_event_id: String::new(),
                original_start: String::new(),
                start_time_zone: String::new(),
                end_time_zone: String::new(),
                is_all_day: false,
                raw_json: String::new(),
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
                google_etag: "e1".to_string(),
                google_updated_at: String::new(),
                last_synced_at: "2026-08-23T00:00:00Z".to_string(),
                title: "Fajr".to_string(),
                description: String::new(),
                start_time: "2026-08-23T10:00:00Z".to_string(),
                end_time: "2026-08-23T10:30:00Z".to_string(),
                recurrence: String::new(),
                task_id: String::new(),
                ical_uid: String::new(),
                sequence: 0,
                status: String::new(),
                recurring_event_id: String::new(),
                original_start: String::new(),
                start_time_zone: String::new(),
                end_time_zone: String::new(),
                is_all_day: false,
                raw_json: String::new(),
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

    // ──────────────────────────────────────────
    // The agenda fill (slice 4)
    // ──────────────────────────────────────────

    /// The horizon under `NOW_UNIX`: today … today+6, inclusive.
    fn horizon_dates() -> Vec<String> {
        (0..crate::agenda_fill::OCCURRENCE_HORIZON_DAYS)
            .map(|offset| add_civil_days("2026-08-23", offset).unwrap())
            .collect()
    }

    /// `fill_occurrence_horizon` with the shared repos and the test clock.
    fn fill_horizon(
        repos: &Repos,
        user_id: &str,
        only_routine: Option<&str>,
    ) -> Result<(), AgendaError> {
        pollster::block_on(fill_occurrence_horizon(
            &repos.routines,
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            user_id,
            NOW_UNIX,
            only_routine,
        ))
    }

    /// `ensure_task_items` with the shared repos and the test clock.
    fn ensure_tasks(repos: &Repos, user_id: &str) -> Result<(), AgendaError> {
        pollster::block_on(ensure_task_items(
            &repos.agenda,
            &repos.tasks,
            &repos.calendars,
            user_id,
            NOW_UNIX,
        ))
    }

    /// One `run_agenda_fill_cron` pass over the shared repos.
    fn fill_cron(repos: &Repos) -> AgendaFillReport {
        pollster::block_on(run_agenda_fill_cron(
            &repos.routines,
            &repos.occurrences,
            &repos.agenda,
            &repos.tasks,
            &repos.calendars,
            NOW_UNIX,
        ))
    }

    /// `create_routine` over the shared repos (create fills the horizon).
    fn create(
        repos: &Repos,
        user_id: &str,
        title: &str,
        rrule: &str,
    ) -> Result<RoutineResponse, RoutinesError> {
        pollster::block_on(create_routine(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            user_id,
            &NewRoutineInput {
                title: title.to_string(),
                estimated_minutes: None,
                rrule: rrule.to_string(),
            },
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
    }

    /// `update_routine` over the shared repos.
    fn update(
        repos: &Repos,
        user_id: &str,
        routine_id: &str,
        updates: &UpdateRoutine,
    ) -> Result<RoutineResponse, RoutinesError> {
        pollster::block_on(update_routine(
            &repos.categories,
            &repos.routines,
            user_id,
            routine_id,
            updates,
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
    }

    /// `delete_routine` over the shared repos.
    fn delete(
        repos: &Repos,
        user_id: &str,
        routine_id: &str,
    ) -> Result<DeleteRoutineResponse, RoutinesError> {
        pollster::block_on(delete_routine(
            &repos.routines,
            user_id,
            routine_id,
            &unix_secs_to_rfc3339(NOW_UNIX),
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
    }

    /// The stored occurrence dates of `user_id`, ascending.
    fn occurrence_dates_in_store(repos: &Repos, user_id: &str) -> Vec<String> {
        let mut dates: Vec<String> = repos
            .occurrences
            .stored
            .lock()
            .unwrap()
            .iter()
            .filter(|occ| occ.user_id == user_id)
            .map(|occ| occ.local_date.clone())
            .collect();
        dates.sort();
        dates
    }

    /// The stored occurrence of `routine_id` on `date` (panics when absent).
    fn occurrence_on(repos: &Repos, routine_id: &str, date: &str) -> RoutineOccurrence {
        pollster::block_on(repos.occurrences.get_by_routine_and_date(routine_id, date))
            .unwrap()
            .unwrap_or_else(|| panic!("no occurrence for {routine_id} on {date}"))
    }

    /// The stored membership rows of one occurrence, on ANY date (the
    /// "membership anywhere" view).
    fn items_of(repos: &Repos, occurrence_id: &str) -> Vec<AgendaItem> {
        let mut rows: Vec<AgendaItem> = repos
            .agenda
            .stored
            .lock()
            .unwrap()
            .iter()
            .filter(|item| item.ref_id == occurrence_id)
            .cloned()
            .collect();
        rows.sort_by(|a, b| a.local_date.cmp(&b.local_date));
        rows
    }

    /// `(ref_id, sort_order, local_date)` for one user's membership rows, in
    /// stream order — the append-order assertion shape.
    fn membership_triples(repos: &Repos, user_id: &str) -> Vec<(String, i64, String)> {
        let mut rows: Vec<AgendaItem> = repos
            .agenda
            .stored
            .lock()
            .unwrap()
            .iter()
            .filter(|item| item.user_id == user_id)
            .cloned()
            .collect();
        rows.sort_by(|a, b| (&a.local_date, a.sort_order).cmp(&(&b.local_date, b.sort_order)));
        rows.into_iter()
            .map(|item| (item.ref_id, item.sort_order, item.local_date))
            .collect()
    }

    #[test]
    fn fill_covers_today_through_today_plus_six_and_writes_nothing_twice() {
        let repos = repos();
        // A daily rule whose DTSTART is in January: the past is NOT
        // backfilled, only the horizon dates the rule covers.
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "DTSTART:20260101T053000\nRRULE:FREQ=DAILY",
        ));

        fill_horizon(&repos, "u-1", None).unwrap();

        let expected = horizon_dates();
        assert_eq!(
            occurrence_dates_in_store(&repos, "u-1"),
            expected,
            "exactly today … today+6"
        );
        assert!(
            repos
                .occurrences
                .stored
                .lock()
                .unwrap()
                .iter()
                .all(|occ| occ.local_date.as_str() >= "2026-08-23"),
            "a past DTSTART never writes a past row"
        );
        assert_eq!(
            membership_triples(&repos, "u-1")
                .iter()
                .map(|(_, _, date)| date.clone())
                .collect::<Vec<_>>(),
            expected,
            "one agenda row per occurrence, on its rule date"
        );

        // A second pass with the same data writes nothing at all.
        let occurrences_before = repos.occurrences.stored.lock().unwrap().len();
        let agenda_before = repos.agenda.stored.lock().unwrap().len();
        repos.occurrences.reset_calls();
        repos.agenda.reset_calls();
        fill_horizon(&repos, "u-1", None).unwrap();

        assert_eq!(repos.occurrences.stored.lock().unwrap().len(), occurrences_before);
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), agenda_before);
        assert_eq!(repos.occurrences.call_counts().insert_many, 0, "no occurrence write");
        assert_eq!(repos.agenda.call_counts().insert_many, 0, "no agenda write");
    }

    #[test]
    fn fill_weekly_monday_lands_on_monday_alone() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "DTSTART:20260105T053000\nRRULE:FREQ=WEEKLY;BYDAY=MO",
        ));

        fill_horizon(&repos, "u-1", None).unwrap();

        // 2026-08-24 is the one Monday inside today … today+6.
        assert_eq!(occurrence_dates_in_store(&repos, "u-1"), ["2026-08-24"]);
        assert_eq!(
            membership_triples(&repos, "u-1"),
            vec![(
                occurrence_on(&repos, "rt-1", "2026-08-24").id,
                0,
                "2026-08-24".to_string()
            )]
        );
    }

    #[test]
    fn fill_never_reattaches_a_rescheduled_occurrence_to_its_rule_date() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "DTSTART:20260101T053000\nRRULE:FREQ=DAILY",
        ));
        fill_horizon(&repos, "u-1", None).unwrap();

        // Snooze today's row two days forward.
        let today_item = membership_triples(&repos, "u-1")
            .into_iter()
            .find(|(_, _, date)| date == "2026-08-23")
            .map(|(ref_id, _, _)| ref_id)
            .expect("today's row exists");
        let item_id = pollster::block_on(repos.agenda.list_by_user_and_date("u-1", "2026-08-23"))
            .unwrap()
            .into_iter()
            .find(|item| item.ref_id == today_item)
            .unwrap()
            .id;
        let occurrence_id = today_item;
        let before = occurrence_on(&repos, "rt-1", "2026-08-23");
        let moved = reschedule(&repos, "u-1", &item_id, "2026-08-25").unwrap();
        assert_eq!(moved.item.local_date, "2026-08-25");

        fill_horizon(&repos, "u-1", None).unwrap();

        // The rule date gains no second row: the moved row is the only one.
        let memberships = items_of(&repos, &occurrence_id);
        assert_eq!(memberships.len(), 1, "no re-attachment to the rule date");
        assert_eq!(memberships[0].local_date, "2026-08-25");
        assert_eq!(
            occurrence_on(&repos, "rt-1", "2026-08-23"),
            before,
            "the occurrence keeps its rule date"
        );
        assert_eq!(
            occurrence_dates_in_store(&repos, "u-1").len(),
            7,
            "nothing was added or removed"
        );
    }

    #[test]
    fn fill_appends_after_the_existing_pile_in_standing_routine_order() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().extend([
            FakeRoutineRepo::row("rt-1", "u-1", "Fajr", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"),
            FakeRoutineRepo::row("rt-2", "u-1", "Salat", 1, "DTSTART:20260101T120000\nRRULE:FREQ=DAILY"),
        ]);
        // A pre-existing row at rank 5 on today: the fill appends after it and
        // never reshuffles it.
        agenda_row(&repos, "ai-pinned", "u-1", "2026-08-23", AGENDA_KIND_TASK, "task-x", 5);

        fill_horizon(&repos, "u-1", None).unwrap();

        let today: Vec<(String, i64)> =
            pollster::block_on(repos.agenda.list_by_user_and_date("u-1", "2026-08-23"))
                .unwrap()
                .into_iter()
                .map(|item| (item.ref_id, item.sort_order))
                .collect();
        assert_eq!(
            today,
            vec![
                ("task-x".to_string(), 5),
                (occurrence_on(&repos, "rt-1", "2026-08-23").id, 6),
                (occurrence_on(&repos, "rt-2", "2026-08-23").id, 7),
            ],
            "today's new rows land after the pile's max rank, in routine order"
        );
    }

    #[test]
    fn ensure_task_items_backfills_open_tasks_on_today_in_board_order() {
        let repos = repos();
        let mut running = task_row("task-run", "u-1", "Run | Work", "IN_PROGRESS");
        running.sort_order = 1;
        let mut planned = task_row("task-plan", "u-1", "Plan | Work", "PLANNED");
        planned.sort_order = 0;
        let mut open = task_row("task-open", "u-1", "Open | Work", "OPEN");
        open.sort_order = 4;
        let completed = task_row("task-done", "u-1", "Done | Work", "COMPLETED");
        let mut placed = task_row("task-placed", "u-1", "Placed | Work", "OPEN");
        placed.sort_order = 9;
        for task in [running, planned, open, completed, placed] {
            repos.tasks.push(task);
        }
        // A task that already holds a slot (on a past day) is left alone: no
        // second row, no move.
        agenda_row(&repos, "ai-placed", "u-1", "2026-08-20", AGENDA_KIND_TASK, "task-placed", 2);

        ensure_tasks(&repos, "u-1").unwrap();

        let today: Vec<(String, i64)> =
            pollster::block_on(repos.agenda.list_by_user_and_date("u-1", "2026-08-23"))
                .unwrap()
                .into_iter()
                .map(|item| (item.ref_id, item.sort_order))
                .collect();
        assert_eq!(
            today,
            vec![
                ("task-run".to_string(), 0),
                ("task-plan".to_string(), 1),
                ("task-open".to_string(), 2),
            ],
            "IN_PROGRESS, PLANNED, OPEN — ranks 0, 1, 2"
        );
        assert!(
            membership_triples(&repos, "u-1")
                .iter()
                .all(|(ref_id, _, _)| ref_id != "task-done"),
            "a COMPLETED task gets nothing"
        );
        let placed_rows: Vec<(String, i64, String)> = membership_triples(&repos, "u-1")
            .into_iter()
            .filter(|(ref_id, _, _)| ref_id == "task-placed")
            .collect();
        assert_eq!(
            placed_rows,
            vec![("task-placed".to_string(), 2, "2026-08-20".to_string())],
            "an already-placed task keeps its single row where it was"
        );

        // A second pass writes nothing.
        let before = repos.agenda.stored.lock().unwrap().len();
        repos.agenda.reset_calls();
        ensure_tasks(&repos, "u-1").unwrap();
        assert_eq!(repos.agenda.stored.lock().unwrap().len(), before);
        assert_eq!(repos.agenda.call_counts().insert_many, 0);
    }

    #[test]
    fn update_routine_reconciles_only_untouched_future_rows() {
        let repos = repos();
        let created = create(
            &repos,
            "u-1",
            "Work",
            "DTSTART:20260101T053000\nRRULE:FREQ=DAILY",
        )
        .unwrap();
        let routine_id = created.routine.id.clone();
        // Create filled the whole horizon.
        assert_eq!(occurrence_dates_in_store(&repos, "u-1"), horizon_dates());

        // One row per "the user touched it" shape, all after today.
        let done_id = occurrence_on(&repos, &routine_id, "2026-08-26").id;
        pollster::block_on(repos.occurrences.set_status(&done_id, OCCURRENCE_STATUS_DONE)).unwrap();
        let renamed_id = occurrence_on(&repos, &routine_id, "2026-08-27").id;
        pollster::block_on(repos.occurrences.update_title(&renamed_id, Some("Overridden"))).unwrap();
        let chipped_id = occurrence_on(&repos, &routine_id, "2026-08-28").id;
        pollster::block_on(repos.occurrences.set_event_ids(&chipped_id, "cal-1", "evt-1")).unwrap();
        // …and one rescheduled out of its rule date.
        let moved_id = occurrence_on(&repos, &routine_id, "2026-08-25").id;
        let moved_item = pollster::block_on(repos.agenda.get_by_ref(
            "u-1",
            AGENDA_KIND_OCCURRENCE,
            &moved_id,
        ))
        .unwrap()
        .unwrap();
        reschedule(&repos, "u-1", &moved_item.id, "2026-08-28").unwrap();
        // The untouched pending row the new rule will not cover.
        let doomed_id = occurrence_on(&repos, &routine_id, "2026-08-29").id;

        let updated = update(
            &repos,
            "u-1",
            &routine_id,
            &UpdateRoutine {
                rrule: Some("DTSTART:20260101T053000\nRRULE:FREQ=WEEKLY;BYDAY=MO".to_string()),
                ..UpdateRoutine::default()
            },
        )
        .unwrap();
        assert_eq!(updated.routine.rrule, "DTSTART:20260101T053000\nRRULE:FREQ=WEEKLY;BYDAY=MO");

        assert_eq!(
            occurrence_dates_in_store(&repos, "u-1"),
            [
                "2026-08-23", // today (a Sunday) is NEVER deleted
                "2026-08-24", // the one Monday the new rule covers
                "2026-08-25", // rescheduled: kept
                "2026-08-26", // done: kept
                "2026-08-27", // renamed: kept
                "2026-08-28", // Google chip: kept
            ],
            "only the untouched 2026-08-29 row went"
        );
        assert!(items_of(&repos, &doomed_id).is_empty(), "its agenda row went with it");
        assert_eq!(
            items_of(&repos, &moved_id)
                .iter()
                .map(|item| item.local_date.clone())
                .collect::<Vec<_>>(),
            ["2026-08-28"],
            "the rescheduled membership is untouched"
        );

        // A title-only update does no occurrence work at all.
        let before = occurrence_dates_in_store(&repos, "u-1");
        update(
            &repos,
            "u-1",
            &routine_id,
            &UpdateRoutine {
                title: Some("Work".to_string()),
                ..UpdateRoutine::default()
            },
        )
        .unwrap();
        assert_eq!(occurrence_dates_in_store(&repos, "u-1"), before);
    }

    #[test]
    fn delete_routine_hard_deletes_only_untouched_future_rows() {
        let repos = repos();
        let created = create(
            &repos,
            "u-1",
            "Work",
            "DTSTART:20260101T053000\nRRULE:FREQ=DAILY",
        )
        .unwrap();
        let routine_id = created.routine.id.clone();
        assert_eq!(occurrence_dates_in_store(&repos, "u-1"), horizon_dates());

        // One tomorrow-or-later row the user acted on.
        let done_id = occurrence_on(&repos, &routine_id, "2026-08-25").id;
        pollster::block_on(repos.occurrences.set_status(&done_id, OCCURRENCE_STATUS_DONE)).unwrap();

        let response = delete(&repos, "u-1", &routine_id).unwrap();
        assert!(response.success);

        assert_eq!(
            occurrence_dates_in_store(&repos, "u-1"),
            ["2026-08-23", "2026-08-25"],
            "today's pending row and the done row stay; the untouched future ones are gone"
        );
        assert_eq!(
            membership_triples(&repos, "u-1")
                .iter()
                .map(|(_, _, date)| date.clone())
                .collect::<Vec<_>>(),
            ["2026-08-23", "2026-08-25"],
            "the deleted rows' agenda items went with them"
        );
        assert!(
            repos
                .routines
                .stored
                .lock()
                .unwrap()
                .iter()
                .any(|row| row.id == routine_id && row.deleted_at.is_some()),
            "the routine itself is soft-deleted"
        );
    }

    #[test]
    fn agenda_fill_cron_fills_every_user_and_reports_per_user_failures() {
        let repos = repos();
        // u-1: a daily routine and no open task.
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "DTSTART:20260101T053000\nRRULE:FREQ=DAILY",
        ));
        // u-2: an OPEN task and no routine.
        let mut open = task_row("task-1", "u-2", "Ship | Work", "OPEN");
        open.sort_order = 0;
        repos.tasks.push(open);

        let report = fill_cron(&repos);

        assert_eq!(report.users, 2);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(occurrence_dates_in_store(&repos, "u-1"), horizon_dates());
        assert_eq!(
            membership_triples(&repos, "u-2"),
            vec![("task-1".to_string(), 0, "2026-08-23".to_string())],
            "u-2's OPEN task landed on today"
        );

        // A living routine whose calendar read fails: one broken user is
        // recorded and the others still fill.
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-b",
            "u-broken",
            "Fajr",
            0,
            "DTSTART:20260101T053000\nRRULE:FREQ=DAILY",
        ));
        repos.calendars.fail_for("u-broken");

        let report = fill_cron(&repos);

        assert_eq!(report.users, 3, "every affected user is attempted");
        assert_eq!(report.errors.len(), 2, "{:?}", report.errors);
        assert!(
            report.errors.iter().all(|error| error.contains("u-broken")),
            "{:?}",
            report.errors
        );
        assert!(occurrence_dates_in_store(&repos, "u-broken").is_empty());
        // The healthy users are untouched by the failure (and idempotent).
        assert_eq!(occurrence_dates_in_store(&repos, "u-1"), horizon_dates());
        assert_eq!(
            membership_triples(&repos, "u-2"),
            vec![("task-1".to_string(), 0, "2026-08-23".to_string())]
        );
    }
}