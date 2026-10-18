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

use crate::calendar_color::load_classify_taxonomy;
use crate::categories::classify_event_leaf;
use crate::repo::{RepoError, TaskCategoryRepo};
use crate::time::{civil_date_in_zone, unix_secs_to_rfc3339};

use super::repo::{dirty_dates_for_span, DigestRepo, JobStatus};
use super::sleep::{resolve_sleep_category_id, ALGO_VERSION};
use super::{digest_days, shift_date};

/// Days rebuilt per user per tick.
pub const DIGEST_TICK_BUDGET_DAYS: usize = 7;
/// Events reclassified per tick (`reclassify_batch`).
pub const RECLASSIFY_BATCH_LIMIT: usize = 50;
/// Extra days loaded on each side of a rebuild window: an event that starts
/// before the window can still own a night inside it, and a lexicographic
/// `substr` compare needs slack for zone-shifted instants anyway.
pub const DIGEST_EVENT_WINDOW_PAD_DAYS: i64 = 2;
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
async fn ensure_reclassify_job(
    repo: &dyn DigestRepo,
    user_id: &str,
    now_rfc3339: &str,
) -> Result<(), RepoError> {
    let running = repo
        .job_status(user_id, JOB_RECLASSIFY)
        .await?
        .as_ref()
        .is_some_and(JobStatus::is_running);
    if running {
        // Mid-scan: resetting the cursor would restart the whole history.
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
                now_unix,
                &mut report,
            )
            .await;
        }
    }

    // Decide whether the caller should come back soon. Recomputed from the
    // DB: a partially successful branch may have left work behind.
    let dirty_left = repo
        .count_dirty(user_id)
        .await
        .map(|count| count > 0)
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

#[allow(clippy::too_many_arguments)]
async fn run_backfill(
    repo: &dyn DigestRepo,
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
    cursor: &str,
    budget_days: usize,
    now_unix: i64,
    now_rfc3339: &str,
    report: &mut DigestTickReport,
) {
    let primary_tz = repo
        .primary_time_zone(user_id)
        .await
        .unwrap_or(None)
        .unwrap_or_default();
    let today = civil_date_in_zone(now_unix, &primary_tz);

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

    if start > today || budget_days == 0 {
        finish_job(repo, user_id, JOB_BACKFILL, now_rfc3339, report).await;
        report.backfill_done = true;
        return;
    }

    // Consecutive days from the cursor, never past today: a future day has no
    // finished time to aggregate, and its night must not be guessed early.
    let mut dates: Vec<String> = Vec::new();
    let mut next_date = start.clone();
    while dates.len() < budget_days && next_date <= today {
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
        now_unix,
        &mut report.errors,
    )
    .await;
    match rebuilt {
        Ok(days) => {
            report.rebuilt_days += days;
            if let Err(err) = repo.clear_dirty(user_id, &dates).await {
                report
                    .errors
                    .push(format!("digest: clear dirty failed: {err}"));
            }
            let next = dates
                .last()
                .and_then(|last| shift_date(last, 1))
                .unwrap_or_else(|| today.clone());
            if next > today {
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
    if rows.is_empty() {
        finish_job(repo, user_id, JOB_RECLASSIFY, now_rfc3339, report).await;
        report.reclassify_done = true;
        return true;
    }

    for row in &rows {
        let next = classify_event_leaf(
            &row.title,
            &row.google_calendar_id,
            &taxonomy.categories,
            &taxonomy.matchers,
        );
        if next == row.category_id {
            // Unchanged stamp: nothing to write and nothing to rebuild.
            continue;
        }
        if let Err(err) = repo.set_category(&row.id, next.as_deref()).await {
            report
                .errors
                .push(format!("digest: set_category failed for {}: {err}", row.id));
            return false;
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
    // A short batch means the next one is empty: keep the job `running` (the
    // next tick marks it done) but let this tick spend its remaining budget on
    // dirty days.
    rows.len() < RECLASSIFY_BATCH_LIMIT
}

async fn run_rebuild(
    repo: &dyn DigestRepo,
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
    budget_days: usize,
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
        now_unix,
        &mut report.errors,
    )
    .await;
    match rebuilt {
        Ok(days) => {
            report.rebuilt_days += days;
            if let Err(err) = repo.clear_dirty(user_id, &dates).await {
                report
                    .errors
                    .push(format!("digest: clear dirty failed: {err}"));
            }
        }
        Err(err) => report
            .errors
            .push(format!("digest: rebuild failed: {err}")),
    }
}

/// Loads one padded event window, digests `dates`, and replaces each day.
///
/// Days are replaced even when they hold nothing (zeros + no nights) so a
/// deleted event cannot leave stale minutes behind. Dirty rows are cleared by
/// the caller only after this returns `Ok`.
async fn rebuild_dates(
    repo: &dyn DigestRepo,
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
    dates: &[String],
    now_unix: i64,
    errors: &mut Vec<String>,
) -> Result<usize, RepoError> {
    let (Some(first), Some(last)) = (dates.first(), dates.last()) else {
        return Ok(0);
    };
    let from = shift_date(first, -DIGEST_EVENT_WINDOW_PAD_DAYS).unwrap_or_else(|| first.clone());
    let to = shift_date(last, DIGEST_EVENT_WINDOW_PAD_DAYS).unwrap_or_else(|| last.clone());
    let events = repo.load_events_touching(user_id, &from, &to).await?;
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

    let days = digest_days(
        &events,
        dates,
        now_unix,
        &fallback_tz,
        sleep_category.as_deref(),
    );
    for day in &days {
        repo.replace_day(user_id, day).await?;
    }
    Ok(days.len())
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
