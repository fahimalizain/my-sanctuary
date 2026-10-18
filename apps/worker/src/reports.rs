//! `/api/reports/*` handlers (reports slice 2).
//!
//! Session-gated like `/api/categories/*`. The orchestration lives in
//! `api_core::digest` (pure, unit-tested); this file extracts the session
//! user, wires the D1 repo, and maps the result to JSON.
//!
//! `GET /api/reports/status` is deliberately cheap: it reports the two job
//! flags and the dirty-day count, and *at most* flips the backfill job to
//! `running` when the user has events but no computed days. The rebuild itself
//! belongs to the cron/queue tick — the HTTP call never blocks on it.

use worker::*;

fn d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<crate::db::D1DigestRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1DigestRepo::new(db))
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
