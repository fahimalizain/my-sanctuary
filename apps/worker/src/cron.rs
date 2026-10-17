//! `#[event(scheduled)]` — the background cron jobs:
//! - **every tick** (`*/2 * * * *` and `*/15 * * * *`): elongate every living
//!   `IN_PROGRESS` task's Google event (`end = max(current, ceil-5min(now
//!   + 5min)` in the event calendar's TZ)) so a running task never looks
//!   finished on the calendar.
//! - **only the 15-minute tick**: the fallback cron (ADR 0001 § Fallback
//!   cron) — publish replicas for dirty / stale / backoff-due calendars
//!   (15-minute `last_success_at` backstop), renew watch channels that would
//!   expire within 24 hours, and notify open browsers for each successful
//!   publish — plus the agenda fill (issue #99, slice 4): materialize every
//!   affected user's occurrence horizon (today … today+6) and backfill their
//!   open tasks with no agenda row. The fill needs no OAuth: a missing OAuth
//!   config still runs it (and only it) on the 15-minute tick. Never on a pure
//!   `*/2` tick.
//!
//! The orchestration lives in `api_core::run_elongate_cron` /
//! `api_core::run_fallback_cron` / `api_core::run_agenda_fill_cron` (pure,
//! unit-tested); this handler is a thin shell that loads the config, wires the
//! D1 repos and `WorkerHttp`, and logs the reports.

use worker::*;

/// The 15-minute trigger expression from `wrangler.toml` — the tick that owns
/// the fallback sync and the agenda fill.
const FIFTEEN_MINUTE_CRON: &str = "*/15 * * * *";

/// A fresh D1 handle for one repo (`D1Database` is not `Clone` in worker
/// 0.8.5, but `Env::d1` returns a new wrapper around the same binding each
/// call). Stops at the first missing binding.
fn d1(env: &Env) -> Result<D1Database> {
    env.d1("DB")
}

/// The agenda-fill repos, one fresh D1 handle each. `Err` when the DB binding
/// is missing — the caller logs and skips.
fn agenda_fill_repos(
    env: &Env,
) -> Result<(
    crate::db::D1RoutineRepo,
    crate::db::D1OccurrenceRepo,
    crate::db::D1AgendaItemRepo,
    crate::db::D1TaskRepo,
    crate::db::D1CalendarRepo,
)> {
    Ok((
        crate::db::D1RoutineRepo::new(d1(env)?),
        crate::db::D1OccurrenceRepo::new(d1(env)?),
        crate::db::D1AgendaItemRepo::new(d1(env)?),
        crate::db::D1TaskRepo::new(d1(env)?),
        crate::db::D1CalendarRepo::new(d1(env)?),
    ))
}

/// Runs the agenda fill and logs its report. OAuth-free by construction, so a
/// deployment without Google credentials still fills the horizon.
async fn run_agenda_fill(env: &Env, now_unix: i64) {
    let Ok((routines, occurrences, agenda, tasks, calendars)) = agenda_fill_repos(env) else {
        console_log!("cron: DB binding missing — skipping agenda fill");
        return;
    };
    let report = api_core::run_agenda_fill_cron(
        &routines,
        &occurrences,
        &agenda,
        &tasks,
        &calendars,
        now_unix,
    )
    .await;
    for error in &report.errors {
        console_log!("cron: {error}");
    }
    console_log!(
        "cron: agenda_fill users={} errors={}",
        report.users,
        report.errors.len()
    );
}

/// Cron entrypoint, invoked by the `*/2 * * * *` and `*/15 * * * *` triggers
/// in `wrangler.toml`.
///
/// worker 0.8's `#[event(scheduled)]` signature: `(ScheduledEvent, Env,
/// ScheduleContext)`, returning `()`. The triggering cron expression is read
/// from `event.cron()` so the 15-minute work (fallback sync + agenda fill)
/// never runs on a pure `*/2` tick. A missing config, OAuth credentials or DB
/// binding only logs and returns — a broken cron tick must never take down the
/// isolate. A missing OAuth config skips everything EXCEPT the agenda fill on
/// the 15-minute tick (the fill touches no Google API).
#[event(scheduled)]
pub async fn scheduled(event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let Some(config) = crate::load_config(&env) else {
        console_log!("cron: config unavailable — skipping cron");
        return;
    };
    // A missing DB binding skips everything — the agenda fill included.
    let Ok(db) = env.d1("DB") else {
        console_log!("cron: DB binding missing — skipping cron");
        return;
    };
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;

    let Some(oauth) = config.oauth.clone() else {
        console_log!("cron: oauth not configured — skipping cron");
        // The agenda fill is the one job that never talks to Google: keep the
        // 15-minute tick filling the horizon even without credentials.
        if event.cron() == FIFTEEN_MINUTE_CRON {
            run_agenda_fill(&env, now_unix).await;
        }
        return;
    };
    // A fresh D1 handle per repo: `D1Database` is not Clone in worker 0.8.5,
    // but `Env::d1` returns a new wrapper around the same binding each call.
    let calendars = crate::db::D1CalendarRepo::new(db);
    let events = match env.d1("DB") {
        Ok(db) => crate::db::D1CalendarEventRepo::new(db),
        Err(err) => {
            console_log!("cron: DB binding missing: {err} — skipping cron");
            return;
        }
    };
    let watches = match env.d1("DB") {
        Ok(db) => crate::db::D1WatchChannelRepo::new(db),
        Err(err) => {
            console_log!("cron: DB binding missing: {err} — skipping cron");
            return;
        }
    };
    let tokens = match env.d1("DB") {
        Ok(db) => crate::db::D1TokenRepo::new(db),
        Err(err) => {
            console_log!("cron: DB binding missing: {err} — skipping cron");
            return;
        }
    };
    let tasks = match env.d1("DB") {
        Ok(db) => crate::db::D1TaskRepo::new(db),
        Err(err) => {
            console_log!("cron: DB binding missing: {err} — skipping cron");
            return;
        }
    };
    let logs = match env.d1("DB") {
        Ok(db) => crate::db::D1TaskLogRepo::new(db),
        Err(err) => {
            console_log!("cron: DB binding missing: {err} — skipping cron");
            return;
        }
    };
    let occurrences = match env.d1("DB") {
        Ok(db) => crate::db::D1OccurrenceRepo::new(db),
        Err(err) => {
            console_log!("cron: DB binding missing: {err} — skipping cron");
            return;
        }
    };
    let operations = match env.d1("DB") {
        Ok(db) => crate::db::D1CalendarEventOperationRepo::new(db),
        Err(err) => {
            console_log!("cron: DB binding missing: {err} — skipping cron");
            return;
        }
    };

    /// Live wall clock for replica lease renewal (never `SystemTime` on wasm32).
    struct WorkerClock;
    impl api_core::Clock for WorkerClock {
        fn now_unix(&self) -> i64 {
            (worker::Date::now().as_millis() / 1000) as i64
        }
    }

    // Every tick (*/2 and */15): grow living IN_PROGRESS events — tasks and
    // in_progress occurrences alike (slice 6), so a running occurrence's
    // one-shot log never looks finished either.
    let elongate = api_core::run_elongate_cron(
        &crate::http::WorkerHttp,
        &calendars,
        &events,
        &operations,
        &logs,
        &tasks,
        &tokens,
        &oauth,
        now_unix,
    )
    .await;
    for error in &elongate.errors {
        console_log!("cron: {error}");
    }
    let elongate_occurrences = api_core::run_elongate_occurrences(
        &crate::http::WorkerHttp,
        &calendars,
        &events,
        &operations,
        &occurrences,
        &tokens,
        &oauth,
        now_unix,
    )
    .await;
    for error in &elongate_occurrences.errors {
        console_log!("cron: {error}");
    }
    console_log!(
        "cron: elongated={} occurrences_elongated={} skipped={} errors={}",
        elongate.elongated,
        elongate_occurrences.occurrences_elongated,
        elongate.skipped + elongate_occurrences.skipped,
        elongate.errors.len() + elongate_occurrences.errors.len()
    );

    // Only the 15-minute tick runs the fallback sync + watch renewal — never
    // on a pure */2 tick.
    if event.cron() == FIFTEEN_MINUTE_CRON {
        let report = api_core::run_fallback_cron_with_clock(
            &crate::http::WorkerHttp,
            &calendars,
            &events,
            &operations,
            &watches,
            &tokens,
            &oauth,
            config.watch_callback_url.as_deref(),
            now_unix,
            &WorkerClock,
        )
        .await;
        for error in &report.errors {
            console_log!("cron: {error}");
        }
        // Notify only after D1 is updated — failures and LeaseBusy are absent
        // from `published`.
        for (user_id, calendar_id) in &report.published {
            crate::user_hub::notify_user(&env, user_id, Some(calendar_id)).await;
        }
        console_log!(
            "cron: synced={} renewed={} published={} errors={}",
            report.synced,
            report.renewed,
            report.published.len(),
            report.errors.len()
        );
        // Structured diagnostics (APP_VERSION stamped; duration from api-core).
        for diagnostic in report.diagnostics {
            crate::sync_log::emit_replica_walk(diagnostic, None);
        }
        for warning in &report.warnings {
            crate::sync_log::emit_operator_warning(warning);
        }

        // The agenda fill (issue #99, slice 4): materialize today … today+6
        // for every affected user, then backfill their open tasks. Runs after
        // the fallback sync and needs no OAuth — the missing-oauth branch
        // above runs it on its own.
        run_agenda_fill(&env, now_unix).await;
    }
}
