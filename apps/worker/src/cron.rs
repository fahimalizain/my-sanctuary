//! `#[event(scheduled)]` — the fallback cron (ADR 0001 § Fallback cron):
//! every 15 minutes, sync every sync-enabled calendar whose last sync is
//! older than 15 minutes (or missing), and renew watch channels that would
//! expire within 24 hours.
//!
//! The orchestration lives in `api_core::run_fallback_cron` (pure,
//! unit-tested); this handler is a thin shell that loads the config, wires
//! the D1 repos and `WorkerHttp`, and logs the report.

use worker::*;

/// Cron entrypoint, invoked by the `*/15 * * * *` trigger in `wrangler.toml`.
///
/// worker 0.8's `#[event(scheduled)]` signature: `(ScheduledEvent, Env,
/// ScheduleContext)`, returning `()`. A missing config, OAuth credentials or
/// DB binding only logs and returns — a broken cron tick must never take
/// down the isolate.
#[event(scheduled)]
pub async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let Some(config) = crate::load_config(&env) else {
        console_log!("cron: config unavailable — skipping fallback cron");
        return;
    };
    let Some(oauth) = config.oauth.clone() else {
        console_log!("cron: oauth not configured — skipping fallback cron");
        return;
    };
    let Ok(db) = env.d1("DB") else {
        console_log!("cron: DB binding missing — skipping fallback cron");
        return;
    };
    // A fresh D1 handle per repo: `D1Database` is not Clone in worker 0.8.5,
    // but `Env::d1` returns a new wrapper around the same binding each call.
    let calendars = crate::db::D1CalendarRepo::new(db);
    let events = match env.d1("DB") {
        Ok(db) => crate::db::D1CalendarEventRepo::new(db),
        Err(err) => {
            console_log!("cron: DB binding missing: {err} — skipping fallback cron");
            return;
        }
    };
    let watches = match env.d1("DB") {
        Ok(db) => crate::db::D1WatchChannelRepo::new(db),
        Err(err) => {
            console_log!("cron: DB binding missing: {err} — skipping fallback cron");
            return;
        }
    };
    let tokens = match env.d1("DB") {
        Ok(db) => crate::db::D1TokenRepo::new(db),
        Err(err) => {
            console_log!("cron: DB binding missing: {err} — skipping fallback cron");
            return;
        }
    };

    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let report = api_core::run_fallback_cron(
        &crate::http::WorkerHttp,
        &calendars,
        &events,
        &watches,
        &tokens,
        &oauth,
        config.watch_callback_url.as_deref(),
        now_unix,
    )
    .await;
    for error in &report.errors {
        console_log!("cron: {error}");
    }
    console_log!(
        "cron: synced={} renewed={} errors={}",
        report.synced,
        report.renewed,
        report.errors.len()
    );
}
