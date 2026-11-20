//! The agenda fill (issue #99, slice 4): materialize the horizon **ahead of
//! time**, never on a read.
//!
//! Materializing an agenda day used to be a GET side effect (seed-on-GET). Now
//! the occurrence horizon is filled by the 15-minute cron
//! ([`run_agenda_fill_cron`]) and by routine create/edit
//! ([`crate::routines::create_routine`] / [`crate::routines::update_routine`]),
//! and `GET /api/agenda` stays a pure range read.
//!
//! Domain rules:
//! - The horizon is **today … today+6** (7 inclusive civil days,
//!   [`OCCURRENCE_HORIZON_DAYS`]). Nothing is ever written before today: a
//!   routine whose DTSTART lies in the past is not backfilled into the past —
//!   only the horizon dates its rule actually covers.
//! - A routine **edit** fills **tomorrow … today+6** instead
//!   (`include_today: false`): the edit never adds an occurrence (or an agenda
//!   row) on today — the next `*/15` cron tick fills today if the new rule
//!   covers it.
//! - Today is always the user's `user_today` (primary calendar `time_zone`,
//!   UTC when there is none). The fill never computes a UTC today of its own.
//! - Everything here is idempotent: a second pass with unchanged data writes
//!   nothing (`INSERT OR IGNORE` under `UNIQUE (routine_id, local_date)` /
//!   `UNIQUE (user_id, local_date, kind, ref_id)`, plus a "membership anywhere"
//!   check that never re-attaches a rescheduled row to its rule date).
//! - A stored `rrule` blob that no longer parses is skipped, never fatal: one
//!   rotten blob must not 400 (or fail) the whole user.
//!
//! The companion reconcile lives in [`crate::routines`]: a rule change rewrites
//! only **untouched** rows from tomorrow through today+6, and a routine delete
//! hard-deletes those same rows.
//!
//! The task half ([`ensure_task_items`]) only **supplies a default**: a
//! rowless `OPEN`/`PLANNED` task gets the backlog sentinel, a rowless
//! `IN_PROGRESS`/`WAITING` task gets today, and an undated (at/after the
//! 10-year horizon) `IN_PROGRESS`/`WAITING` row is dated today. It never moves
//! a dated row — in particular `move-day-to-today` may explicitly put an
//! `OPEN` row on a real day, and this cron must not undo it.

use std::collections::{HashMap, HashSet};

use crate::agenda::{
    add_civil_days, backlog_local_date, planning_horizon_date, user_today, AgendaError,
    AGENDA_KIND_OCCURRENCE, AGENDA_KIND_TASK,
};
use crate::models::{
    AgendaItem, NewAgendaItem, NewRoutineOccurrence, Routine, RoutineOccurrence, Task,
};
use crate::repo::{AgendaItemRepo, CalendarRepo, OccurrenceRepo, RoutineRepo, TaskRepo};
use crate::routines::occurrence_dates;
use crate::tasks::{
    TASK_STATUS_IN_PROGRESS, TASK_STATUS_OPEN, TASK_STATUS_PLANNED, TASK_STATUS_WAITING,
};

/// How many inclusive civil days the fill materializes: today … today+6.
pub const OCCURRENCE_HORIZON_DAYS: i64 = 7;

// ──────────────────────────────────────────
// Occurrence horizon
// ──────────────────────────────────────────

/// Materializes every living routine's occurrences for **today … today+6** and
/// appends the missing membership rows — one batched pass per user.
///
/// `include_today` picks the window start: `true` (the cron and routine
/// **create**) fills `today … today+6`; `false` (a routine **edit**) starts at
/// **tomorrow**, so a rule change never gives today a new occurrence or agenda
/// row — the next `*/15` cron tick fills today if the new rule covers it. A
/// window that starts after the horizon end (impossible while
/// [`OCCURRENCE_HORIZON_DAYS`] ≥ 2, stated anyway) writes nothing.
///
/// Steps:
/// 1. Resolve civil today (primary calendar zone, else UTC) and the horizon
///    end (`today + 6`).
/// 2. Routines: `only_routine` loads that single living routine (missing or
///    soft-deleted → `Ok(())`: create/update only ever call this with a living
///    id); `None` loads all of the user's living routines in standing order.
/// 3. Expand each rule over the window ([`occurrence_dates`], inclusive).
///    A blob that no longer parses skips **that routine** — a fill never fails
///    the user on one bad row. Dates before the window start are dropped
///    outright.
/// 4. ONE `list_by_user_and_date_range` read of the window; only the missing
///    `(routine_id, local_date)` pairs are inserted (`insert_many` is
///    `INSERT OR IGNORE`), then the window is re-listed so a UUID lost to the
///    unique index is replaced by the surviving id.
/// 5. Agenda rows: `list_by_refs(user, "occurrence", ids)` on **any** date —
///    a row is appended only when that occurrence has no membership anywhere,
///    so a rescheduled occurrence is never re-attached to its rule date. New
///    rows land after that date's current `max_sort_order` (else 0), in
///    `routines.sort_order` order, and existing ranks are never reshuffled.
pub async fn fill_occurrence_horizon(
    routine_repo: &dyn RoutineRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    agenda_repo: &dyn AgendaItemRepo,
    calendars: &dyn CalendarRepo,
    user_id: &str,
    now_unix: i64,
    only_routine: Option<&str>,
    include_today: bool,
) -> Result<(), AgendaError> {
    let (today, _time_zone) = user_today(calendars, user_id, now_unix).await?;
    let horizon_end = add_civil_days(&today, OCCURRENCE_HORIZON_DAYS - 1)?;
    let window_start = if include_today {
        today
    } else {
        add_civil_days(&today, 1)?
    };
    if window_start.as_str() > horizon_end.as_str() {
        return Ok(());
    }

    let routines: Vec<Routine> = match only_routine {
        Some(id) => match routine_repo.get_by_id(id).await? {
            Some(routine) if routine.user_id == user_id => vec![routine],
            // Missing, soft-deleted or another user's: nothing to fill (the
            // callers only pass living ids, so this is a no-op, not an error).
            _ => return Ok(()),
        },
        None => routine_repo.list_by_user_id(user_id).await?,
    };

    // (routine_id, local_date) pairs the rules cover in the window, in
    // standing routine order.
    let mut wanted: Vec<(String, String)> = Vec::new();
    for routine in &routines {
        let Ok(dates) = occurrence_dates(&routine.rrule, &window_start, &horizon_end) else {
            // A rotten blob never fails the whole user: skip this routine.
            continue;
        };
        for date in dates {
            // The window already starts at `window_start`; the guard states
            // the rule.
            if date.as_str() < window_start.as_str() {
                continue;
            }
            wanted.push((routine.id.clone(), date));
        }
    }

    let mut occurrences = occurrence_repo
        .list_by_user_and_date_range(user_id, &window_start, &horizon_end)
        .await?;
    let stored: HashSet<(String, String)> = occurrences
        .iter()
        .map(|occ| (occ.routine_id.clone(), occ.local_date.clone()))
        .collect();
    let missing: Vec<NewRoutineOccurrence> = wanted
        .iter()
        .filter(|(routine_id, date)| !stored.contains(&(routine_id.clone(), date.clone())))
        .map(|(routine_id, date)| NewRoutineOccurrence {
            routine_id: routine_id.clone(),
            user_id: user_id.to_string(),
            local_date: date.clone(),
        })
        .collect();
    if !missing.is_empty() {
        // `INSERT OR IGNORE`: only the absent pairs are written (the filter
        // above is a best effort — a concurrent fill may have won the race).
        occurrence_repo.insert_many(missing).await?;
        // Re-list: a row that lost the race kept the winner's UUID, which the
        // in-memory batch never saw.
        occurrences = occurrence_repo
            .list_by_user_and_date_range(user_id, &window_start, &horizon_end)
            .await?;
    }

    // Filled routines only: a soft-deleted routine's leftover rows are not
    // this pass's business. The index carries `routines.sort_order` order into
    // the append loop below.
    let routine_order: HashMap<&str, usize> = routines
        .iter()
        .enumerate()
        .map(|(index, routine)| (routine.id.as_str(), index))
        .collect();
    let mut candidates: Vec<&RoutineOccurrence> = occurrences
        .iter()
        .filter(|occ| {
            occ.user_id == user_id && routine_order.contains_key(occ.routine_id.as_str())
        })
        .collect();
    if candidates.is_empty() {
        return Ok(());
    }
    let ref_ids: Vec<String> = candidates.iter().map(|occ| occ.id.clone()).collect();
    let memberships = agenda_repo
        .list_by_refs(user_id, AGENDA_KIND_OCCURRENCE, &ref_ids)
        .await?;
    let with_row: HashSet<&str> = memberships.iter().map(|item| item.ref_id.as_str()).collect();
    candidates.retain(|occ| !with_row.contains(occ.id.as_str()));
    if candidates.is_empty() {
        return Ok(());
    }

    // One append per date, after that date's current max rank, in standing
    // routine order. The map only groups; the per-date vectors are sorted by
    // the routine index so the append order is deterministic.
    let mut by_date: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    for occ in &candidates {
        by_date
            .entry(occ.local_date.clone())
            .or_default()
            .push((routine_order[occ.routine_id.as_str()], occ.id.clone()));
    }
    let mut rows: Vec<NewAgendaItem> = Vec::with_capacity(candidates.len());
    for (date, mut refs) in by_date {
        refs.sort();
        let mut rank = agenda_repo
            .max_sort_order(user_id, &date)
            .await?
            .map(|max| max + 1)
            .unwrap_or(0);
        for (_, ref_id) in refs {
            rows.push(NewAgendaItem {
                user_id: user_id.to_string(),
                local_date: date.clone(),
                kind: AGENDA_KIND_OCCURRENCE.to_string(),
                ref_id,
                sort_order: rank,
            });
            rank += 1;
        }
    }
    agenda_repo.insert_many(rows).await?;
    Ok(())
}

// ──────────────────────────────────────────
// Task placement (self-heal, ADR 0007)
// ──────────────────────────────────────────

/// Puts each living task's single agenda row on its **status default** only
/// when the task has no row or holds an **undated** row — the `*/15` self-heal
/// after status and date were decoupled. A **dated** row is scheduling
/// intent and is never moved by this fill, whatever the task's status.
///
/// - Living tasks only (`list_by_user_id` hides soft-deleted rows) and only
///   `OPEN | PLANNED | IN_PROGRESS | WAITING`: `COMPLETED`/`DISCARDED` get
///   nothing.
/// - `list_by_refs(user, "task", ids)` on **any** date maps each task to the
///   row it already holds, if any (never a second row — one row per task is
///   the 0018 invariant). The backlog date is civil today + 50 years
///   ([`backlog_local_date`]); the 10-year line is civil today + 10 years
///   ([`planning_horizon_date`]). A row below that line is **dated**; at/after
///   it the row is **undated** (warehoused).
/// - `OPEN` with no row → one is inserted on the backlog date. **Any**
///   existing `OPEN` row is left alone, dated or undated: `move-day-to-today`
///   may deliberately put an `OPEN` row on a real day, and the cron must not
///   undo that. An undated `OPEN` row (yesterday's sentinel included) is still
///   valid warehouse work and is not normalized either.
/// - `PLANNED` with no row → one is inserted on the backlog date (ADR 0010).
///   Any existing `PLANNED` row is left alone — past, dated future, and
///   at/after the horizon are all valid.
/// - `IN_PROGRESS` with a row dated `>=` the horizon → the row moves to
///   today; with no row → one is inserted on today. A dated row in
///   `[past, horizon)` is never touched (a past `IN_PROGRESS` row can still
///   pin the anchor).
/// - `WAITING` behaves exactly like `IN_PROGRESS` (ADR 0011): a rowless task
///   or an undated row lands on today — this only completes a failed
///   placement; a row on a real date is never touched, and no calendar event
///   is created.
/// - The arrivals are appended in **board order** per destination: the today
///   pile holds `IN_PROGRESS` first and then `WAITING`, the backlog pile
///   `PLANNED` first and then `OPEN`, each by `tasks.sort_order` with ties
///   broken by task id ascending. Ranks start at
///   `max_sort_order(destination) + 1` (0 when that pile is empty) and
///   increase by one in that order. Rows already correctly on the destination
///   keep their ranks — moving and inserting share one counter, so a healed
///   row and a rowless task land as one board.
///
/// Idempotent: a second pass with nothing missing or undated writes nothing
/// (no `insert_many`, no `set_local_date`).
pub async fn ensure_task_items(
    agenda_repo: &dyn AgendaItemRepo,
    task_repo: &dyn TaskRepo,
    calendars: &dyn CalendarRepo,
    user_id: &str,
    now_unix: i64,
) -> Result<(), AgendaError> {
    let (today, _time_zone) = user_today(calendars, user_id, now_unix).await?;
    let backlog_date = backlog_local_date(&today)?;
    let horizon = planning_horizon_date(&today)?;

    let tasks: Vec<Task> = task_repo
        .list_by_user_id(user_id)
        .await?
        .into_iter()
        .filter(|task| status_rank(&task.status).is_some())
        .collect();
    if tasks.is_empty() {
        return Ok(());
    }

    let ids: Vec<String> = tasks.iter().map(|task| task.id.clone()).collect();
    let memberships = agenda_repo
        .list_by_refs(user_id, AGENDA_KIND_TASK, &ids)
        .await?;
    let rows_by_task: HashMap<&str, &AgendaItem> = memberships
        .iter()
        .map(|item| (item.ref_id.as_str(), item))
        .collect();

    // Destination piles: `Some(row)` is a row that has to travel,
    // `None` a rowless task. Everything absent here is already correct.
    let mut today_pile: Vec<(&Task, Option<&AgendaItem>)> = Vec::new();
    let mut backlog_pile: Vec<(&Task, Option<&AgendaItem>)> = Vec::new();
    for task in &tasks {
        let row = rows_by_task.get(task.id.as_str()).copied();
        match (task.status.as_str(), row) {
            // OPEN is a warehouse status by default, but the row is scheduling
            // intent and the fill never moves a dated one: `move-day-to-today`
            // may deliberately put an OPEN row on a real day, and the cron must
            // not undo that. Only a rowless OPEN task gets the sentinel.
            (TASK_STATUS_OPEN, None) => backlog_pile.push((task, None)),
            (TASK_STATUS_OPEN, Some(_)) => {}
            // PLANNED is a warehouse status since ADR 0010: a rowless task is
            // parked on the sentinel, and ANY existing row (past, dated
            // future, or at/after the horizon) is valid and left alone.
            (TASK_STATUS_PLANNED, None) => backlog_pile.push((task, None)),
            (TASK_STATUS_PLANNED, Some(_)) => {}
            // IN_PROGRESS and WAITING are real-day statuses: a rowless task
            // lands on today, and a row parked at/after the horizon is healed
            // there. A WAITING row on a real date is never moved (ADR 0011:
            // only a real transition moves it; this heal just completes a
            // failed hand-off placement).
            (TASK_STATUS_IN_PROGRESS | TASK_STATUS_WAITING, None) => {
                today_pile.push((task, None))
            }
            (TASK_STATUS_IN_PROGRESS | TASK_STATUS_WAITING, Some(row))
                if row.local_date.as_str() >= horizon.as_str() =>
            {
                today_pile.push((task, Some(row)));
            }
            // Already correct: an IN_PROGRESS/WAITING row still inside the
            // horizon (terminal statuses never reach here — `status_rank`
            // dropped them above).
            _ => {}
        }
    }

    append_task_pile(agenda_repo, user_id, &today, today_pile).await?;
    append_task_pile(agenda_repo, user_id, &backlog_date, backlog_pile).await?;
    Ok(())
}

/// Appends one destination pile after that date's current max rank, in board
/// order (the today pile holds `IN_PROGRESS` then `WAITING`; the backlog pile
/// holds `PLANNED` then `OPEN`, ADR 0010).
///
/// A task that already holds a row travels with
/// [`AgendaItemRepo::set_local_date`]; the rowless ones are collected and
/// written with one `insert_many` at the end, using the ranks already assigned
/// by the shared `max + 1` counter. An empty pile writes nothing.
async fn append_task_pile(
    agenda_repo: &dyn AgendaItemRepo,
    user_id: &str,
    local_date: &str,
    mut pile: Vec<(&Task, Option<&AgendaItem>)>,
) -> Result<(), AgendaError> {
    if pile.is_empty() {
        return Ok(());
    }
    pile.sort_by(|(a, _), (b, _)| {
        (status_rank(&a.status), a.sort_order, a.id.as_str()).cmp(&(
            status_rank(&b.status),
            b.sort_order,
            b.id.as_str(),
        ))
    });

    let mut rank = agenda_repo
        .max_sort_order(user_id, local_date)
        .await?
        .map(|max| max + 1)
        .unwrap_or(0);
    let mut inserts: Vec<NewAgendaItem> = Vec::new();
    for (task, row) in pile {
        match row {
            Some(row) => {
                agenda_repo
                    .set_local_date(&row.id, local_date, rank)
                    .await?
            }
            None => inserts.push(NewAgendaItem {
                user_id: user_id.to_string(),
                local_date: local_date.to_string(),
                kind: AGENDA_KIND_TASK.to_string(),
                ref_id: task.id.clone(),
                sort_order: rank,
            }),
        }
        rank += 1;
    }
    if !inserts.is_empty() {
        agenda_repo.insert_many(inserts).await?;
    }
    Ok(())
}

/// Board order rank of a still-actionable task status: `IN_PROGRESS`, then
/// `WAITING`, then `PLANNED`, then `OPEN`. `None` for terminal statuses — they
/// are never placed.
fn status_rank(status: &str) -> Option<u8> {
    match status {
        TASK_STATUS_IN_PROGRESS => Some(0),
        TASK_STATUS_WAITING => Some(1),
        TASK_STATUS_PLANNED => Some(2),
        TASK_STATUS_OPEN => Some(3),
        _ => None,
    }
}

// ──────────────────────────────────────────
// The 15-minute cron
// ──────────────────────────────────────────

/// Outcome of one agenda fill cron run: how many users were attempted plus
/// human-readable failures — one user's failure never fails the whole job.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgendaFillReport {
    /// Users attempted (the union of routine owners and open-task owners).
    pub users: usize,
    /// Human-readable failures; empty when everything worked.
    pub errors: Vec<String>,
}

/// The agenda fill cron (the `*/15 * * * *` tick): for each affected user,
/// materialize the occurrence horizon and supply each living task's single
/// agenda row only when it is missing or undated — [`ensure_task_items`].
/// A dated row is never moved; a rowless `OPEN`/`PLANNED` task lands on the
/// backlog date, and a rowless or undated `IN_PROGRESS`/`WAITING` task lands
/// on today (status/date decoupling; ADR 0007 / 0009 / 0010 / 0011).
///
/// The work list is the sorted, deduped union of
/// [`RoutineRepo::list_user_ids_with_living_routines`] and
/// [`TaskRepo::list_user_ids_with_open_tasks`] — no user table scan, and a
/// failure of either lister is recorded and the other one still runs. Per user
/// the fill runs first, then the task backfill: a failing fill still gets its
/// task backfill (its error is recorded, the loop continues).
///
/// Never returns `Err`: this runs in a cron tick, where a broken isolate is
/// worse than a logged error.
pub async fn run_agenda_fill_cron(
    routine_repo: &dyn RoutineRepo,
    occurrence_repo: &dyn OccurrenceRepo,
    agenda_repo: &dyn AgendaItemRepo,
    task_repo: &dyn TaskRepo,
    calendars: &dyn CalendarRepo,
    now_unix: i64,
) -> AgendaFillReport {
    let mut report = AgendaFillReport::default();
    let mut user_ids: Vec<String> = Vec::new();
    match routine_repo.list_user_ids_with_living_routines().await {
        Ok(ids) => user_ids.extend(ids),
        Err(err) => report
            .errors
            .push(format!("list_user_ids_with_living_routines failed: {err}")),
    }
    match task_repo.list_user_ids_with_open_tasks().await {
        Ok(ids) => user_ids.extend(ids),
        Err(err) => report
            .errors
            .push(format!("list_user_ids_with_open_tasks failed: {err}")),
    }
    // Sorted + deduped in Rust: a user with both routines and open tasks must
    // be filled exactly once.
    user_ids.sort();
    user_ids.dedup();

    for user_id in &user_ids {
        report.users += 1;
        if let Err(err) = fill_occurrence_horizon(
            routine_repo,
            occurrence_repo,
            agenda_repo,
            calendars,
            user_id,
            now_unix,
            None,
            // The cron owns today: a rule edit deliberately starts its refill
            // tomorrow, and this tick is what fills today when the new rule
            // covers it.
            true,
        )
        .await
        {
            report
                .errors
                .push(format!("agenda fill failed for user {user_id}: {err}"));
        }
        if let Err(err) =
            ensure_task_items(agenda_repo, task_repo, calendars, user_id, now_unix).await
        {
            report
                .errors
                .push(format!("agenda task backfill failed for user {user_id}: {err}"));
        }
    }
    report
}
