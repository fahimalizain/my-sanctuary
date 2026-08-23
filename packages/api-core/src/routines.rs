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
//! - Delete is SOFT; materialized occurrences are never touched. A missing/
//!   other-user/soft-deleted routine is always [`RoutinesError::NotFound`] —
//!   ownership is never leaked.
//! - Rule changes (`rrule`) only affect future ensure; already-materialized
//!   occurrences stay.

use std::collections::HashMap;

use chrono::{NaiveDate, NaiveDateTime, TimeZone};
use thiserror::Error;

use crate::categories::{
    classify, ensure_taxonomy, CalendarScope, CategoryWithPatterns, ClassifyOutcome,
};
use crate::models::{
    NewRoutine, NewRoutineInput, Routine, TaskCategory, TaskCategoryPattern, UpdateRoutine,
};
use crate::repo::{RepoError, RoutineRepo, TaskCategoryRepo, TaskListRepo};
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
pub async fn create_routine(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    user_id: &str,
    input: &NewRoutineInput,
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
/// - A missing, soft-deleted, or another user's routine is 404. Rule changes
///   never touch materialized occurrences.
pub async fn update_routine(
    category_repo: &dyn TaskCategoryRepo,
    routine_repo: &dyn RoutineRepo,
    user_id: &str,
    id: &str,
    updates: &UpdateRoutine,
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

    let Some(updated) = routine_repo.update(id, updates).await? else {
        // Deleted between the read and the write.
        return Err(RoutinesError::NotFound);
    };
    Ok(RoutineResponse {
        routine: to_view(&updated, &taxonomy),
    })
}

/// SOFT deletes a routine. Materialized occurrences are not deleted (skip is
/// their decline; they simply belong to a now-deleted series). Missing or
/// another user's routine → 404.
pub async fn delete_routine(
    repo: &dyn RoutineRepo,
    user_id: &str,
    id: &str,
    now_rfc3339: &str,
) -> Result<DeleteRoutineResponse, RoutinesError> {
    let Some(routine) = repo.get_by_id(id).await? else {
        return Err(RoutinesError::NotFound);
    };
    if routine.user_id != user_id {
        return Err(RoutinesError::NotFound);
    }
    repo.soft_delete(id, now_rfc3339).await?;
    Ok(DeleteRoutineResponse { success: true })
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
        matchers.push(CategoryWithPatterns {
            category_id: category.id.clone(),
            parent_id: category.parent_id.clone(),
            patterns,
        });
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
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::models::{
        NewTaskCategory, NewTaskCategoryPattern, NewTaskList, TaskList, UpdateTaskCategory,
        UpdateTaskList,
    };

    const NOW: &str = "2026-08-23T00:00:00Z";

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
                google_color_id: None,
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

    struct Repos {
        lists: FakeTaskListRepo,
        categories: FakeTaskCategoryRepo,
        routines: FakeRoutineRepo,
    }

    fn repos() -> Repos {
        Repos {
            lists: FakeTaskListRepo::new(),
            categories: FakeTaskCategoryRepo::new(),
            routines: FakeRoutineRepo::new(),
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
        ))
        .unwrap();
        let id = created.routine.id.clone();

        let err = pollster::block_on(update_routine(
            &repos.categories,
            &repos.routines,
            "u-1",
            &id,
            &UpdateRoutine::default(),
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
                &rename
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
                &rename
            )),
            Err(RoutinesError::NotFound)
        ));
        // Soft-deleted: get_by_id filters it out.
        pollster::block_on(delete_routine(&repos.routines, "u-1", &id, NOW)).unwrap();
        assert!(matches!(
            pollster::block_on(update_routine(
                &repos.categories,
                &repos.routines,
                "u-1",
                &id,
                &rename
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
        ))
        .unwrap();
        let id = created.routine.id.clone();

        let response = pollster::block_on(delete_routine(&repos.routines, "u-1", &id, NOW)).unwrap();
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
            pollster::block_on(delete_routine(&repos.routines, "u-1", &id, NOW)),
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
        ))
        .unwrap();
        let id = created.routine.id.clone();
        assert!(matches!(
            pollster::block_on(delete_routine(&repos.routines, "u-1", "nope", NOW)),
            Err(RoutinesError::NotFound)
        ));
        assert!(matches!(
            pollster::block_on(delete_routine(&repos.routines, "u-2", &id, NOW)),
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
