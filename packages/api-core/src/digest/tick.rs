//! The recompute tick: backfill, reclassify, and dirty-day rebuilds.
//!
//! `run_digest_tick` is the single entry point for cron and the queue
//! consumer. It never throws — every failure is returned in
//! [`DigestTickReport::errors`], same style as `run_fallback_cron`.
//!
//! One tick handles **one user** (the first from `users_with_work`) with a
//! budget of [`DIGEST_TICK_BUDGET_DAYS`] days: the work is bounded so a tick
//! always finishes well inside the Worker's CPU/wall limits, and the remaining
//! work is picked up by the next tick or by a re-enqueued queue message.

use std::collections::BTreeSet;

use crate::calendar_color::load_classify_taxonomy;
use crate::categories::classify_event_leaf;
use crate::repo::{RepoError, TaskCategoryRepo};
use crate::time::{civil_date_in_zone, unix_secs_to_rfc3339};

use super::repo::{dirty_dates_for_span, DigestRepo, JobStatus};
use super::sleep::{resolve_sleep_category_id, ALGO_VERSION, DEFAULT_SLEEP_SECS};
use super::{digest_days, shift_date};

/// Days rebuilt per user per tick.
pub const DIGEST_TICK_BUDGET_DAYS: usize = 7;
/// Events reclassified per tick (`reclassify_batch`).
pub const RECLASSIFY_BATCH_LIMIT: usize = 50;
/// Extra days loaded on each side of a rebuild window: an event that starts
/// before the window can still own a night inside it, and a lexicographic
/// `substr` compare needs slack for zone-shifted instants anyway.
pub const DIGEST_EVENT_WINDOW_PAD_DAYS: i64 = 2;
/// Requested days one `load_events_touching` window may cover.
///
/// A run of dirty dates is split into chunks of at most this many **requested**
/// days, so the loaded window is at most
/// `45 + 2 * DIGEST_EVENT_WINDOW_PAD_DAYS = 49` civil dates wide (51 for the
/// digest set, which is expanded by one day on each side). A sparse dirty set
/// therefore never loads the years between its dates, and a long contiguous
/// backlog never becomes one unbounded scan.
pub const DIGEST_REBUILD_RUN_MAX_DAYS: usize = 45;
/// The backfill starts one day before the earliest event so the first night's
/// pre-midnight half is rebuilt too.
pub const DIGEST_BACKFILL_START_PAD_DAYS: i64 = 1;

/// `report_job_status.kind` values.
pub const JOB_BACKFILL: &str = "backfill";
/// `report_job_status.kind` values.
pub const JOB_RECLASSIFY: &str = "reclassify";
/// `report_job_status.status` values.
pub const JOB_IDLE: &str = "idle";
/// `report_job_status.status` values.
pub const JOB_RUNNING: &str = "running";
/// `report_job_status.status` values.
pub const JOB_DONE: &str = "done";

/// What one tick did. `errors` is never empty on a partial failure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DigestTickReport {
    /// The user this tick worked on (the queue path always names one).
    pub user_id: Option<String>,
    /// Days whose `daily_meta` / category / hour / night rows were replaced.
    pub rebuilt_days: usize,
    /// Events restamped by the reclassify job.
    pub reclassified: usize,
    /// The backfill job was (re)started by this tick.
    pub backfill_started: bool,
    /// The backfill job finished in this tick.
    pub backfill_done: bool,
    /// The reclassify job finished in this tick.
    pub reclassify_done: bool,
    /// Days that stayed dirty because a reclassify was in flight when the tick
    /// finished rebuilding (F10). Not an error: the next tick redoes them.
    pub kept_dirty: usize,
    /// Dirty days or a `running` job remain — the caller should re-enqueue.
    pub remaining: bool,
    pub errors: Vec<String>,
}

/// Reads (and, when needed, starts) the backfill job for one user.
///
/// Starts the job when the taxonomy of days is out of date — either an
/// `algo_version` mismatch, or the user has a living event, no `daily_meta`
/// row, and the job is neither `running` nor `done`. Never blocks on the
/// rebuild itself: the tick does the work.
///
/// A **first-run** backfill (no meta yet) also queues the reclassify job: the
/// cached events carry `NULL` stamps until a taxonomy edit enqueues one, so a
/// rollup built from them would store the whole history as unclassified and
/// nothing would ever restamp it. The `algo_version` path deliberately leaves
/// the stamps alone — they are still valid, only the rollup changed.
///
/// Returns the observed status (`None` = no row) and whether *this* call
/// started it.
pub async fn ensure_backfill_job(
    repo: &dyn DigestRepo,
    user_id: &str,
    now_rfc3339: &str,
) -> Result<BackfillJob, RepoError> {
    let status = repo.job_status(user_id, JOB_BACKFILL).await?;
    if status.as_ref().is_some_and(JobStatus::is_running) {
        return Ok(BackfillJob {
            status,
            started: false,
        });
    }
    let done = status.as_ref().is_some_and(|row| row.status == JOB_DONE);

    let stale = repo.has_stale_meta(user_id, ALGO_VERSION).await?;
    // The two reasons to start are kept apart: only the first one may restamp.
    let first_run = !stale
        && !done
        && !repo.has_meta(user_id).await?
        && repo.earliest_event_date(user_id).await?.is_some();
    if !stale && !first_run {
        return Ok(BackfillJob {
            status,
            started: false,
        });
    }
    repo.upsert_job_status(user_id, JOB_BACKFILL, JOB_RUNNING, "", now_rfc3339)
        .await?;
    if first_run {
        ensure_reclassify_job(repo, user_id, now_rfc3339).await?;
    }
    Ok(BackfillJob {
        status: Some(JobStatus {
            status: JOB_RUNNING.to_string(),
            cursor: String::new(),
        }),
        started: true,
    })
}

/// The observed backfill job, plus whether the caller just started it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillJob {
    pub status: Option<JobStatus>,
    pub started: bool,
}

impl BackfillJob {
    /// `status == "running"`.
    pub fn is_running(&self) -> bool {
        self.status.as_ref().is_some_and(JobStatus::is_running)
    }

    /// The cursor to resume from (`""` = not started).
    pub fn cursor(&self) -> &str {
        self.status.as_ref().map(|row| row.cursor.as_str()).unwrap_or("")
    }
}

/// Queues the reclassify job from the first event unless it is mid-scan.
///
/// Only the job row is written here: the taxonomy load and the stamps belong to
/// the tick's `run_reclassify`, so a read endpoint (`GET /api/reports/status`,
/// opening the record) can queue work but can never classify anything.
///
/// A `done` job is restarted only when the user still has a living event that
/// was never classified (`has_unstamped_event`): the finished scan's stamps are
/// valid, so re-running it would rewrite nothing (only `has_unstamped_event`
/// can have changed since — an event synced in after the scan). A `done` job
/// with nothing pending is left alone.
async fn ensure_reclassify_job(
    repo: &dyn DigestRepo,
    user_id: &str,
    now_rfc3339: &str,
) -> Result<(), RepoError> {
    let status = repo.job_status(user_id, JOB_RECLASSIFY).await?;
    if status.as_ref().is_some_and(JobStatus::is_running) {
        // Mid-scan: resetting the cursor would restart the whole history.
        return Ok(());
    }
    let done = status.as_ref().is_some_and(|row| row.status == JOB_DONE);
    if done && !repo.has_unstamped_event(user_id).await? {
        // A finished scan with nothing left to stamp: restarting it would only
        // walk the whole history again for no write.
        return Ok(());
    }
    repo.upsert_job_status(user_id, JOB_RECLASSIFY, JOB_RUNNING, "", now_rfc3339)
        .await
}

/// The user's digest status, starting the backfill job when it is due.
///
/// This is what `GET /api/reports/status` calls: it never awaits the rebuild,
/// it only reports (and, at most, *marks*) work — a first-run backfill also
/// queues the reclassify it depends on (see [`ensure_backfill_job`]).
pub async fn digest_status(
    repo: &dyn DigestRepo,
    user_id: &str,
    now_unix: i64,
) -> Result<DigestStatus, RepoError> {
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    let backfill = ensure_backfill_job(repo, user_id, &now_rfc3339).await?.status;
    let reclassify = repo.job_status(user_id, JOB_RECLASSIFY).await?;
    Ok(DigestStatus {
        backfilling: backfill.as_ref().is_some_and(JobStatus::is_running),
        reclassifying: reclassify.as_ref().is_some_and(JobStatus::is_running),
        dirty_days: repo.count_dirty(user_id).await?,
    })
}

/// `GET /api/reports/status` body.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DigestStatus {
    pub reclassifying: bool,
    pub backfilling: bool,
    pub dirty_days: i64,
}

/// Runs one digest tick. `user_id = None` picks the first user with work.
pub async fn run_digest_tick(
    repo: &dyn DigestRepo,
    category_repo: &dyn TaskCategoryRepo,
    user_id: Option<&str>,
    budget_days: usize,
    now_unix: i64,
) -> DigestTickReport {
    if let Some(user_id) = user_id {
        return run_digest_tick_user(repo, category_repo, user_id, budget_days, now_unix).await;
    }
    let users = match repo.users_with_work(1).await {
        Ok(users) => users,
        Err(err) => {
            return DigestTickReport {
                errors: vec![format!("digest: users_with_work failed: {err}")],
                ..DigestTickReport::default()
            }
        }
    };
    let Some(user_id) = users.into_iter().next() else {
        return DigestTickReport::default();
    };
    run_digest_tick_user(repo, category_repo, &user_id, budget_days, now_unix).await
}

/// Runs the tick for one named user.
///
/// Order of work, highest priority first:
/// 0. **today is marked dirty** (when the user has any event or any `daily_meta`
///    row), so a quiet user's tail is recomputed instead of freezing at the
///    last write — today is never cleared, so it is always the newest pending
///    day;
/// 1. a `running` reclassify spends one batch **first**, and while it is
///    unfinished it owns the whole tick: the stamps it writes are what a rollup
///    reads, so no day may be rebuilt before the scan is done;
/// 2. once it drains, a `running` backfill (or a stale `algo_version`) gets the
///    remaining budget;
/// 3. otherwise: rebuild up to `budget_days` dirty days — including the ones
///    the reclassify just marked.
pub async fn run_digest_tick_user(
    repo: &dyn DigestRepo,
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
    budget_days: usize,
    now_unix: i64,
) -> DigestTickReport {
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    let mut report = DigestTickReport {
        user_id: Some(user_id.to_string()),
        ..DigestTickReport::default()
    };
    let today = user_today(repo, user_id, now_unix).await;
    if let Err(err) = mark_today_dirty(repo, user_id, &today, &now_rfc3339).await {
        report
            .errors
            .push(format!("digest: mark today dirty failed: {err}"));
    }

    let backfill = match ensure_backfill_job(repo, user_id, &now_rfc3339).await {
        Ok(job) => job,
        Err(err) => {
            report.errors.push(format!("digest: backfill status failed: {err}"));
            BackfillJob {
                status: None,
                started: false,
            }
        }
    };
    report.backfill_started = backfill.started;
    let reclassify_status = match repo.job_status(user_id, JOB_RECLASSIFY).await {
        Ok(status) => status,
        Err(err) => {
            report
                .errors
                .push(format!("digest: reclassify status failed: {err}"));
            None
        }
    };
    let reclassify_running = reclassify_status.as_ref().is_some_and(JobStatus::is_running);

    // A restamp lands before any rollup: a day rebuilt while its events still
    // carry stale/NULL stamps would store every second as unclassified.
    let drained = if reclassify_running {
        run_reclassify(
            repo,
            category_repo,
            user_id,
            reclassify_status
                .as_ref()
                .map(|row| row.cursor.as_str())
                .unwrap_or(""),
            &now_rfc3339,
            &mut report,
        )
        .await
    } else {
        true
    };

    if drained {
        if backfill.is_running() {
            let cursor = backfill.cursor().to_string();
            run_backfill(
                repo,
                category_repo,
                user_id,
                &cursor,
                budget_days,
                &today,
                now_unix,
                &now_rfc3339,
                &mut report,
            )
            .await;
        } else {
            run_rebuild(
                repo,
                category_repo,
                user_id,
                budget_days,
                &today,
                now_unix,
                &mut report,
            )
            .await;
        }
    }

    // Decide whether the caller should come back soon. Recomputed from the
    // DB: a partially successful branch may have left work behind.
    //
    // Only a day that is **already finished** (< today) is pending work:
    // today's row is deliberately never cleared (its tail is still being
    // written) and a future date has nothing to compute yet, so counting
    // either as "work remains" would make every tick re-enqueue itself
    // forever. The oldest dirty date answers both — dirty dates sort
    // ascending, so if it is not before today, none is.
    let dirty_left = repo
        .list_dirty(user_id, 1)
        .await
        .map(|dates| {
            dates
                .first()
                .is_some_and(|date| date.as_str() < today.as_str())
        })
        .unwrap_or(false);
    let backfill_still_running = repo
        .job_status(user_id, JOB_BACKFILL)
        .await
        .ok()
        .flatten()
        .is_some_and(|row| row.is_running());
    let reclassify_still_running = repo
        .job_status(user_id, JOB_RECLASSIFY)
        .await
        .ok()
        .flatten()
        .is_some_and(|row| row.is_running());
    report.remaining = dirty_left || backfill_still_running || reclassify_still_running;
    report
}

/// The user's **today**: the civil date of `now_unix` in the primary calendar
/// zone (empty/unknown ⇒ UTC). One definition for the backfill floor, the
/// rebuild guard, and the "today stays dirty" rule.
async fn user_today(repo: &dyn DigestRepo, user_id: &str, now_unix: i64) -> String {
    let primary_tz = repo
        .primary_time_zone(user_id)
        .await
        .unwrap_or(None)
        .unwrap_or_default();
    civil_date_in_zone(now_unix, &primary_tz)
}

/// Marks `today` dirty for a user the tick is about to work on.
///
/// Today's rows are always clipped at `now` and are **never** cleared (see
/// [`rebuild_dates`]), so the tail has to be recomputed by a later tick. A
/// quiet user — no new event, no reclassify — would otherwise keep whatever
/// partial total the last write produced.
///
/// A user with no event and no `daily_meta` row has nothing to keep open, so
/// the mark is skipped: a user the tick would never pick gets no dirty row.
async fn mark_today_dirty(
    repo: &dyn DigestRepo,
    user_id: &str,
    today: &str,
    now_rfc3339: &str,
) -> Result<(), RepoError> {
    let has_events = repo.earliest_event_date(user_id).await?.is_some();
    if !has_events && !repo.has_meta(user_id).await? {
        return Ok(());
    }
    repo.mark_dirty(user_id, &[today.to_string()], now_rfc3339)
        .await
}

#[allow(clippy::too_many_arguments)]
async fn run_backfill(
    repo: &dyn DigestRepo,
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
    cursor: &str,
    budget_days: usize,
    today: &str,
    now_unix: i64,
    now_rfc3339: &str,
    report: &mut DigestTickReport,
) {
    let start = if cursor.is_empty() {
        match repo.earliest_event_date(user_id).await {
            Ok(Some(earliest)) => shift_date(&earliest, -DIGEST_BACKFILL_START_PAD_DAYS)
                .unwrap_or(earliest),
            Ok(None) => {
                // No events at all: nothing to backfill, and nothing to look
                // at later either.
                finish_job(repo, user_id, JOB_BACKFILL, now_rfc3339, report).await;
                report.backfill_done = true;
                return;
            }
            Err(err) => {
                report.errors.push(format!("digest: backfill floor failed: {err}"));
                return;
            }
        }
    } else {
        cursor.to_string()
    };

    if start.as_str() > today || budget_days == 0 {
        finish_job(repo, user_id, JOB_BACKFILL, now_rfc3339, report).await;
        report.backfill_done = true;
        return;
    }

    // Consecutive days from the cursor, never past today: a future day has no
    // finished time to aggregate, and its night must not be guessed early.
    let mut dates: Vec<String> = Vec::new();
    let mut next_date = start.clone();
    while dates.len() < budget_days && next_date.as_str() <= today {
        dates.push(next_date.clone());
        match shift_date(&next_date, 1) {
            Some(shifted) => next_date = shifted,
            None => break,
        }
    }
    if dates.is_empty() {
        finish_job(repo, user_id, JOB_BACKFILL, now_rfc3339, report).await;
        report.backfill_done = true;
        return;
    }
    let rebuilt = rebuild_dates(
        repo,
        category_repo,
        user_id,
        &dates,
        today,
        now_unix,
        &mut report.errors,
    )
    .await;
    match rebuilt {
        Ok(outcome) => {
            report.rebuilt_days += outcome.rebuilt;
            // Today's row was clipped at `now`: it stays dirty so the next
            // tick recomputes the tail.
            clear_rebuilt_days(repo, user_id, &outcome.clearable, report).await;
            let next = dates
                .last()
                .and_then(|last| shift_date(last, 1))
                .unwrap_or_else(|| today.to_string());
            if next.as_str() > today {
                finish_job(repo, user_id, JOB_BACKFILL, now_rfc3339, report).await;
                report.backfill_done = true;
            } else if let Err(err) =
                repo.upsert_job_status(user_id, JOB_BACKFILL, JOB_RUNNING, &next, now_rfc3339)
                    .await
            {
                report
                    .errors
                    .push(format!("digest: backfill advance failed: {err}"));
            }
        }
        Err(err) => report
            .errors
            .push(format!("digest: backfill rebuild failed: {err}")),
    }
}

/// Returns `true` when there is nothing left to scan.
///
/// The id walk is `e.id > ? ORDER BY e.id`, and ids are random UUIDs: a row
/// inserted while the scan is walking can sort **before** the cursor and would
/// never be visited again, so a short batch may not simply finish the job. The
/// scan is exhausted when a batch comes back short **and** the verification
/// query ([`DigestRepo::unstamped_batch`]) finds no living event that was never
/// classified. Verification rows are stamped in this same tick; a full
/// verification batch keeps the job `running` and rewinds the cursor to the
/// start so the next tick's walk cannot skip anything.
async fn run_reclassify(
    repo: &dyn DigestRepo,
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
    cursor: &str,
    now_rfc3339: &str,
    report: &mut DigestTickReport,
) -> bool {
    // No seeding here: a reclassify must never create taxonomy as a side
    // effect. A load failure leaves the job `running` with the cursor
    // unmoved, so cron retries and no stamp is blanked.
    let taxonomy = match load_classify_taxonomy(category_repo, user_id).await {
        Ok(taxonomy) => taxonomy,
        Err(err) => {
            report
                .errors
                .push(format!("digest: taxonomy load failed: {err}"));
            return false;
        }
    };
    let rows = match repo
        .reclassify_batch(user_id, cursor, RECLASSIFY_BATCH_LIMIT)
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            report
                .errors
                .push(format!("digest: reclassify batch failed: {err}"));
            return false;
        }
    };
    if !stamp_rows(repo, user_id, &rows, &taxonomy, now_rfc3339, report).await {
        // A stamp write failed: leave the job `running` with its cursor so the
        // next tick retries, never advance over the unwritten row.
        return false;
    }

    if rows.len() == RECLASSIFY_BATCH_LIMIT {
        // A full batch: more rows follow the cursor.
        return advance_reclassify_cursor(repo, user_id, &rows, cursor, now_rfc3339, report).await;
    }

    // A short batch means the id walk is exhausted — but the walk can have
    // **skipped** rows: ids are random UUIDs, so a row inserted mid-scan sorts
    // before the cursor and `e.id > ?` will never visit it again. The job may
    // only finish once the verification scan finds nothing that was never
    // classified (a NULL leaf alone means "no pattern matched", not "pending").
    let pending = match repo.unstamped_batch(user_id, RECLASSIFY_BATCH_LIMIT).await {
        Ok(pending) => pending,
        Err(err) => {
            report
                .errors
                .push(format!("digest: unstamped verification failed: {err}"));
            return false;
        }
    };
    if pending.is_empty() {
        if rows.is_empty() {
            finish_job(repo, user_id, JOB_RECLASSIFY, now_rfc3339, report).await;
            report.reclassify_done = true;
            return true;
        }
        // The last walk found rows and nothing is left unstamped: keep the
        // existing shape (a short batch stays `running`, the next tick's empty
        // batch finishes it) but let this tick spend the rest of its budget on
        // dirty days.
        return advance_reclassify_cursor(repo, user_id, &rows, cursor, now_rfc3339, report).await;
    }

    // Stamp the hidden rows in this tick: finishing first would leave them
    // unclassified forever (nothing would ever restamp them).
    if !stamp_rows(repo, user_id, &pending, &taxonomy, now_rfc3339, report).await {
        return false;
    }
    if pending.len() < RECLASSIFY_BATCH_LIMIT {
        // Nothing is unstamped any more: the verification was the real end.
        finish_job(repo, user_id, JOB_RECLASSIFY, now_rfc3339, report).await;
        report.reclassify_done = true;
        return true;
    }
    // A full verification batch: more rows may remain. Rewind the cursor to the
    // start — the next walk must be able to reach ids that sort before the old
    // cursor.
    if let Err(err) = repo
        .upsert_job_status(user_id, JOB_RECLASSIFY, JOB_RUNNING, "", now_rfc3339)
        .await
    {
        report
            .errors
            .push(format!("digest: reclassify rewind failed: {err}"));
    }
    false
}

/// Advances the reclassify cursor past a short/full walk batch. Returns
/// `true` only when the walk is exhausted and may hand its budget to a rebuild.
async fn advance_reclassify_cursor(
    repo: &dyn DigestRepo,
    user_id: &str,
    rows: &[crate::digest::ReclassifyRow],
    cursor: &str,
    now_rfc3339: &str,
    report: &mut DigestTickReport,
) -> bool {
    let last_id = rows.last().map(|row| row.id.as_str()).unwrap_or(cursor);
    if let Err(err) = repo
        .upsert_job_status(user_id, JOB_RECLASSIFY, JOB_RUNNING, last_id, now_rfc3339)
        .await
    {
        report
            .errors
            .push(format!("digest: reclassify advance failed: {err}"));
        return false;
    }
    rows.len() < RECLASSIFY_BATCH_LIMIT
}

/// Classifies and stamps a batch, marking the days each changed row touches
/// dirty. Shared by the id walk and the verification scan (same columns).
///
/// A row whose verdict did not change is skipped **only** when it was already
/// classified (`category_classified != 0`): a never-classified row must still
/// be written through [`DigestRepo::set_category`] so the flag records that it
/// was looked at — a no-match verdict that stays `NULL` is the whole point of
/// the flag. Such a write is not a reclassification: no counter, no dirty mark.
///
/// Returns `false` when a stamp write failed: the caller must leave the job
/// `running` with its cursor so the row is retried, never finish over it.
async fn stamp_rows(
    repo: &dyn DigestRepo,
    user_id: &str,
    rows: &[crate::digest::ReclassifyRow],
    taxonomy: &crate::calendar_color::PaintTaxonomy,
    now_rfc3339: &str,
    report: &mut DigestTickReport,
) -> bool {
    for row in rows {
        let next = classify_event_leaf(
            &row.title,
            &row.google_calendar_id,
            &taxonomy.categories,
            &taxonomy.matchers,
        );
        let changed = next != row.category_id;
        if !changed && row.category_classified != 0 {
            // Unchanged verdict on an already-classified row: nothing to write
            // and nothing to rebuild.
            continue;
        }
        if let Err(err) = repo.set_category(&row.id, next.as_deref()).await {
            report
                .errors
                .push(format!("digest: set_category failed for {}: {err}", row.id));
            return false;
        }
        if !changed {
            // First look at a no-match row: the flag is now set, but the
            // stored verdict did not move, so no day changed.
            continue;
        }
        report.reclassified += 1;
        let dates = dirty_dates_for_span(
            &row.start_time,
            &row.end_time,
            &row.start_time_zone,
            &row.end_time_zone,
        );
        if let Err(err) = repo.mark_dirty(user_id, &dates, now_rfc3339).await {
            report
                .errors
                .push(format!("digest: dirty mark failed after reclassify: {err}"));
        }
    }
    true
}

async fn run_rebuild(
    repo: &dyn DigestRepo,
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
    budget_days: usize,
    today: &str,
    now_unix: i64,
    report: &mut DigestTickReport,
) {
    let dates = match repo.list_dirty(user_id, budget_days).await {
        Ok(dates) => dates,
        Err(err) => {
            report
                .errors
                .push(format!("digest: list dirty failed: {err}"));
            return;
        }
    };
    if dates.is_empty() {
        return;
    }
    let rebuilt = rebuild_dates(
        repo,
        category_repo,
        user_id,
        &dates,
        today,
        now_unix,
        &mut report.errors,
    )
    .await;
    match rebuilt {
        Ok(outcome) => {
            report.rebuilt_days += outcome.rebuilt;
            // Only days that are already finished may be cleared: today's rows
            // were clipped at `now` and must stay dirty for the next tick, and
            // a future date was not rebuilt at all.
            clear_rebuilt_days(repo, user_id, &outcome.clearable, report).await;
        }
        Err(err) => report
            .errors
            .push(format!("digest: rebuild failed: {err}")),
    }
}

/// Clears the days this tick rebuilt, unless a reclassify is in flight.
///
/// The status is re-read **here**, not at the top of the tick: a taxonomy edit
/// can enqueue a restamp while the rebuild is running, and a day rebuilt
/// against the pre-reclassify stamps must not be marked clean — the next tick
/// has to redo it once the scan has written the new stamps. Keeping a day
/// dirty only costs a rebuild, so this errs on the safe side.
async fn clear_rebuilt_days(
    repo: &dyn DigestRepo,
    user_id: &str,
    dates: &[String],
    report: &mut DigestTickReport,
) {
    if dates.is_empty() {
        return;
    }
    let in_flight = repo
        .job_status(user_id, JOB_RECLASSIFY)
        .await
        .ok()
        .flatten()
        .is_some_and(|row| row.is_running());
    if in_flight {
        report.kept_dirty += dates.len();
        return;
    }
    if let Err(err) = repo.clear_dirty(user_id, dates).await {
        report
            .errors
            .push(format!("digest: clear dirty failed: {err}"));
    }
}

/// What one [`rebuild_dates`] pass did.
struct RebuildOutcome {
    /// Days whose `daily_meta` / category / hour / night rows were replaced.
    rebuilt: usize,
    /// Requested days **strictly before today** — the only dirty rows the
    /// caller may clear. Today's rows were clipped at `now` (the day is still
    /// being written) and a future date was not rebuilt at all, so both keep
    /// their dirty row.
    clearable: Vec<String>,
}

/// Rebuilds the requested `dates` for one user, in bounded runs.
///
/// Three rules keep a day honest:
///
/// 1. **A date strictly after `today` is never replaced.** A future date has
///    no finished time: digesting it would clip every segment at `now`, write
///    a zero day, and — with its dirty row cleared — freeze that way forever.
///    Its dirty row is left in place for the tick that runs *on* the day.
/// 2. **Today is rebuilt but never cleared.** The day is provisional, so its
///    tail is recomputed by the next tick (which re-marks it dirty in
///    [`run_digest_tick_user`]). A past day's digest is not clipped — its end
///    is before `now` — so it may be cleared.
/// 3. **The digest set is wider than the persist set.** `digest_days` sees the
///    run's dates expanded by one civil day on each side, because the
///    estimation only runs for the dates it is handed: the night that wakes on
///    `D + 1` owns a pre-midnight half on `D`, and rebuilding `D` alone would
///    silently drop it. Only the requested dates are written.
///
/// Dates are split into **contiguous runs** (a gap of at most
/// `2 * DIGEST_EVENT_WINDOW_PAD_DAYS`, the windows that would otherwise
/// overlap) and each run is loaded with its own `load_events_touching`, capped
/// at [`DIGEST_REBUILD_RUN_MAX_DAYS`] requested days per load — a dirty set of
/// `{2023-01-05, 2026-10-18}` must not load three years of events.
///
/// Days are replaced even when they hold nothing (zeros + no nights) so a
/// deleted event cannot leave stale minutes behind.
async fn rebuild_dates(
    repo: &dyn DigestRepo,
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
    dates: &[String],
    today: &str,
    now_unix: i64,
    errors: &mut Vec<String>,
) -> Result<RebuildOutcome, RepoError> {
    let requested: BTreeSet<String> = dates.iter().cloned().collect();
    let rebuildable: Vec<String> = requested
        .iter()
        .filter(|date| date.as_str() <= today)
        .cloned()
        .collect();
    if rebuildable.is_empty() {
        return Ok(RebuildOutcome {
            rebuilt: 0,
            clearable: Vec::new(),
        });
    }
    let runs = rebuild_runs(&rebuildable);
    let fallback_tz = repo.primary_time_zone(user_id).await?.unwrap_or_default();
    // The sleep category comes from the taxonomy the tick already loads for
    // the reclassify job — never a second source of truth. A taxonomy failure
    // degrades to the `*sleep` sentinel instead of stalling the rebuild.
    let sleep_category = match category_repo.list_by_user_id(user_id).await {
        Ok(categories) => resolve_sleep_category_id(&categories),
        Err(err) => {
            errors.push(format!("digest: sleep category load failed: {err}"));
            None
        }
    };
    // The estimated-night length is the user's own `sleep_duration` rule target
    // (the rule is seeded at 6.5h but editable), read through the digest repo so
    // this module never depends on the rules service. No rule / a failed read
    // falls back to `DEFAULT_SLEEP_SECS`.
    let default_sleep_secs = match repo.sleep_target_secs(user_id).await {
        Ok(Some(secs)) => secs,
        Ok(None) => DEFAULT_SLEEP_SECS,
        Err(err) => {
            errors.push(format!("digest: sleep target load failed: {err}"));
            DEFAULT_SLEEP_SECS
        }
    };
    // Read every run's prior zone **before** the first write: a zone this pass
    // stores must not change what a later run infers from persisted history.
    let mut plan: Vec<(Vec<String>, Option<String>)> = Vec::with_capacity(runs.len());
    for run in runs {
        let prior_zone = repo.latest_zone_before(user_id, &run[0]).await?;
        plan.push((run, prior_zone));
    }

    let mut rebuilt = 0;
    for (run, prior_zone) in &plan {
        let (Some(first), Some(last)) = (run.first(), run.last()) else {
            continue;
        };
        let from = shift_date(first, -DIGEST_EVENT_WINDOW_PAD_DAYS).unwrap_or_else(|| first.clone());
        let to = shift_date(last, DIGEST_EVENT_WINDOW_PAD_DAYS).unwrap_or_else(|| last.clone());
        let events = repo.load_events_touching(user_id, &from, &to).await?;
        let digest_set = expand_dates(run);
        let days = digest_days(
            &events,
            &digest_set,
            now_unix,
            &fallback_tz,
            prior_zone.as_deref(),
            sleep_category.as_deref(),
            default_sleep_secs,
        );
        // The expanded neighbours were digested (their segments are shared with
        // the run) but never persisted: only the requested dates are ours.
        for day in days
            .iter()
            .filter(|day| run.iter().any(|date| date == &day.local_date))
        {
            repo.replace_day(user_id, day).await?;
            rebuilt += 1;
        }
    }

    let clearable = rebuildable
        .iter()
        .filter(|date| date.as_str() < today)
        .cloned()
        .collect();
    Ok(RebuildOutcome { rebuilt, clearable })
}

/// Splits sorted dates into load runs.
///
/// Two dates share a run when the gap between them is at most
/// `2 * DIGEST_EVENT_WINDOW_PAD_DAYS`: their padded windows would overlap
/// anyway, so one load serves both. A longer run is chunked into
/// [`DIGEST_REBUILD_RUN_MAX_DAYS`] consecutive days so a single
/// `load_events_touching` stays bounded.
fn rebuild_runs(dates: &[String]) -> Vec<Vec<String>> {
    let mut runs: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut previous: Option<&String> = None;
    for date in dates {
        let contiguous = previous.is_some_and(|previous| {
            shift_date(previous, 2 * DIGEST_EVENT_WINDOW_PAD_DAYS)
                .is_some_and(|limit| date.as_str() <= limit.as_str())
        });
        if !current.is_empty() && (!contiguous || current.len() >= DIGEST_REBUILD_RUN_MAX_DAYS) {
            runs.push(std::mem::take(&mut current));
        }
        current.push(date.clone());
        previous = Some(date);
    }
    if !current.is_empty() {
        runs.push(current);
    }
    runs
}

/// `dates` expanded by one civil day on each side, sorted and deduplicated.
///
/// A shift that fails (the extremes of `NaiveDate`) is skipped rather than
/// failing the rebuild.
fn expand_dates(dates: &[String]) -> Vec<String> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    for date in dates {
        if let Some(previous) = shift_date(date, -1) {
            set.insert(previous);
        }
        set.insert(date.clone());
        if let Some(next) = shift_date(date, 1) {
            set.insert(next);
        }
    }
    set.into_iter().collect()
}

async fn finish_job(
    repo: &dyn DigestRepo,
    user_id: &str,
    kind: &str,
    now_rfc3339: &str,
    report: &mut DigestTickReport,
) {
    if let Err(err) = repo
        .upsert_job_status(user_id, kind, JOB_DONE, "", now_rfc3339)
        .await
    {
        report
            .errors
            .push(format!("digest: finishing {kind} failed: {err}"));
    }
}
