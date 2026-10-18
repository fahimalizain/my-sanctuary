//! `/api/reports/*` handlers (reports slices 2 and 5).
//!
//! Session-gated like `/api/categories/*`. The orchestration lives in
//! `api_core::digest` / `api_core::record` (pure, unit-tested); this file
//! extracts the session user, wires the D1 repo, and maps the result to JSON.
//!
//! `GET /api/reports/status` is deliberately cheap: it reports the two job
//! flags and the dirty-day count, and *at most* flips the backfill job to
//! `running` when the user has events but no computed days. The rebuild itself
//! belongs to the cron/queue tick — the HTTP call never blocks on it.
//!
//! `GET /api/reports/record?range=7d|30d|90d|ytd` is the descriptive report
//! ("The Record"): the days, the ranking, the productive trend, the hour ×
//! weekday grid, the data quality, and the year table. It starts the backfill
//! the same way `/status` does and never waits for it.

use worker::*;

use api_core::RecordRange;

fn d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<crate::db::D1DigestRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1DigestRepo::new(db))
}

fn record_d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<crate::db::D1RecordRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1RecordRepo::new(db))
}

fn json_error(
    ctx: &RouteContext<Option<api_core::Config>>,
    status: u16,
    message: &str,
) -> Result<Response> {
    let headers = crate::auth::json_headers(crate::auth::frontend_url(ctx))?;
    let response = Response::from_json(&serde_json::json!({ "error": message }))?
        .with_status(status)
        .with_headers(headers);
    Ok(response)
}

/// `GET /api/reports/status` → 200
/// `{"reclassifying":bool,"backfilling":bool,"dirty_days":n}`.
pub async fn status(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return json_error(&ctx, 401, "unauthorized");
    };
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    match api_core::digest_status(&d1(&ctx)?, &user.id, now_unix).await {
        Ok(status) => {
            let response = Response::from_json(&status)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(
                &ctx,
            ))?))
        }
        Err(err) => {
            console_log!("reports: status failed: {err}");
            json_error(&ctx, 500, "failed to load report status")
        }
    }
}

/// `GET /api/reports/record?range=30d` → 200 the assembled `RecordReport`.
///
/// A missing `range` is the 30-day default; anything else is a 400. The report
/// is descriptive only — it never carries a target or a verdict.
pub async fn record(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return json_error(&ctx, 401, "unauthorized");
    };
    let requested = req
        .url()
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "range")
                .map(|(_, value)| value.to_string())
        });
    let range = match requested {
        Some(value) => match RecordRange::parse(&value) {
            Some(range) => range,
            None => return json_error(&ctx, 400, "range must be 7d, 30d, 90d or ytd"),
        },
        None => RecordRange::default(),
    };

    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    match api_core::record_report(&record_d1(&ctx)?, &d1(&ctx)?, &user.id, range, now_unix).await {
        Ok(report) => {
            let response = Response::from_json(&report)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(
                &ctx,
            ))?))
        }
        Err(err) => {
            console_log!("reports: record failed: {err}");
            json_error(&ctx, 500, "failed to load the record")
        }
    }
}
