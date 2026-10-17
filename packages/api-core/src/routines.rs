//! Routine service: CRUD for standing routines (ADR 0004) plus the RRULE
//! helpers every later slice shares.
//!
//! Pure Rust and unit-testable: persistence goes through [`RoutineRepo`]
//! (faked with an in-memory impl in tests); classification reuses the same
//! matcher as tasks. The Worker layers session checks on top
//! (`apps/worker/src/routines.rs`).
//!
//! Domain rules (ADR 0004 amendment, locked):
//! - A routine is a standing definition — never completable, never on the
//!   Board. Repetition is stored as ONE `routines.rrule` TEXT blob with
//!   exactly two `\n`-separated lines — `DTSTART:YYYYMMDDTHHMMSS` (floating
//!   local, no Z/TZID) + `RRULE:<body>` — and expansion happens in **floating
//!   local civil time** (the naive DTSTART is treated as UTC inside this crate
//!   purely to satisfy the parser — no TZID ever exists, and the tzdb is
//!   never used for membership). No EXDATE/RDATE/EXRULE/TZID anywhere; the
//!   RRULE body may keep a Z-form `UNTIL` if the crate needs it.
//! - Create/update classify the title with the **same unique non-untracked
//!   rules as `create_task`** (400 on 0 matches / conflict / untracked sink),
//!   seeding the taxonomy first like the tasks endpoints do. The classify
//!   match is deliberately duplicated here instead of refactoring
//!   `tasks.rs` (slice brief).
//! - Titles are NOT unique. `estimated_minutes` defaults to 15 and must be
//!   >= 1. Create appends the standing order: `max(sort_order)+1` (0 when the
//!   pile is empty); peers never shift.
//! - Create fills the occurrence horizon for the new routine (today …
//!   today+6). A validation 400 still writes nothing.
//! - Delete is SOFT; it then hard-deletes this routine's **untouched** rows
//!   from **tomorrow through today+6** (and their agenda items). Untouched
//!   means still `pending`, no title override, no Google chip, and not
//!   rescheduled. Today's and past rows always stay — the range read already
//!   hides a deleted routine's pending/in_progress leftovers and keeps its
//!   done/skipped history. A missing/other-user/soft-deleted routine is
//!   always [`RoutinesError::NotFound`] — ownership is never leaked.
//! - A rule change (`rrule`) reconciles the same future window for that
//!   routine: rows the new rule does not cover and that the user never
//!   touched are hard-deleted (with their agenda items), then the horizon is
//!   filled again. The prune runs **before** the new blob is saved, so a
//!   failed prune is still retryable; the refill starts at **tomorrow** —
//!   the edit never gives today a new occurrence, and the next `*/15` cron
//!   tick fills today if the new rule covers it. Today and every earlier date
//!   are never deleted or edited; existing rows are never rewritten. A
//!   title/estimate/sort-only update does no occurrence/agenda work at all.

use std::collections::{HashMap, HashSet};

use chrono::{NaiveDate, NaiveDateTime, TimeZone};
use thiserror::Error;

use crate::agenda::{
    add_civil_days, user_today, AgendaError, AGENDA_KIND_OCCURRENCE, OCCURRENCE_STATUS_PENDING,
};
use crate::agenda_fill::{fill_occurrence_horizon, OCCURRENCE_HORIZON_DAYS};
use crate::categories::{
    classify, ensure_taxonomy, CalendarScope, CategoryWithPatterns, ClassifyOutcome,
};
use crate::models::{
    NewRoutine, NewRoutineInput, Routine, RoutineOccurrence, TaskCategory, TaskCategoryPattern,
    UpdateRoutine,
};
use crate::repo::{
    AgendaItemRepo, CalendarRepo, OccurrenceRepo, RepoError, RoutineRepo, TaskCategoryRepo,
    TaskListRepo,
};
use crate::tasks::TaskCategorySummary;

/// `routines.estimated_minutes` default (ADR 0004): a planned estimate, not a
/// duration. The start marker stays the fixed task rule regardless.
pub const DEFAULT_ESTIMATED_MINUTES: i64 = 15;
/// Lower bound enforced at the API; 400 "estimated_minutes must be at least 1".
pub const MIN_ESTIMATED_MINUTES: i64 = 1;

/// Errors produced by the routines service.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RoutinesError {
    #[error("{0}")]
    Invalid(String),
    #[error("routine not found")]
    NotFound,
    #[error("database error: {0}")]
    Repo(#[from] RepoError),
}

/// `list_routines`/`create_routine` run `ensure_taxonomy`, whose errors fold
/// into [`RoutinesError`] (same HTTP mapping as tasks: 400 / 404 / 500).
impl From<crate::categories::CategoriesError> for RoutinesError {
    fn from(err: crate::categories::CategoriesError) -> Self {
        match err {
            crate::categories::CategoriesError::Invalid(message) => RoutinesError::Invalid(message),
            crate::categories::CategoriesError::NotFound => RoutinesError::NotFound,
            crate::categories::CategoriesError::Conflict(message) => RoutinesError::Invalid(message),
            crate::categories::CategoriesError::Repo(err) => RoutinesError::Repo(err),
        }
    }
}

/// Response envelope for `GET /api/routines`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RoutinesResponse {
    pub routines: Vec<RoutineView>,
}

/// Response envelope for `POST /api/routines` and `PATCH /api/routines/:id`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RoutineResponse {
    pub routine: RoutineView,
}

/// Response envelope for `DELETE /api/routines/:id`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeleteRoutineResponse {
    pub success: bool,
}

/// HTTP shape of a routine: every `routines` column (minus `deleted_at`),
/// plus the **computed** category summary so the client never reimplements
/// the matcher.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RoutineView {
    pub id: String,
    pub user_id: String,
    pub title: String,
    pub estimated_minutes: i64,
    /// The two-line recurrence blob (`DTSTART:` line + `RRULE:` line).
    pub rrule: String,
    pub sort_order: i64,
    /// RFC 3339 instant.
    pub created_at: String,
    /// RFC 3339 instant.
    pub updated_at: String,
    /// Computed per title with the same matcher as tasks (never fails — an
    /// unmatched title keeps the `untracked` summary; reads are not validation).
    pub category: TaskCategorySummary,
}

// ──────────────────────────────────────────
// Recurrence: parse / validate / expand
// ──────────────────────────────────────────

/// Parses the two-line recurrence blob into its floating-local DTSTART and
/// the bare RRULE body.
///
/// The stored format is locked by ADR 0004 (amendment): exactly two
/// `\n`-separated lines, no trailing extras —
/// `DTSTART:20260105T063000\nRRULE:FREQ=WEEKLY;BYDAY=MO`. The DTSTART is
/// **floating local** (basic form `YYYYMMDDTHHMMSS`, no `Z`, no `TZID`); the
/// RRULE line holds the body only. Rejected (400 "invalid rrule") when the
/// blob contains `EXDATE`, `RDATE`, `EXRULE`, or `TZID` anywhere, more than
/// one `RRULE:` line, a `Z` on the DTSTART value, or anything other than
/// exactly those two lines.
fn parse_blob(rrule: &str) -> Result<(NaiveDateTime, &str), RoutinesError> {
    let blob = rrule.trim();
    let upper = blob.to_ascii_uppercase();
    for forbidden in ["EXDATE", "RDATE", "EXRULE", "TZID"] {
        if upper.contains(forbidden) {
            return Err(RoutinesError::Invalid(
                "invalid rrule: recurrence must be DTSTART + RRULE only".to_string(),
            ));
        }
    }
    if upper.matches("RRULE:").count() != 1 {
        return Err(RoutinesError::Invalid(
            "invalid rrule: exactly one RRULE line is required".to_string(),
        ));
    }
    let mut lines = blob
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let Some(dtstart_line) = lines.next() else {
        return Err(RoutinesError::Invalid("invalid rrule".to_string()));
    };
    let Some(rrule_line) = lines.next() else {
        return Err(RoutinesError::Invalid("invalid rrule".to_string()));
    };
    if lines.next().is_some() {
        return Err(RoutinesError::Invalid(
            "invalid rrule: exactly two lines (DTSTART + RRULE)".to_string(),
        ));
    }
    if !dtstart_line.to_ascii_uppercase().starts_with("DTSTART:") {
        return Err(RoutinesError::Invalid(
            "invalid rrule: first line must be DTSTART:".to_string(),
        ));
    }
    let dt_value = dtstart_line["DTSTART:".len()..].trim();
    // Floating local: the basic form must never carry a Z or an offset.
    if dt_value.to_ascii_uppercase().contains('Z') {
        return Err(RoutinesError::Invalid(
            "invalid rrule: DTSTART must be floating local (no Z)".to_string(),
        ));
    }
    let naive_start = NaiveDateTime::parse_from_str(dt_value, "%Y%m%dT%H%M%S").map_err(|_| {
        RoutinesError::Invalid("invalid rrule: DTSTART must be YYYYMMDDTHHMMSS".to_string())
    })?;
    if !rrule_line.to_ascii_uppercase().starts_with("RRULE:") {
        return Err(RoutinesError::Invalid(
            "invalid rrule: second line must be RRULE:".to_string(),
        ));
    }
    let body = rrule_line["RRULE:".len()..].trim();
    if body.is_empty() {
        return Err(RoutinesError::Invalid("invalid rrule".to_string()));
    }
    Ok((naive_start, body))
}

/// Validates one local date string (`YYYY-MM-DD`).
fn parse_local_date(date: &str) -> Result<NaiveDate, RoutinesError> {
    NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d")
        .map_err(|_| RoutinesError::Invalid("dates must be YYYY-MM-DD".to_string()))
}

/// Parses + validates a recurrence blob against its own DTSTART. Returns the
/// aware civil datetime (UTC carrier for the naive local time) plus the
/// validated rule ready for [`rrule::RRuleSet::rrule`].
///
/// Floating local civil time: the naive DTSTART is attached to UTC purely as
/// a carrier for the crate (which requires an aware datetime). Expansion then
/// yields exactly those civil times back, so membership never involves the
/// tzdb — matching ADR 0004's floating-local rule.
fn parse_recurrence(
    rrule: &str,
) -> Result<(chrono::DateTime<rrule::Tz>, rrule::RRule<rrule::Validated>), RoutinesError> {
    let (naive_start, body) = parse_blob(rrule)?;
    let dt_start = rrule::Tz::UTC.from_utc_datetime(&naive_start);
    let unvalidated: rrule::RRule<rrule::Unvalidated> =
        body.parse().map_err(|_| RoutinesError::Invalid("invalid rrule".to_string()))?;
    let validated = unvalidated
        .validate(dt_start)
        .map_err(|_| RoutinesError::Invalid("invalid rrule".to_string()))?;
    Ok((dt_start, validated))
}

/// Validates a recurrence blob without expanding it — the create/update gate
/// (invalid rule → 400 "invalid rrule").
pub fn validate_recurrence(rrule: &str) -> Result<(), RoutinesError> {
    parse_recurrence(rrule)?;
    Ok(())
}

/// Expands a recurrence blob into the local civil dates it covers in the
/// inclusive window `[from, to]` (`YYYY-MM-DD`, both ends included). Dates
/// come back ascending and deduplicated.
///
/// Floating local civil time throughout: results are read straight off the
/// UTC-carried civil datetimes (see [`parse_recurrence`]) — never converted
/// through any timezone database.
pub fn occurrence_dates(
    rrule: &str,
    from: &str,
    to: &str,
) -> Result<Vec<String>, RoutinesError> {
    let from = parse_local_date(from)?;
    let to = parse_local_date(to)?;
    if from > to {
        return Err(RoutinesError::Invalid(
            "from must not be after to".to_string(),
        ));
    }

    // Widen the crate-level bounds by a day on each side so inclusivity at the
    // edges cannot depend on the library's boundary conventions; occurrences
    // are filtered to the exact civil-date window below anyway.
    let after_bound = utc_midnight(from.pred_opt().ok_or_else(|| {
        RoutinesError::Invalid("from is out of range".to_string())
    })?);
    let before_bound = utc_midnight(to.succ_opt().ok_or_else(|| {
        RoutinesError::Invalid("to is out of range".to_string())
    })?);

    let (dt_start, validated) = parse_recurrence(rrule)?;
    let set = rrule::RRuleSet::new(dt_start)
        .rrule(validated)
        .after(after_bound)
        .before(before_bound);
    // Terminates on its own: `before` bounds the iteration (all_unchecked only
    // skips the crate's result-count limit, which would silently truncate).
    let dates = set.all_unchecked();

    let mut out: Vec<String> = Vec::with_capacity(dates.len());
    for date in dates {
        // The carrier tz is UTC, so the naive view IS the floating civil time.
        let civil = date.naive_utc().date();
        if civil < from || civil > to {
            continue;
        }
        let day = civil.format("%Y-%m-%d").to_string();
        if !out.contains(&day) {
            out.push(day);
        }
    }
    Ok(out)
}

/// Midnight UTC of a civil date — the widened window bound carrier.
fn utc_midnight(date: NaiveDate) -> chrono::DateTime<rrule::Tz> {
    rrule::Tz::UTC
        .from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("00:00:00 is a valid time"))
}

// ──────────────────────────────────────────
// Service
// ──────────────────────────────────────────

/// Lists the user's living routines in standing order, each with its computed
/// category summary.
///
/// Seeding: like `list_tasks`, this runs `ensure_taxonomy` (count-gated) so a
/// routines-only first visitor still has categories to file titles against.
pub async fn list_routines(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    user_id: &str,
) -> Result<RoutinesResponse, RoutinesError> {
    let lists = list_repo.list_by_user_id(user_id).await?;
    let categories = category_repo.list_by_user_id(user_id).await?;
    ensure_taxonomy(list_repo, category_repo, &lists, &categories, user_id).await?;
    let taxonomy = load_taxonomy(category_repo, user_id).await?;
    let routines = routine_repo.list_by_user_id(user_id).await?;
    let views = routines
        .iter()
        .map(|routine| to_view(routine, &taxonomy))
        .collect();
    Ok(RoutinesResponse { routines: views })
}

/// Creates a routine.
///
/// - `title` is trimmed and must not be empty AND must uniquely match a
///   non-untracked category — the exact `create_task` rules and messages.
/// - `estimated_minutes` defaults to [`DEFAULT_ESTIMATED_MINUTES`] and must be
///   >= [`MIN_ESTIMATED_MINUTES`].
/// - `rrule` (the two-line blob) is validated (invalid rule → 400
///   "invalid rrule").
/// - The new routine appends the standing order: `max(sort_order)+1`, or 0
///   when the pile is empty. Titles are not unique.
/// - Once the row is stored, the occurrence horizon (today … today+6) is
///   filled for it — so a brand-new routine shows on today's agenda without
///   any read having to write. A validation failure still writes nothing, and
///   a fill failure is swallowed: the routine is committed, so a 500 would
///   make the client retry and insert a second routine (the `*/15` cron
///   backfills the missing rows).
pub async fn create_routine(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    user_id: &str,
    input: &NewRoutineInput,
    occurrence_repo: &dyn OccurrenceRepo,
    agenda_repo: &dyn AgendaItemRepo,
    calendars: &dyn CalendarRepo,
    now_unix: i64,
) -> Result<RoutineResponse, RoutinesError> {
    let title = input.title.trim().to_string();
    if title.is_empty() {
        return Err(RoutinesError::Invalid("title must not be empty".to_string()));
    }
    let estimated_minutes = input.estimated_minutes.unwrap_or(DEFAULT_ESTIMATED_MINUTES);
    if estimated_minutes < MIN_ESTIMATED_MINUTES {
        return Err(RoutinesError::Invalid(
            "estimated_minutes must be at least 1".to_string(),
        ));
    }
    validate_recurrence(&input.rrule)?;

    // Seed the taxonomy like `create_task`, then enforce its unique-match
    // rules before anything is persisted.
    let lists = list_repo.list_by_user_id(user_id).await?;
    let categories = category_repo.list_by_user_id(user_id).await?;
    ensure_taxonomy(list_repo, category_repo, &lists, &categories, user_id).await?;
    let taxonomy = load_taxonomy(category_repo, user_id).await?;
    resolve_category(&title, &taxonomy)?;

    // Append the standing order; living peers keep their ranks.
    let max = routine_repo.max_sort_order(user_id).await?;
    let routine = routine_repo
        .insert(NewRoutine {
            user_id: user_id.to_string(),
            title,
            estimated_minutes,
            rrule: input.rrule.trim().to_string(),
            sort_order: append_rank(max),
        })
        .await?;
    // The horizon is materialized here, not on a read: fill the new routine's
    // today … today+6 occurrences (and their agenda rows) right away.
    if let Err(_) = fill_occurrence_horizon(
        routine_repo,
        occurrence_repo,
        agenda_repo,
        calendars,
        user_id,
        now_unix,
        Some(&routine.id),
        // Create owns today: the new routine shows on today's agenda at once.
        true,
    )
    .await
    {
        // Best-effort: the routine row is already committed, so a 500 would
        // make the client retry and insert a second routine. The `*/15` cron
        // backfills every missing row (and never repairs a half-written
        // Google log).
    }
    Ok(RoutineResponse {
        routine: to_view(&routine, &taxonomy),
    })
}

/// Updates a routine's `title`/`estimated_minutes`/`rrule`/`sort_order`
/// (`None` = unchanged).
///
/// - A body with nothing to update is 400 ("nothing to update").
/// - When `title` is present it must be non-blank AND uniquely match a
///   non-untracked category (same rules and messages as create).
/// - `estimated_minutes` must be >= 1 when present. A changed `rrule` blob is
///   validated on its own (it carries its own DTSTART).
/// - A missing, soft-deleted, or another user's routine is 404.
/// - A **changed** `rrule` (trimmed text differs from the stored blob)
///   reconciles the horizon **before the new blob is saved**: untouched rows
///   from **tomorrow through today+6** that the new rule no longer covers are
///   hard-deleted with their agenda rows. A reconcile failure therefore
///   returns with the old blob still stored — a retry still sees
///   `changed_rule` and prunes again. After the save the horizon is refilled
///   for **tomorrow through today+6** (`include_today: false`): the edit never
///   adds an occurrence or agenda row on today; the next `*/15` cron tick
///   fills today if the new rule covers it. That refill is best-effort — a
///   failure must not 500 an edit whose prune already happened. Today and the
///   past are never deleted or edited, and neither is any row the user
///   touched. A `None`/unchanged blob does no occurrence or agenda work at
///   all.
pub async fn update_routine(
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    user_id: &str,
    id: &str,
    updates: &UpdateRoutine,
    occurrence_repo: &dyn OccurrenceRepo,
    agenda_repo: &dyn AgendaItemRepo,
    calendars: &dyn CalendarRepo,
    now_unix: i64,
) -> Result<RoutineResponse, RoutinesError> {
    if updates.title.is_none()
        && updates.estimated_minutes.is_none()
        && updates.rrule.is_none()
        && updates.sort_order.is_none()
    {
        return Err(RoutinesError::Invalid("nothing to update".to_string()));
    }
    if let Some(estimated_minutes) = updates.estimated_minutes {
        if estimated_minutes < MIN_ESTIMATED_MINUTES {
            return Err(RoutinesError::Invalid(
                "estimated_minutes must be at least 1".to_string(),
            ));
        }
    }
    if let Some(title) = updates.title.as_deref() {
        if title.trim().is_empty() {
            return Err(RoutinesError::Invalid("title must not be empty".to_string()));
        }
    }

    let Some(existing) = routine_repo.get_by_id(id).await? else {
        return Err(RoutinesError::NotFound);
    };
    // `get_by_id` is intentionally not user-scoped; ownership is checked here
    // so another user's routine is a plain 404, never a leak.
    if existing.user_id != user_id {
        return Err(RoutinesError::NotFound);
    }

    // A changed blob is validated on its own — it carries its own DTSTART, so
    // there is no cross-field pair to check anymore.
    if let Some(rrule) = updates.rrule.as_deref() {
        validate_recurrence(rrule)?;
    }
    // A new title must classify to a single non-untracked category, exactly
    // like create. The old title needs no re-validation.
    let taxonomy = if updates.title.is_some() {
        let taxonomy = load_taxonomy(category_repo, user_id).await?;
        resolve_category(updates.title.as_deref().unwrap_or_default().trim(), &taxonomy)?;
        taxonomy
    } else {
        load_taxonomy(category_repo, user_id).await?
    };

    // Only a changed rule touches the materialized side. `None` (or a blob
    // that only differs in surrounding whitespace, which the insert already
    // trimmed away) is a title/estimate/sort-only update.
    let changed_rule = updates
        .rrule
        .as_deref()
        .map(|rrule| rrule.trim() != existing.rrule.trim())
        .unwrap_or(false);
    if changed_rule {
        // PRUNE BEFORE SAVING: the new rule text is what decides which
        // untouched rows survive — and if this fails, the stored blob is
        // still the old rule, so a retry still sees `changed_rule` and prunes
        // again. Saving first would make a failed prune unrepeatable.
        reconcile_occurrence_horizon(
            occurrence_repo,
            agenda_repo,
            calendars,
            user_id,
            &existing.id,
            updates.rrule.as_deref().unwrap_or_default().trim(),
            now_unix,
        )
        .await?;
    }

    let Some(updated) = routine_repo.update(id, updates).await? else {
        // Deleted between the read and the write.
        return Err(RoutinesError::NotFound);
    };

    if changed_rule {
        if let Err(_) = fill_occurrence_horizon(
            routine_repo,
            occurrence_repo,
            agenda_repo,
            calendars,
            user_id,
            now_unix,
            Some(&updated.id),
            // The edit skips today; the cron fills it on its next tick.
            false,
        )
        .await
        {
            // Best-effort refill from tomorrow: the prune already happened,
            // so a fill failure must not 500 an edit that landed. The next
            // `*/15` cron tick fills any missing date — including today, which
            // the edit deliberately leaves alone.
        }
    }
    Ok(RoutineResponse {
        routine: to_view(&updated, &taxonomy),
    })
}

/// SOFT deletes a routine, then HARD-deletes its **untouched** occurrences
/// dated **tomorrow through today+6** (and their agenda items).
///
/// Untouched means the fill's own row: still `pending`, no title override
/// (NULL, not `""`), no `google_event_id`, and not rescheduled (no agenda
/// item anywhere, or one still sitting on the occurrence's rule date). Today's
/// and past occurrences always stay in the database, pending ones included —
/// the range read already hides a deleted routine's pending/in_progress
/// leftovers and keeps its done/skipped history. No fill runs after a delete.
///
/// Missing or another user's routine → 404.
pub async fn delete_routine(
    repo: &dyn RoutineRepo,
    user_id: &str,
    id: &str,
    now_rfc3339: &str,
    occurrence_repo: &dyn OccurrenceRepo,
    agenda_repo: &dyn AgendaItemRepo,
    calendars: &dyn CalendarRepo,
    now_unix: i64,
) -> Result<DeleteRoutineResponse, RoutinesError> {
    let Some(routine) = repo.get_by_id(id).await? else {
        return Err(RoutinesError::NotFound);
    };
    if routine.user_id != user_id {
        return Err(RoutinesError::NotFound);
    }
    repo.soft_delete(id, now_rfc3339).await?;

    let (from, to) = reconcile_window(calendars, user_id, now_unix)
        .await
        .map_err(map_agenda_error)?;
    prune_untouched_occurrences(
        occurrence_repo,
        agenda_repo,
        user_id,
        id,
        &from,
        &to,
        None,
    )
    .await
    .map_err(map_agenda_error)?;
    Ok(DeleteRoutineResponse { success: true })
}

// ──────────────────────────────────────────
// Horizon reconcile (rule changes + deletes)
// ──────────────────────────────────────────

/// Starts the horizon: `today + 6`. Also used by the reconcile window.
fn horizon_end(today: &str) -> Result<String, AgendaError> {
    add_civil_days(today, OCCURRENCE_HORIZON_DAYS - 1)
}

/// The reconcile window: **tomorrow … today+6** inclusive. Today and every
/// earlier date are deliberately outside it — a rule change or a delete never
/// deletes or edits them.
async fn reconcile_window(
    calendars: &dyn CalendarRepo,
    user_id: &str,
    now_unix: i64,
) -> Result<(String, String), AgendaError> {
    let (today, _time_zone) = user_today(calendars, user_id, now_unix).await?;
    Ok((add_civil_days(&today, 1)?, horizon_end(&today)?))
}

/// Rewrites one routine's row for a **changed rule**: every untouched
/// occurrence in `[tomorrow, today+6]` that the new rule no longer covers is
/// hard-deleted together with its agenda item. `rrule` is the **new** blob —
/// the caller passes it before the routine row is saved, so a failure here
/// leaves the old rule stored and the next attempt prunes again. Nothing else
/// is touched — today, the past, dates outside the horizon (they do not
/// exist), rows the user acted on, and rows sitting on a date the new rule
/// still covers.
async fn reconcile_occurrence_horizon(
    occurrence_repo: &dyn OccurrenceRepo,
    agenda_repo: &dyn AgendaItemRepo,
    calendars: &dyn CalendarRepo,
    user_id: &str,
    routine_id: &str,
    rrule: &str,
    now_unix: i64,
) -> Result<(), RoutinesError> {
    let (from, to) = reconcile_window(calendars, user_id, now_unix)
        .await
        .map_err(map_agenda_error)?;
    // The new blob was validated moments ago; a surprise parse failure here
    // deletes nothing rather than guessing which dates to drop.
    let Ok(dates) = occurrence_dates(rrule, &from, &to) else {
        return Ok(());
    };
    let keep_dates: HashSet<String> = dates.into_iter().collect();
    prune_untouched_occurrences(
        occurrence_repo,
        agenda_repo,
        user_id,
        routine_id,
        &from,
        &to,
        Some(&keep_dates),
    )
    .await
    .map_err(map_agenda_error)
}

/// Hard-deletes a routine's untouched occurrences inside the inclusive
/// `[from, to]` window, plus their agenda items — the memberships first, so a
/// failure leaves the occurrences for a retry to prune again (the reverse
/// order could strand agenda rows; a missing membership is re-attached by the
/// next fill). `keep_dates` marks the civil dates the (new) rule still
/// produces: a row on one of them is never a candidate. `None` deletes every
/// untouched row in the window (the routine deletion path).
async fn prune_untouched_occurrences(
    occurrence_repo: &dyn OccurrenceRepo,
    agenda_repo: &dyn AgendaItemRepo,
    user_id: &str,
    routine_id: &str,
    from: &str,
    to: &str,
    keep_dates: Option<&HashSet<String>>,
) -> Result<(), AgendaError> {
    let window = occurrence_repo
        .list_by_user_and_date_range(user_id, from, to)
        .await?;
    let mut doomed: Vec<String> = Vec::new();
    for occurrence in window {
        if occurrence.routine_id != routine_id {
            continue;
        }
        if let Some(keep_dates) = keep_dates {
            if keep_dates.contains(&occurrence.local_date) {
                continue;
            }
        }
        if !is_untouched(&occurrence) {
            continue;
        }
        // An agenda item on another date means the user rescheduled this
        // instance: keep both the occurrence and its membership.
        if let Some(item) = agenda_repo
            .get_by_ref(user_id, AGENDA_KIND_OCCURRENCE, &occurrence.id)
            .await?
        {
            if item.local_date != occurrence.local_date {
                continue;
            }
        }
        doomed.push(occurrence.id);
    }
    if doomed.is_empty() {
        return Ok(());
    }
    // The memberships go FIRST: if their delete fails, the occurrences are
    // still there and a retry prunes again. Deleting occurrences first could
    // leave their agenda rows dangling forever; this way a failure after the
    // membership delete just means the next fill re-attaches a missing row —
    // the acceptable direction.
    agenda_repo
        .delete_by_refs(user_id, AGENDA_KIND_OCCURRENCE, &doomed)
        .await?;
    occurrence_repo.delete_by_ids(&doomed).await?;
    Ok(())
}

/// A row only the fill ever wrote: still `pending`, no title override (NULL,
/// not `""`), and no Google chip. Anything else is the user's and is never
/// deleted or rewritten by a fill/reconcile.
fn is_untouched(occurrence: &RoutineOccurrence) -> bool {
    occurrence.status == OCCURRENCE_STATUS_PENDING
        && occurrence.title.is_none()
        && occurrence.google_event_id.is_none()
}

/// Maps an [`AgendaError`] onto [`RoutinesError`]: a repository failure stays
/// a 500-shaped `Repo`, anything else (validation, calendar surprise) folds
/// into `Invalid` — the same HTTP mapping the agenda service uses.
fn map_agenda_error(err: AgendaError) -> RoutinesError {
    match err {
        AgendaError::Repo(err) => RoutinesError::Repo(err),
        other => RoutinesError::Invalid(other.to_string()),
    }
}

// ──────────────────────────────────────────
// Classification plumbing
// ──────────────────────────────────────────
//
// Deliberate duplication of `tasks.rs`'s private taxonomy helpers (slice
// brief: duplicate the classify match, do not refactor tasks.rs). If the task
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

/// Resolves a title to a single non-untracked category id — the `create_task`
/// rules verbatim:
/// - 0 matches → `title does not match a category`
/// - several matches (cross-tree conflict) → `title matches multiple categories`
/// - the matched category is the `untracked` sink → `title matches untracked`
///
/// A root matched without matching children resolves to the root (parent
/// remainder) — `classify` already drops parents beaten by their children.
fn resolve_category(title: &str, taxonomy: &Taxonomy) -> Result<String, RoutinesError> {
    match classify(title, CalendarScope::Ignore, &taxonomy.matchers) {
        ClassifyOutcome::Matched { category_id } => {
            if taxonomy
                .categories
                .iter()
                .any(|category| category.id == category_id && category.is_untracked)
            {
                return Err(RoutinesError::Invalid("title matches untracked".to_string()));
            }
            Ok(category_id)
        }
        ClassifyOutcome::Untracked { conflict: false } => Err(RoutinesError::Invalid(
            "title does not match a category".to_string(),
        )),
        ClassifyOutcome::Untracked { conflict: true } => Err(RoutinesError::Invalid(
            "title matches multiple categories".to_string(),
        )),
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

/// Wraps a stored routine into its HTTP view. Classification is a read: a
/// title that no longer uniquely matches keeps the `untracked` summary —
/// listing never 400s on classification.
fn to_view(routine: &Routine, taxonomy: &Taxonomy) -> RoutineView {
    let outcome = classify(&routine.title, CalendarScope::Ignore, &taxonomy.matchers);
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
    RoutineView {
        id: routine.id.clone(),
        user_id: routine.user_id.clone(),
        title: routine.title.clone(),
        estimated_minutes: routine.estimated_minutes,
        rrule: routine.rrule.clone(),
        sort_order: routine.sort_order,
        created_at: routine.created_at.clone(),
        updated_at: routine.updated_at.clone(),
        category,
    }
}

/// The append rank for a new routine: `max + 1`, or 0 when the pile is empty
/// (peers never shift) — the same convention as Backlog capture.
fn append_rank(max: Option<i64>) -> i64 {
    max.map(|m| m + 1).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    use super::*;
    use crate::models::{
        AgendaItem, GoogleCalendar, NewAgendaItem, NewTaskCategory, NewTaskCategoryPattern,
        NewTaskList, NewCalendar, NewRoutineOccurrence, RoutineOccurrence, TaskList,
        UpdateTaskCategory, UpdateTaskList,
    };

    const NOW: &str = "2026-08-23T00:00:00Z";
    /// The test clock: `2026-08-23` in UTC (the fake calendar repo has no
    /// primary, so `user_today` falls back to UTC).
    const NOW_UNIX: i64 = 1_787_479_200;

    // ──────────────────────────────────────────
    // Fakes
    // ──────────────────────────────────────────

    /// In-memory `RoutineRepo` mirroring D1 semantics: soft-deleted rows are
    /// filtered from reads, inserts mint ids/timestamps.
    struct FakeRoutineRepo {
        stored: Mutex<Vec<Routine>>,
        inserted: Mutex<Vec<NewRoutine>>,
        next_id: Mutex<u64>,
    }

    impl FakeRoutineRepo {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                inserted: Mutex::new(Vec::new()),
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
            // Mirrors ROUTINE_LIST_BY_USER_ID_SQL.
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
            self.inserted.lock().unwrap().push(NewRoutine {
                user_id: row.user_id.clone(),
                title: row.title.clone(),
                estimated_minutes: row.estimated_minutes,
                rrule: row.rrule.clone(),
                sort_order: row.sort_order,
            });
            self.stored.lock().unwrap().push(row.clone());
            Ok(row)
        }

        async fn update(
            &self,
            id: &str,
            updates: &UpdateRoutine,
        ) -> Result<Option<Routine>, RepoError> {
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
                row.updated_at = now_rfc3339.to_string();
            }
            Ok(())
        }

        async fn max_sort_order(&self, user_id: &str) -> Result<Option<i64>, RepoError> {
            // Mirrors ROUTINE_MAX_SORT_ORDER_SQL: highest living rank.
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
            // owners.
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

        async fn list_by_ids_including_deleted(
            &self,
            ids: &[String],
        ) -> Result<Vec<Routine>, RepoError> {
            // Mirrors build_routine_list_by_ids_sql: id-scoped, NO deleted
            // filter, empty input touches nothing.
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

    /// In-memory `TaskListRepo` for the taxonomy seed path.
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
    /// `ensure_taxonomy` and the matcher work end to end.
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

        /// Adds a raw category with patterns (for conflict/untracked cases).
        fn add_category(
            &self,
            user_id: &str,
            title: &str,
            slug: &str,
            is_untracked: bool,
            regexes: &[&str],
        ) {
            let mut next = self.next_id.lock().unwrap();
            let id = format!("cat-x{next}");
            *next += 1;
            drop(next);
            self.stored.lock().unwrap().push(crate::models::TaskCategory {
                id: id.clone(),
                user_id: user_id.to_string(),
                list_id: None,
                parent_id: None,
                title: title.to_string(),
                slug: slug.to_string(),
                color: "#000000".to_string(),
                is_productive: false,
                google_calendar_id: None,
                sort_order: 99,
                is_untracked,
                created_at: "2026-08-18T00:00:00Z".to_string(),
                updated_at: "2026-08-18T00:00:00Z".to_string(),
                deleted_at: None,
            });
            let pattern_id = id.clone();
            self.patterns.lock().unwrap().insert(
                id,
                regexes
                    .iter()
                    .enumerate()
                    .map(|(i, regex)| crate::models::TaskCategoryPattern {
                        id: format!("{pattern_id}-pat-{i}"),
                        category_id: pattern_id.clone(),
                        regex: regex.to_string(),
                        google_calendar_id: None,
                        sort_order: i as i64,
                        created_at: "2026-08-18T00:00:00Z".to_string(),
                        updated_at: "2026-08-18T00:00:00Z".to_string(),
                    })
                    .collect(),
            );
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
            // Mirrors TASK_CATEGORY_PATTERNS_LIST_BY_USER_ID_SQL: only living
            // categories' patterns.
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

    // ──────────────────────────────────────────
    // Agenda fakes (the fill runs on create/update/delete)
    // ──────────────────────────────────────────

    /// A user with no calendars: `user_today` falls back to UTC, so `NOW_UNIX`
    /// reads as the civil date `2026-08-23`. Every other method is unreachable
    /// from the create/update/delete paths.
    struct FakeCalendarRepo;

    #[async_trait::async_trait(?Send)]
    impl CalendarRepo for FakeCalendarRepo {
        async fn list_by_user_id(&self, _user_id: &str) -> Result<Vec<GoogleCalendar>, RepoError> {
            Ok(Vec::new())
        }

        async fn list_sync_enabled(&self) -> Result<Vec<GoogleCalendar>, RepoError> {
            unimplemented!("the routines service never lists sync-enabled calendars")
        }

        async fn get_by_id(&self, _id: &str) -> Result<Option<GoogleCalendar>, RepoError> {
            unimplemented!("the routines service never loads a calendar by id")
        }

        async fn get_by_id_unfiltered(&self, _id: &str) -> Result<Option<GoogleCalendar>, RepoError> {
            unimplemented!("the routines service never loads a calendar by id")
        }

        async fn get_by_google_cal_id(
            &self,
            _user_id: &str,
            _google_cal_id: &str,
        ) -> Result<Option<GoogleCalendar>, RepoError> {
            unimplemented!("the routines service never resolves a Google calendar id")
        }

        async fn upsert(&self, _calendar: NewCalendar) -> Result<(), RepoError> {
            unimplemented!("the routines service never writes calendars")
        }

        async fn upsert_batch(&self, _calendars: Vec<NewCalendar>) -> Result<(), RepoError> {
            unimplemented!("the routines service never writes calendars")
        }

        async fn update_sync_state(
            &self,
            _id: &str,
            _sync_token: &str,
            _last_synced_at_rfc3339: &str,
        ) -> Result<(), RepoError> {
            unimplemented!("the routines service never touches sync state")
        }

        async fn record_sync_attempt(&self, _id: &str, _now_rfc3339: &str) -> Result<(), RepoError> {
            unimplemented!("the routines service never touches sync state")
        }

        async fn record_sync_success(
            &self,
            _id: &str,
            _sync_token: &str,
            _query_fingerprint: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            unimplemented!("the routines service never touches sync state")
        }

        async fn record_sync_success_if_owner(
            &self,
            _id: &str,
            _sync_token: &str,
            _query_fingerprint: &str,
            _lease_owner: &str,
            _now_rfc3339: &str,
        ) -> Result<bool, RepoError> {
            unimplemented!("the routines service never touches sync state")
        }

        async fn record_sync_failure(
            &self,
            _id: &str,
            _error_code: &str,
            _sync_status: &str,
            _next_retry_rfc3339: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            unimplemented!("the routines service never touches sync state")
        }

        async fn record_sync_contention(
            &self,
            _id: &str,
            _error_code: &str,
            _sync_status: &str,
            _next_retry_rfc3339: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            unimplemented!("the routines service never touches sync state")
        }

        async fn begin_replica_reseed(&self, _id: &str, _now_rfc3339: &str) -> Result<(), RepoError> {
            unimplemented!("the routines service never touches sync state")
        }

        async fn try_acquire_lease(
            &self,
            _id: &str,
            _owner: &str,
            _now_rfc3339: &str,
            _expires_rfc3339: &str,
        ) -> Result<bool, RepoError> {
            unimplemented!("the routines service never touches leases")
        }

        async fn release_lease(
            &self,
            _id: &str,
            _owner: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            unimplemented!("the routines service never touches leases")
        }

        async fn renew_lease(
            &self,
            _id: &str,
            _owner: &str,
            _expires_rfc3339: &str,
            _now_rfc3339: &str,
        ) -> Result<bool, RepoError> {
            unimplemented!("the routines service never touches leases")
        }

        async fn bump_dirty_requested(&self, _id: &str, _now_rfc3339: &str) -> Result<(), RepoError> {
            unimplemented!("the routines service never touches sync state")
        }

        async fn mark_dirty_applied(
            &self,
            _id: &str,
            _generation: i64,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            unimplemented!("the routines service never touches sync state")
        }

        async fn set_sync_enabled(
            &self,
            _id: &str,
            _enabled: bool,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            unimplemented!("the routines service never touches sync state")
        }

        async fn set_event_labels(
            &self,
            _id: &str,
            _event_labels_json: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            unimplemented!("the routines service never caches event labels")
        }

        async fn set_watch_coverage(
            &self,
            _id: &str,
            _coverage: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            unimplemented!("the routines service never touches watch coverage")
        }

        async fn set_event_coverage(
            &self,
            _id: &str,
            _coverage: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            unimplemented!("the routines service never touches event coverage")
        }

        async fn delete(&self, _id: &str, _now_rfc3339: &str) -> Result<(), RepoError> {
            unimplemented!("the routines service never deletes calendars")
        }

        async fn get_calendar_list_sync_token(
            &self,
            _user_id: &str,
        ) -> Result<Option<String>, RepoError> {
            unimplemented!("the routines service never reads the calendar-list cursor")
        }

        async fn set_calendar_list_sync_token(
            &self,
            _user_id: &str,
            _token: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            unimplemented!("the routines service never writes the calendar-list cursor")
        }

        async fn list_user_ids_with_calendars(&self) -> Result<Vec<String>, RepoError> {
            unimplemented!("the routines service never walks calendar owners")
        }

        async fn clear_authorization_required_for_user(
            &self,
            _user_id: &str,
            _next_retry_rfc3339: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            unimplemented!("the routines service never clears authorization state")
        }
    }

    /// In-memory `OccurrenceRepo`: no soft-delete (the table has none);
    /// `insert_many` mirrors `INSERT OR IGNORE` on `(routine_id, local_date)`
    /// and `delete_by_ids` mirrors the HARD reconcile delete.
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
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.local_date == local_date)
                .cloned()
                .collect())
        }

        async fn list_by_user_and_date_range(
            &self,
            user_id: &str,
            from: &str,
            to: &str,
        ) -> Result<Vec<RoutineOccurrence>, RepoError> {
            // Mirrors OCCURRENCE_LIST_BY_USER_AND_DATE_RANGE_SQL.
            let mut rows: Vec<RoutineOccurrence> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| {
                    row.user_id == user_id
                        && row.local_date.as_str() >= from
                        && row.local_date.as_str() <= to
                })
                .cloned()
                .collect();
            rows.sort_by(|a, b| a.local_date.cmp(&b.local_date));
            Ok(rows)
        }

        async fn insert(&self, occurrence: NewRoutineOccurrence) -> Result<RoutineOccurrence, RepoError> {
            let mut stored = self.stored.lock().unwrap();
            if let Some(existing) = stored.iter().find(|row| {
                row.routine_id == occurrence.routine_id && row.local_date == occurrence.local_date
            }) {
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

        async fn list_by_ids(&self, ids: &[String]) -> Result<Vec<RoutineOccurrence>, RepoError> {
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

        async fn insert_many(&self, rows: Vec<NewRoutineOccurrence>) -> Result<(), RepoError> {
            for row in rows {
                self.insert(row).await?;
            }
            Ok(())
        }

        async fn delete_by_ids(&self, ids: &[String]) -> Result<(), RepoError> {
            // Mirrors build_occurrence_delete_by_ids_sql: HARD and id-scoped;
            // an empty list touches nothing.
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
                row.title = title.map(|value| value.to_string());
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
            }
            Ok(())
        }

        async fn clear_event_ids(&self, id: &str) -> Result<(), RepoError> {
            if let Some(row) = self.stored.lock().unwrap().iter_mut().find(|row| row.id == id) {
                row.calendar_id = None;
                row.google_event_id = None;
            }
            Ok(())
        }

        async fn set_status(&self, id: &str, status: &str) -> Result<(), RepoError> {
            if let Some(row) = self.stored.lock().unwrap().iter_mut().find(|row| row.id == id) {
                row.status = status.to_string();
            }
            Ok(())
        }

        async fn list_in_progress(&self) -> Result<Vec<RoutineOccurrence>, RepoError> {
            unimplemented!("the routines service never runs the elongate cron")
        }
    }

    /// In-memory `AgendaItemRepo`: membership rows are hard-deleted (unpin),
    /// `insert_many` mirrors `INSERT OR IGNORE` on the UNIQUE key.
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

        /// The stored rows of one day, in pile order — the append assertions.
        fn pile(&self, user_id: &str, date: &str) -> Vec<AgendaItem> {
            let mut rows: Vec<AgendaItem> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.local_date == date)
                .cloned()
                .collect();
            rows.sort_by(|a, b| (a.sort_order, &a.created_at).cmp(&(b.sort_order, &b.created_at)));
            rows
        }
    }

    #[async_trait::async_trait(?Send)]
    impl AgendaItemRepo for FakeAgendaItemRepo {
        async fn list_by_user_and_date(
            &self,
            user_id: &str,
            local_date: &str,
        ) -> Result<Vec<AgendaItem>, RepoError> {
            Ok(self.pile(user_id, local_date))
        }

        async fn list_by_user_and_date_range(
            &self,
            user_id: &str,
            from: &str,
            to: &str,
        ) -> Result<Vec<AgendaItem>, RepoError> {
            let mut rows: Vec<AgendaItem> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| {
                    row.user_id == user_id
                        && row.local_date.as_str() >= from
                        && row.local_date.as_str() <= to
                })
                .cloned()
                .collect();
            rows.sort_by(|a, b| {
                (&a.local_date, a.sort_order).cmp(&(&b.local_date, b.sort_order))
            });
            Ok(rows)
        }

        async fn min_local_date(&self, user_id: &str) -> Result<Option<String>, RepoError> {
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
            _user_id: &str,
            _before: &str,
        ) -> Result<Option<String>, RepoError> {
            unimplemented!("the routines service never computes the stream anchor")
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

        async fn get_by_ref(
            &self,
            user_id: &str,
            kind: &str,
            ref_id: &str,
        ) -> Result<Option<AgendaItem>, RepoError> {
            // Mirrors AGENDA_ITEM_GET_BY_REF_SQL: ANY date (a rescheduled row
            // lives on another day), LIMIT 1.
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|row| row.user_id == user_id && row.kind == kind && row.ref_id == ref_id)
                .cloned())
        }

        async fn insert(&self, item: NewAgendaItem) -> Result<AgendaItem, RepoError> {
            let mut stored = self.stored.lock().unwrap();
            // idx_agenda_items_one_per_task is `(user_id, ref_id) WHERE kind =
            // 'task'`: a task holds ONE row on ANY date, so a second insert for
            // the same task is ignored too — the stored date and rank win.
            if item.kind == "task" {
                if let Some(existing) = stored.iter().find(|row| {
                    row.user_id == item.user_id
                        && row.kind == item.kind
                        && row.ref_id == item.ref_id
                }) {
                    return Ok(existing.clone());
                }
            }
            // INSERT OR IGNORE: the UNIQUE key already holds a row → return it
            // unchanged (an append never reshuffles an existing item).
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

        async fn list_by_refs(
            &self,
            user_id: &str,
            kind: &str,
            ref_ids: &[String],
        ) -> Result<Vec<AgendaItem>, RepoError> {
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
                    row.user_id == user_id
                        && row.kind == kind
                        && ref_set.contains(row.ref_id.as_str())
                })
                .cloned()
                .collect())
        }

        async fn insert_many(&self, rows: Vec<NewAgendaItem>) -> Result<(), RepoError> {
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
            self.stored.lock().unwrap().retain(|row| {
                !(row.user_id == user_id && row.kind == kind && row.ref_id == ref_id)
            });
            Ok(())
        }

        async fn delete_by_refs(
            &self,
            user_id: &str,
            kind: &str,
            ref_ids: &[String],
        ) -> Result<(), RepoError> {
            // Mirrors build_agenda_item_delete_by_refs_sql: every matching row
            // of the user's `kind`; empty input touches nothing.
            if ref_ids.is_empty() {
                return Ok(());
            }
            let ref_set: HashSet<&str> = ref_ids.iter().map(|id| id.as_str()).collect();
            self.stored.lock().unwrap().retain(|row| {
                !(row.user_id == user_id
                    && row.kind == kind
                    && ref_set.contains(row.ref_id.as_str()))
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
            for row in self.stored.lock().unwrap().iter_mut() {
                if row.user_id == user_id && row.local_date == local_date && row.sort_order >= from_rank
                {
                    row.sort_order += delta;
                }
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
        calendars: FakeCalendarRepo,
    }

    fn repos() -> Repos {
        Repos {
            lists: FakeTaskListRepo::new(),
            categories: FakeTaskCategoryRepo::new(),
            routines: FakeRoutineRepo::new(),
            occurrences: FakeOccurrenceRepo::new(),
            agenda: FakeAgendaItemRepo::new(),
            // No calendars → UTC today (`NOW_UNIX` is `2026-08-23`).
            calendars: FakeCalendarRepo,
        }
    }

    /// Composes the two-line recurrence blob from the hyphenated civil
    /// datetime used across these tests (the API accepts the blob as-is).
    fn blob(dtstart: &str, body: &str) -> String {
        let compact: String = dtstart
            .chars()
            .filter(|c| *c != '-' && *c != ':')
            .collect();
        format!("DTSTART:{compact}\nRRULE:{body}")
    }

    fn input(title: &str, rrule: &str) -> NewRoutineInput {
        NewRoutineInput {
            title: title.to_string(),
            estimated_minutes: None,
            rrule: rrule.to_string(),
        }
    }

    // ──────────────────────────────────────────
    // List + seed
    // ──────────────────────────────────────────

    #[test]
    fn list_seeds_taxonomy_and_returns_views_with_computed_category() {
        let repos = repos();
        // A stored row whose title matches nothing (e.g. created before the
        // pattern was deleted): listing must still return it, untracked.
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Fajr",
            0,
            "DTSTART:20260101T053000\nRRULE:FREQ=DAILY",
        ));

        let response =
            pollster::block_on(list_routines(&repos.lists, &repos.categories, &repos.routines, "u-1"))
                .unwrap();
        assert_eq!(response.routines.len(), 1);
        let routine = &response.routines[0];
        assert_eq!(routine.title, "Fajr");
        assert_eq!(routine.estimated_minutes, 15, "default estimate");
        assert_eq!(
            routine.rrule, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY",
            "the stored blob round-trips"
        );
        // Seeded roots exist but none match "Fajr" → untracked summary.
        assert!(routine.category.is_untracked);
        assert_eq!(routine.category.slug, "untracked");
        // Taxonomy seeded once (four lists + four roots + untracked).
        assert_eq!(repos.lists.stored.lock().unwrap().len(), 4);
        assert_eq!(repos.categories.stored.lock().unwrap().len(), 5);
    }

    #[test]
    fn list_orders_by_standing_sort_order() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().extend([
            FakeRoutineRepo::row("rt-b", "u-1", "Second", 2, "DTSTART:20260102T053000\nRRULE:FREQ=DAILY"),
            FakeRoutineRepo::row("rt-a", "u-1", "First", 1, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"),
            FakeRoutineRepo::row("rt-x", "u-2", "Other user", 0, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY"),
        ]);
        let response =
            pollster::block_on(list_routines(&repos.lists, &repos.categories, &repos.routines, "u-1"))
                .unwrap();
        let ids: Vec<&str> = response.routines.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["rt-a", "rt-b"]);
    }

    #[test]
    fn list_keeps_title_that_no_longer_matches_with_untracked_summary() {
        let repos = repos();
        repos.routines.stored.lock().unwrap().push(FakeRoutineRepo::row(
            "rt-1",
            "u-1",
            "Vanished Pattern",
            0,
            "DTSTART:20260101T053000\nRRULE:FREQ=DAILY",
        ));
        let response =
            pollster::block_on(list_routines(&repos.lists, &repos.categories, &repos.routines, "u-1"))
                .unwrap();
        assert_eq!(response.routines.len(), 1);
        assert!(response.routines[0].category.is_untracked);
    }

    // ──────────────────────────────────────────
    // Create
    // ──────────────────────────────────────────

    #[test]
    fn create_happy_path_classifies_and_appends_rank() {
        let repos = repos();
        // First create lands at rank 0…
        let first = pollster::block_on(create_routine(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            "u-1",
            &input("Work", "DTSTART:20260101T063000\nRRULE:FREQ=DAILY;INTERVAL=2"),
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap();
        assert_eq!(first.routine.sort_order, 0);
        assert_eq!(first.routine.category.title, "Work", "root match (parent remainder)");
        assert!(!first.routine.category.is_untracked);

        // …second appends at max+1 even though ranks are shared per pile.
        repos
            .categories
            .add_category("u-1", "Salat", "salat", false, &["^Salat$"]);
        let second = pollster::block_on(create_routine(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            "u-1",
            &NewRoutineInput {
                title: "Salat".to_string(),
                estimated_minutes: Some(10),
                rrule: blob("2026-01-01T05:30:00", "FREQ=DAILY"),
            },
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap();
        assert_eq!(second.routine.sort_order, 1, "append rank max+1");
        assert_eq!(second.routine.estimated_minutes, 10);
        assert_eq!(
            second.routine.rrule, "DTSTART:20260101T053000\nRRULE:FREQ=DAILY",
            "the blob round-trips"
        );
        // Titles are not unique: a second "Salat" is allowed…
        let duplicate = pollster::block_on(create_routine(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            "u-1",
            &input("Salat", "DTSTART:20260201T053000\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE"),
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap();
        assert_eq!(duplicate.routine.sort_order, 2);
    }

    #[test]
    fn create_rejects_blank_title_invalid_estimate_bad_recurrence() {
        let repos = repos();
        let good = blob("2026-01-01T05:30:00", "FREQ=DAILY");
        let cases = [
            (input("", &good), "title must not be empty"),
            (input("   ", &good), "title must not be empty"),
        ];
        for (body, message) in cases {
            let err = pollster::block_on(create_routine(
                &repos.lists,
                &repos.categories,
                &repos.routines,
                "u-1",
                &body,
                &repos.occurrences,
                &repos.agenda,
                &repos.calendars,
                NOW_UNIX,
            ))
            .unwrap_err();
            assert!(matches!(err, RoutinesError::Invalid(ref m) if m == message), "{err:?}");
        }
        let zero_estimate = NewRoutineInput {
            estimated_minutes: Some(0),
            ..input("Work", &good)
        };
        let err = pollster::block_on(create_routine(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            "u-1",
            &zero_estimate,
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap_err();
        assert!(
            matches!(err, RoutinesError::Invalid(ref m) if m == "estimated_minutes must be at least 1"),
            "{err:?}"
        );
        for bad in [
            "",
            "FREQ=DAILY", // missing the DTSTART line
            "DTSTART:20260101T053000", // missing the RRULE line
            "DTSTART:20260101T053000\nRRULE:FREQ=DAILY\nRRULE:FREQ=WEEKLY", // two RRULEs
            "DTSTART:20260101T053000\nFREQ=DAILY", // second line not RRULE:
            "DTSTART:20260101T053000Z\nRRULE:FREQ=DAILY", // Z on DTSTART
            "DTSTART:2026-01-01T05:30:00\nRRULE:FREQ=DAILY", // not the basic form
            "DTSTART:20260101T053000\nRRULE:FREQ=DAILY;EXDATE:20260102", // exdate
            "DTSTART:20260101T053000\nRRULE:FREQ=DAILY;RDATE:20260102", // rdate
            "DTSTART;TZID=Asia/Kolkata:20260101T053000\nRRULE:FREQ=DAILY", // tzid
            "DTSTART:20260101T053000\nRRULE:NOT_A_RULE=1",
            "DTSTART:20260101T053000\nRRULE:",
        ] {
            let err = pollster::block_on(create_routine(
                &repos.lists,
                &repos.categories,
                &repos.routines,
                "u-1",
                &input("Work", bad),
                &repos.occurrences,
                &repos.agenda,
                &repos.calendars,
                NOW_UNIX,
            ))
            .unwrap_err();
            assert!(matches!(err, RoutinesError::Invalid(_)), "want 400, got {err:?} for {bad:?}");
        }
        assert!(
            repos.routines.inserted.lock().unwrap().is_empty(),
            "nothing persisted on validation failure"
        );
    }

    #[test]
    fn create_rejects_rrule_with_exrule_and_extra_lines() {
        let repos = repos();
        // The locked-format rejections beyond the bad-body cases above.
        for bad in [
            // A trailing newline is trimmed away — still exactly two lines.
            "DTSTART:20260101T053000\nRRULE:FREQ=DAILY\nEXTRA", // third line
            "DTSTART:20260101T053000\nRRULE:FREQ=DAILY;EXRULE:FREQ=WEEKLY",
        ] {
            let err = pollster::block_on(create_routine(
                &repos.lists,
                &repos.categories,
                &repos.routines,
                "u-1",
                &input("Work", bad),
                &repos.occurrences,
                &repos.agenda,
                &repos.calendars,
                NOW_UNIX,
            ))
            .unwrap_err();
            assert!(matches!(err, RoutinesError::Invalid(_)), "{err:?} for {bad:?}");
        }
        assert!(repos.routines.inserted.lock().unwrap().is_empty());
    }

    #[test]
    fn create_enforces_the_same_classify_rules_as_create_task() {
        let repos = repos();
        let good = blob("2026-01-01T05:30:00", "FREQ=DAILY");
        // 0 matches.
        let err = pollster::block_on(create_routine(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            "u-1",
            &input("Meditation", &good),
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap_err();
        assert!(
            matches!(err, RoutinesError::Invalid(ref m) if m == "title does not match a category"),
            "{err:?}"
        );

        // Seed + two custom overlapping roots → cross-tree conflict.
        let _ = pollster::block_on(list_routines(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            "u-1",
        ))
        .unwrap();
        repos
            .categories
            .add_category("u-1", "Dupe A", "dupe-a", false, &["^Dupe$"]);
        repos
            .categories
            .add_category("u-1", "Dupe B", "dupe-b", false, &["^Dupe$"]);
        let err = pollster::block_on(create_routine(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            "u-1",
            &input("Dupe", &good),
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap_err();
        assert!(
            matches!(err, RoutinesError::Invalid(ref m) if m == "title matches multiple categories"),
            "{err:?}"
        );

        // Matching the untracked sink itself is invalid.
        repos
            .categories
            .add_category("u-1", "Weird", "weird-sink", true, &["^Weird$"]);
        let err = pollster::block_on(create_routine(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            "u-1",
            &input("Weird", &good),
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap_err();
        assert!(
            matches!(err, RoutinesError::Invalid(ref m) if m == "title matches untracked"),
            "{err:?}"
        );
        assert!(repos.routines.inserted.lock().unwrap().is_empty());
    }

    // ──────────────────────────────────────────
    // Update
    // ──────────────────────────────────────────

    #[test]
    fn update_applies_partial_updates_and_revalidates_blob() {
        let repos = repos();
        let created = pollster::block_on(create_routine(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            "u-1",
            &input("Work", &blob("2026-01-01T06:30:00", "FREQ=DAILY")),
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap();
        let id = created.routine.id.clone();

        // Estimate + rule only: title untouched. The blob replaces the whole
        // recurrence (it carries its own DTSTART).
        let updates = UpdateRoutine {
            estimated_minutes: Some(20),
            rrule: Some(blob("2026-01-05T09:00:00", "FREQ=WEEKLY;BYDAY=MO,WE")),
            ..UpdateRoutine::default()
        };
        let updated = pollster::block_on(update_routine(
            &repos.categories,
            &repos.routines,
            "u-1",
            &id,
            &updates,
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap();
        assert_eq!(updated.routine.estimated_minutes, 20);
        assert_eq!(
            updated.routine.rrule, "DTSTART:20260105T090000\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE",
            "the blob round-trips"
        );

        // A bad replacement blob is refused; the stored rule stays intact.
        let invalid_rule = UpdateRoutine {
            rrule: Some("FREQ=DAILY".to_string()),
            ..UpdateRoutine::default()
        };
        let err = pollster::block_on(update_routine(
            &repos.categories,
            &repos.routines,
            "u-1",
            &id,
            &invalid_rule,
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap_err();
        assert!(matches!(err, RoutinesError::Invalid(_)), "{err:?}");
        let stored = pollster::block_on(repos.routines.get_by_id(&id)).unwrap().unwrap();
        assert_eq!(
            stored.rrule, "DTSTART:20260105T090000\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE",
            "refused update writes nothing"
        );
    }

    #[test]
    fn update_400s_on_empty_body_bad_values_and_bad_titles() {
        let repos = repos();
        let created = pollster::block_on(create_routine(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            "u-1",
            &input("Work", "DTSTART:20260101T063000\nRRULE:FREQ=DAILY"),
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap();
        let id = created.routine.id.clone();

        let err = pollster::block_on(update_routine(
            &repos.categories,
            &repos.routines,
            "u-1",
            &id,
            &UpdateRoutine::default(),
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap_err();
        assert!(
            matches!(err, RoutinesError::Invalid(ref m) if m == "nothing to update"),
            "{err:?}"
        );

        let zero = UpdateRoutine {
            estimated_minutes: Some(0),
            ..UpdateRoutine::default()
        };
        let err = pollster::block_on(update_routine(
            &repos.categories,
            &repos.routines,
            "u-1",
            &id,
            &zero,
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap_err();
        assert!(
            matches!(err, RoutinesError::Invalid(ref m) if m == "estimated_minutes must be at least 1"),
            "{err:?}"
        );

        let invalid_rule = UpdateRoutine {
            rrule: Some("garbage".to_string()),
            ..UpdateRoutine::default()
        };
        let err = pollster::block_on(update_routine(
            &repos.categories,
            &repos.routines,
            "u-1",
            &id,
            &invalid_rule,
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap_err();
        assert!(matches!(err, RoutinesError::Invalid(_)), "{err:?}");

        let blank_title = UpdateRoutine {
            title: Some("   ".to_string()),
            ..UpdateRoutine::default()
        };
        let err = pollster::block_on(update_routine(
            &repos.categories,
            &repos.routines,
            "u-1",
            &id,
            &blank_title,
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap_err();
        assert!(
            matches!(err, RoutinesError::Invalid(ref m) if m == "title must not be empty"),
            "{err:?}"
        );

        let unclassifiable = UpdateRoutine {
            title: Some("No Match At All".to_string()),
            ..UpdateRoutine::default()
        };
        let err = pollster::block_on(update_routine(
            &repos.categories,
            &repos.routines,
            "u-1",
            &id,
            &unclassifiable,
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap_err();
        assert!(
            matches!(err, RoutinesError::Invalid(ref m) if m == "title does not match a category"),
            "{err:?}"
        );
    }

    #[test]
    fn update_404s_for_missing_soft_deleted_and_other_users_routine() {
        let repos = repos();
        let created = pollster::block_on(create_routine(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            "u-1",
            &input("Work", "DTSTART:20260101T063000\nRRULE:FREQ=DAILY"),
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap();
        let id = created.routine.id.clone();

        let rename = UpdateRoutine {
            title: Some("Renamed".to_string()),
            ..UpdateRoutine::default()
        };
        // Another user's routine: 404, never a leak.
        assert!(matches!(
            pollster::block_on(update_routine(
                &repos.categories,
                &repos.routines,
                "u-2",
                &id,
                &rename,
                &repos.occurrences,
                &repos.agenda,
                &repos.calendars,
                NOW_UNIX,
            )),
            Err(RoutinesError::NotFound)
        ));
        // Missing id.
        assert!(matches!(
            pollster::block_on(update_routine(
                &repos.categories,
                &repos.routines,
                "u-1",
                "nope",
                &rename,
                &repos.occurrences,
                &repos.agenda,
                &repos.calendars,
                NOW_UNIX,
            )),
            Err(RoutinesError::NotFound)
        ));
        // Soft-deleted: get_by_id filters it out.
        pollster::block_on(delete_routine(&repos.routines, "u-1", &id, NOW, &repos.occurrences, &repos.agenda, &repos.calendars, NOW_UNIX)).unwrap();
        assert!(matches!(
            pollster::block_on(update_routine(
                &repos.categories,
                &repos.routines,
                "u-1",
                &id,
                &rename,
                &repos.occurrences,
                &repos.agenda,
                &repos.calendars,
                NOW_UNIX,
            )),
            Err(RoutinesError::NotFound)
        ));
    }

    // ──────────────────────────────────────────
    // Delete
    // ──────────────────────────────────────────

    #[test]
    fn delete_is_soft_and_404s_again_afterwards() {
        let repos = repos();
        let created = pollster::block_on(create_routine(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            "u-1",
            &input("Work", "DTSTART:20260101T063000\nRRULE:FREQ=DAILY"),
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap();
        let id = created.routine.id.clone();

        let response = pollster::block_on(delete_routine(&repos.routines, "u-1", &id, NOW, &repos.occurrences, &repos.agenda, &repos.calendars, NOW_UNIX)).unwrap();
        assert!(response.success);
        // Soft delete: the row survives with deleted_at stamped…
        assert_eq!(repos.routines.stored.lock().unwrap().len(), 1);
        assert!(repos.routines.stored.lock().unwrap()[0].deleted_at.is_some());
        // …reads stop seeing it…
        assert_eq!(
            pollster::block_on(repos.routines.max_sort_order("u-1")).unwrap(),
            None
        );
        // …and a second delete is a 404.
        assert!(matches!(
            pollster::block_on(delete_routine(&repos.routines, "u-1", &id, NOW, &repos.occurrences, &repos.agenda, &repos.calendars, NOW_UNIX)),
            Err(RoutinesError::NotFound)
        ));
    }

    #[test]
    fn delete_404s_for_missing_and_other_users_routine() {
        let repos = repos();
        let created = pollster::block_on(create_routine(
            &repos.lists,
            &repos.categories,
            &repos.routines,
            "u-1",
            &input("Work", "DTSTART:20260101T063000\nRRULE:FREQ=DAILY"),
            &repos.occurrences,
            &repos.agenda,
            &repos.calendars,
            NOW_UNIX,
        ))
        .unwrap();
        let id = created.routine.id.clone();
        assert!(matches!(
            pollster::block_on(delete_routine(&repos.routines, "u-1", "nope", NOW, &repos.occurrences, &repos.agenda, &repos.calendars, NOW_UNIX)),
            Err(RoutinesError::NotFound)
        ));
        assert!(matches!(
            pollster::block_on(delete_routine(&repos.routines, "u-2", &id, NOW, &repos.occurrences, &repos.agenda, &repos.calendars, NOW_UNIX)),
            Err(RoutinesError::NotFound)
        ));
        // The routine survived both attempts.
        assert!(repos.routines.stored.lock().unwrap()[0].deleted_at.is_none());
    }

    // ──────────────────────────────────────────
    // Golden fixtures
    // ──────────────────────────────────────────

    /// One case of `fixtures/rrule_golden.json`. Shared with the npm-side
    /// runner in slice 3 — the two engines must agree byte-for-byte.
    #[derive(Debug, serde::Deserialize)]
    struct GoldenFixtures {
        cases: Vec<GoldenCase>,
    }

    #[derive(Debug, serde::Deserialize)]
    #[allow(dead_code)] // `name` is documentation for the npm side too
    struct GoldenCase {
        name: String,
        /// The two-line recurrence blob (`DTSTART:` + `RRULE:`).
        rrule: String,
        from: String,
        to: String,
        expected: Vec<String>,
    }

    #[test]
    fn golden_fixtures_hold_for_every_case() {
        let fixtures: GoldenFixtures = serde_json::from_str(include_str!(
            "../fixtures/rrule_golden.json"
        ))
        .expect("golden fixtures are valid JSON");
        assert!(fixtures.cases.len() >= 4, "daily, BYDAY, INTERVAL=2, UNTIL");

        for case in &fixtures.cases {
            validate_recurrence(&case.rrule)
                .unwrap_or_else(|err| panic!("{} should validate: {err}", case.name));
            let got = occurrence_dates(&case.rrule, &case.from, &case.to)
                .unwrap_or_else(|err| panic!("{} should expand: {err}", case.name));
            assert_eq!(got, case.expected, "case {}", case.name);
        }
    }

    #[test]
    fn occurrence_window_is_inclusive_on_both_ends() {
        let blob = "DTSTART:20260101T063000\nRRULE:FREQ=DAILY";
        // Single-day window [d, d] catches the occurrence on d itself.
        let got = occurrence_dates(blob, "2026-01-03", "2026-01-03").unwrap();
        assert_eq!(got, ["2026-01-03"]);

        // And an inverted window is rejected rather than silently empty.
        let err = occurrence_dates(blob, "2026-01-05", "2026-01-01").unwrap_err();
        assert!(matches!(err, RoutinesError::Invalid(_)), "{err:?}");
    }

    #[test]
    fn occurrence_dates_use_the_blob_dtstart_and_survive_midnight_dtstarts() {
        // Midnight dtstart: the widened bounds must not drop the window edge.
        assert_eq!(
            occurrence_dates(
                "DTSTART:20260101T000000\nRRULE:FREQ=DAILY",
                "2026-01-01",
                "2026-01-02",
            )
            .unwrap(),
            ["2026-01-01", "2026-01-02"]
        );
        // The blob's own DTSTART anchors the series (2026-01-05 is a Monday,
        // so BYDAY=MO covers it; 2026-01-04 does not match).
        assert_eq!(
            occurrence_dates(
                "DTSTART:20260105T063000\nRRULE:FREQ=WEEKLY;BYDAY=MO",
                "2026-01-04",
                "2026-01-12",
            )
            .unwrap(),
            ["2026-01-05", "2026-01-12"]
        );
    }
}
